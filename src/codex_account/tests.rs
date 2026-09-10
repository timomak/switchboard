use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;

fn auth(user: &str, revision: &str) -> Vec<u8> {
    let claims = json!({"sub":user,"email":format!("{user}@example.test"),"https://api.openai.com/auth":{"chatgpt_account_id":"shared-org","chatgpt_user_id":user}});
    serde_json::to_vec(&json!({"tokens":{"id_token":format!("header.{}.sig",URL_SAFE_NO_PAD.encode(claims.to_string())),"account_id":"shared-org","access_token":revision,"refresh_token":"fixture"},"unknown_future_field":"retained"})).unwrap()
}
fn fixture() -> (tempfile::TempDir, Paths) {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths {
        root: temp.path().join("profiles-store"),
        home: temp.path().join("codex-home"),
    };
    store::private_dir(&paths.root).unwrap();
    store::private_dir(&paths.home).unwrap();
    cache::atomic_write(&paths.auth(), &auth("a", "initial")).unwrap();
    store::save_profile(&paths, "personal", &auth("a", "initial")).unwrap();
    store::save_profile(&paths, "work", &auth("b", "initial")).unwrap();
    (temp, paths)
}

#[test]
fn initial_cli_setup_can_resume_without_overwriting_existing_logins() {
    let temp = tempfile::tempdir().unwrap();
    let p = Paths::cli_at(temp.path());
    store::private_dir(&p.root).unwrap();
    store::save_profile(&p, "work", &auth("a", "fresh")).unwrap();
    store::initialize_cli_home(&p, "work").unwrap();
    assert_eq!(read_auth(&p.auth()).unwrap(), auth("a", "fresh"));
    assert!(!temp.path().join(".codex").exists());
    assert!(store::initialize_cli_home(&p, "work").is_err());
    crate::cli_session::set_mode_at(&p, true).unwrap();
    assert!(crate::cli_session::separate_at(&p).unwrap());
    cache::atomic_write(&p.auth(), &auth("unmanaged", "keep")).unwrap();
    assert!(store::initialize_cli_home(&p, "work").is_err());
    assert_eq!(read_auth(&p.auth()).unwrap(), auth("unmanaged", "keep"));
}
#[derive(Default)]
struct Fake {
    events: Vec<&'static str>,
    fail_quit: bool,
    fail_verify: bool,
    fail_open: bool,
    on_quit: Option<(std::path::PathBuf, Vec<u8>)>,
}
impl Runtime for Fake {
    async fn quit(&mut self) -> Result<()> {
        self.events.push("quit");
        if self.fail_quit {
            return Err(error("busy"));
        }
        if let Some((p, b)) = &self.on_quit {
            cache::atomic_write(p, b)?;
        }
        Ok(())
    }
    async fn verify(&mut self, home: &std::path::Path, identity: &Identity) -> Result<()> {
        self.events.push("verify");
        assert!(Identity::from_auth(&read_auth(&home.join("auth.json"))?)?.same_account(identity));
        if self.fail_verify {
            Err(error("fixture failure"))
        } else {
            Ok(())
        }
    }
    async fn reopen(&mut self) -> Result<()> {
        self.events.push("open");
        if self.fail_open {
            Err(error("fixture open failure"))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn switch_preserves_all_shared_data_and_latest_outgoing_tokens() {
    let (_temp, p) = fixture();
    for file in [
        "sessions/chat.jsonl",
        "automations/daily/automation.toml",
        "state_5.sqlite",
        "config.toml",
    ] {
        cache::atomic_write(&p.home.join(file), b"# sentinel").unwrap();
    }
    let rotated = auth("a", "rotated-while-quitting");
    let mut runtime = Fake {
        on_quit: Some((p.auth(), rotated.clone())),
        ..Fake::default()
    };
    activate(&p, "work", &mut runtime).await.unwrap();
    assert_eq!(runtime.events, ["quit", "verify", "open"]);
    assert_eq!(read_auth(&p.auth()).unwrap(), auth("b", "initial"));
    assert_eq!(
        read_auth(&p.profile("personal").unwrap().join("auth.json")).unwrap(),
        rotated
    );
    assert!(!p.pending().exists());
    for file in [
        "sessions/chat.jsonl",
        "automations/daily/automation.toml",
        "state_5.sqlite",
        "config.toml",
    ] {
        assert_eq!(std::fs::read(p.home.join(file)).unwrap(), b"# sentinel");
    }
}
#[tokio::test]
async fn failed_verification_restores_exact_bytes_and_reopens() {
    let (_temp, p) = fixture();
    let before = read_auth(&p.auth()).unwrap();
    let mut runtime = Fake {
        fail_verify: true,
        ..Fake::default()
    };
    assert!(activate(&p, "work", &mut runtime).await.is_err());
    assert_eq!(read_auth(&p.auth()).unwrap(), before);
    assert_eq!(runtime.events, ["quit", "verify", "open"]);
    assert!(!p.pending().exists());
}
#[tokio::test]
async fn refused_quit_cannot_write_credentials() {
    let (_temp, p) = fixture();
    let before = read_auth(&p.auth()).unwrap();
    let mut runtime = Fake {
        fail_quit: true,
        ..Fake::default()
    };
    assert!(activate(&p, "work", &mut runtime).await.is_err());
    assert_eq!(runtime.events, ["quit"]);
    assert_eq!(read_auth(&p.auth()).unwrap(), before);
    assert!(!p.pending().exists());
}
#[tokio::test]
async fn invalid_target_and_unsaved_current_fail_before_quitting() {
    let (_temp, p) = fixture();
    let mut runtime = Fake::default();
    cache::atomic_write(
        &p.profile("work").unwrap().join("auth.json"),
        &auth("a", "wrong"),
    )
    .unwrap();
    assert!(activate(&p, "work", &mut runtime).await.is_err());
    cache::atomic_write(
        &p.profile("work").unwrap().join("auth.json"),
        &auth("b", "initial"),
    )
    .unwrap();
    cache::atomic_write(&p.auth(), &auth("unknown", "initial")).unwrap();
    assert!(activate(&p, "work", &mut runtime).await.is_err());
    assert!(runtime.events.is_empty());
}
#[tokio::test]
async fn external_login_change_while_quitting_cannot_overwrite_a_saved_account() {
    let (_temp, p) = fixture();
    let unexpected = auth("unknown", "new-login");
    let mut runtime = Fake {
        on_quit: Some((p.auth(), unexpected.clone())),
        ..Fake::default()
    };
    assert!(activate(&p, "work", &mut runtime).await.is_err());
    assert_eq!(read_auth(&p.auth()).unwrap(), unexpected);
    assert_eq!(
        read_auth(&p.profile("personal").unwrap().join("auth.json")).unwrap(),
        auth("a", "initial")
    );
    assert_eq!(runtime.events, ["quit", "open"]);
}
fn interrupted(p: &Paths) {
    let prev = auth("a", "initial");
    let target = auth("b", "initial");
    let pending = Pending {
        previous_auth: String::from_utf8(prev.clone()).unwrap(),
        previous: Identity::from_auth(&prev).unwrap(),
        target: Identity::from_auth(&target).unwrap(),
    };
    cache::atomic_write(&p.pending(), &serde_json::to_vec(&pending).unwrap()).unwrap();
    cache::atomic_write(&p.auth(), &target).unwrap();
}
#[tokio::test]
async fn interrupted_switch_blocks_new_mutations_and_recovers() {
    let (_temp, p) = fixture();
    interrupted(&p);
    assert!(p.ensure_ready().is_err());
    let mut runtime = Fake::default();
    recover(&p, &mut runtime).await.unwrap();
    assert_eq!(read_auth(&p.auth()).unwrap(), auth("a", "initial"));
    assert!(!p.pending().exists());
    assert_eq!(runtime.events, ["quit", "open"]);
}
#[tokio::test]
async fn recovery_refuses_to_overwrite_an_unrelated_login() {
    let (_temp, p) = fixture();
    interrupted(&p);
    let unknown = auth("unknown", "initial");
    cache::atomic_write(&p.auth(), &unknown).unwrap();
    let mut runtime = Fake::default();
    assert!(recover(&p, &mut runtime).await.is_err());
    assert_eq!(read_auth(&p.auth()).unwrap(), unknown);
    assert!(p.pending().exists());
}
#[tokio::test]
async fn already_active_is_a_noop_and_reopen_failure_keeps_committed_identity() {
    let (_temp, p) = fixture();
    let mut runtime = Fake {
        fail_open: true,
        ..Fake::default()
    };
    activate(&p, "personal", &mut runtime).await.unwrap();
    assert!(runtime.events.is_empty());
    assert!(activate(&p, "work", &mut runtime).await.is_err());
    assert_eq!(read_auth(&p.auth()).unwrap(), auth("b", "initial"));
    assert!(!p.pending().exists());
}
#[test]
fn profile_isolation_validation_and_secret_free_status() {
    let (_temp, p) = fixture();
    let before = read_auth(&p.auth()).unwrap();
    store::save_profile(&p, "third", &auth("c", "secret-marker")).unwrap();
    assert_eq!(read_auth(&p.auth()).unwrap(), before);
    assert!(store::save_profile(&p, "duplicate", &auth("a", "new")).is_err());
    assert!(store::save_profile(&p, "../escape", &auth("d", "new")).is_err());
    let status = store::status(&p).unwrap();
    assert_eq!(status["active_label"], "personal");
    assert_eq!(status["profiles"].as_array().unwrap().len(), 3);
    assert!(!status.to_string().contains("secret-marker"));
    assert!(!status.to_string().contains("refresh_token"));
    // Different users in the same organization must remain different profiles.
    assert!(
        !Identity::from_auth(&auth("a", "x"))
            .unwrap()
            .same_account(&Identity::from_auth(&auth("b", "x")).unwrap())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(p.profile("third").unwrap().join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&p.root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
#[test]
fn keyring_mode_is_rejected_without_changing_configuration() {
    let (_temp, p) = fixture();
    let config = p.home.join("config.toml");
    std::fs::write(&config, "cli_auth_credentials_store = 'keyring'\n").unwrap();
    assert!(preflight(&p, "work").is_err());
    assert_eq!(
        std::fs::read_to_string(config).unwrap(),
        "cli_auth_credentials_store = 'keyring'\n"
    );
}

#[tokio::test]
async fn a_usage_refresh_must_finish_before_switching_reads_outgoing_credentials() {
    let (_temp, p) = fixture();
    let held = crate::cache::acquire_lock_async(
        &crate::openai::creds::lock_path(&p.auth()),
        std::time::Duration::from_secs(1),
    )
    .await
    .unwrap();
    let mut runtime = Fake::default();
    let refresh = async {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(read_auth(&p.auth()).unwrap(), auth("a", "initial"));
        cache::atomic_write(&p.auth(), &auth("a", "refreshed-by-usage")).unwrap();
        drop(held);
    };
    let (result, ()) = tokio::join!(activate(&p, "work", &mut runtime), refresh);
    result.unwrap();
    assert_eq!(
        read_auth(&p.profile("personal").unwrap().join("auth.json")).unwrap(),
        auth("a", "refreshed-by-usage")
    );
    assert_eq!(read_auth(&p.auth()).unwrap(), auth("b", "initial"));
}

#[tokio::test]
async fn corrupt_metadata_keeps_two_valid_choices_and_blocks_unsafe_save() {
    let (_tmp, p) = fixture();
    let bad = p.profile("broken").unwrap();
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("profile.json"), "{private-invalid-content").unwrap();
    let status = store::status(&p).unwrap();
    assert_eq!(status["profiles"].as_array().unwrap().len(), 2);
    assert_eq!(status["profile_errors"][0]["label"], "broken");
    assert!(!status.to_string().contains("private-invalid-content"));
    assert!(store::profile(&p, "personal").is_ok());
    assert!(store::profile(&p, "broken").is_err());
    assert!(store::save_profile(&p, "third", &auth("c", "new")).is_err());
    let mut app = Fake::default();
    activate(&p, "work", &mut app).await.unwrap();
    assert_eq!(store::status(&p).unwrap()["active_label"], "work");
    assert_eq!(
        std::fs::read(bad.join("profile.json")).unwrap(),
        b"{private-invalid-content"
    );
}
