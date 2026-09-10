//! Saved Bedrock connections; subscription credentials and histories stay in place.
use crate::{
    Result, cache,
    claude_desktop::app::{AppControl, DesktopApp},
    codex_account::store::{error, private_dir},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
#[derive(Serialize, Deserialize)]
struct Profile {
    label: String,
    source: PathBuf,
    model: String,
    desktop_id: String,
}
fn root() -> Result<PathBuf> {
    Ok(cache::home_dir()?.join(".claude-acc/claude-connections"))
}
fn library() -> Result<PathBuf> {
    Ok(cache::home_dir()?.join("Library/Application Support/Claude-3p/configLibrary"))
}
fn profile_at(r: &Path) -> Result<Option<Profile>> {
    let bytes = match std::fs::read(r.join("bedrock.json")) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(error(
                "Claude connection is unreadable. Check permissions on bedrock.json; no selection changed.",
            ));
        }
    };
    let p: Profile = serde_json::from_slice(&bytes)
        .map_err(|_| error("Claude connection is invalid. Restore bedrock.json from a trusted backup; no selection changed."))?;
    if p.label.trim().is_empty()
        || !p.source.is_absolute()
        || p.model.is_empty()
        || p.model.chars().any(char::is_control)
        || p.desktop_id != "5084fe94-a74e-4ea1-a22a-0faf080713ac"
    {
        return Err(error(
            "Claude connection is invalid. Restore bedrock.json from a trusted backup; no selection changed.",
        ));
    }
    Ok(Some(p))
}
fn profile() -> Result<Profile> {
    profile_at(&root()?)?.ok_or_else(|| error("No Claude Bedrock connection saved."))
}
fn values(p: &Profile) -> Result<std::collections::BTreeMap<String, String>> {
    let v = crate::codex_provider::fields(&p.source, &["AWS_BEARER_TOKEN_BEDROCK", "AWS_REGION"])?;
    if !v["AWS_REGION"]
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || p.model.is_empty()
    {
        return Err(error("Invalid Bedrock region or model."));
    }
    Ok(v)
}
pub fn register(label: &str, source: &std::path::Path, model: &str) -> Result<()> {
    if label.trim().is_empty() || !source.is_absolute() || model.chars().any(char::is_control) {
        return Err(error(
            "Use a connection name, absolute source path, and valid model ID.",
        ));
    }
    let p = Profile {
        label: label.into(),
        source: source.into(),
        model: model.into(),
        desktop_id: "5084fe94-a74e-4ea1-a22a-0faf080713ac".into(),
    };
    values(&p)?;
    let r = root()?;
    private_dir(&r)?;
    let _lock = cache::acquire_lock(&r.join("operation.lock"), Duration::from_secs(2))?;
    if r.join("bedrock.json").exists() {
        return Err(error("A Claude Bedrock connection is already saved."));
    }
    cache::atomic_write(&r.join("bedrock.json"), &serde_json::to_vec(&p)?)
}

fn read_meta(lib: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(lib.join("_meta.json")) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(error(
            "Cannot read Claude configuration selection. Check _meta.json permissions.",
        )),
    }
}
fn meta_value(bytes: &Option<Vec<u8>>) -> Result<Value> {
    let meta = bytes
        .as_ref()
        .map(|b| serde_json::from_slice::<Value>(b))
        .transpose()
        .map_err(|_| {
            error(
                "Invalid Claude configuration selection. Restore _meta.json from a trusted backup.",
            )
        })?
        .unwrap_or(json!({}));
    if !meta.is_object() {
        return Err(error(
            "Invalid Claude configuration selection. Expected an object in _meta.json.",
        ));
    }
    Ok(meta)
}
fn cli_selected(r: &Path) -> Result<String> {
    match std::fs::read_to_string(r.join("cli-selected")) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(_) => Err(error(
            "Cannot read Claude CLI selection. Check cli-selected permissions and encoding.",
        )),
    }
}
fn status_at(r: &Path, lib: &Path) -> Result<Value> {
    ensure_ready(r)?;
    let Some(p) = profile_at(r)? else {
        return Ok(json!({"state":"absent"}));
    };
    let meta = meta_value(&read_meta(lib)?)?;
    Ok(json!({"state":"ready","label":p.label,"model":p.model,
        "desktop_selected":meta["appliedId"]==p.desktop_id,"cli_selected":cli_selected(r)?==p.label}))
}
pub fn status() -> Value {
    match root().and_then(|r| status_at(&r, &library()?)) {
        Ok(s) => s,
        Err(e) => json!({"state":"invalid","error":e.to_string()}),
    }
}
pub fn configure_cli(c: &mut Command) -> Result<()> {
    let r = root()?;
    let selected = cli_selected(&r)?;
    if selected.is_empty() {
        return Ok(());
    }
    let p = profile()?;
    if selected != p.label {
        return Err(error(
            "Claude CLI selection does not match the saved connection. Select it again explicitly.",
        ));
    }
    configure_cli_profile(c, &p)
}
fn configure_cli_profile(c: &mut Command, p: &Profile) -> Result<()> {
    let v = values(p)?;
    c.env("CLAUDE_CODE_USE_BEDROCK", "1")
        .env("AWS_BEARER_TOKEN_BEDROCK", &v["AWS_BEARER_TOKEN_BEDROCK"])
        .env("AWS_REGION", &v["AWS_REGION"]);
    for k in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
    ] {
        c.env(k, &p.model);
    }
    Ok(())
}
pub fn token() -> Result<()> {
    let p = profile()?;
    println!("{}", values(&p)?["AWS_BEARER_TOKEN_BEDROCK"]);
    Ok(())
}
pub fn select(desktop: bool, subscription: bool) -> Result<()> {
    let r = root()?;
    private_dir(&r)?;
    let _lock = cache::acquire_lock(&r.join("operation.lock"), Duration::from_secs(2))?;
    let p = profile()?;
    if !desktop {
        let runtime = cache::home_dir()?.join(".claude-acc/claude-cli-runtime");
        let _lease = crate::cli_session::exclusive(&runtime)?;
        if !subscription {
            values(&p)?;
        }
        return cache::atomic_write(
            &r.join("cli-selected"),
            if subscription {
                b""
            } else {
                p.label.as_bytes()
            },
        );
    }
    let config = crate::config::Config::load()?;
    let paths = crate::claude_desktop::Paths::resolve(&config.anthropic)?;
    let _account_lock = cache::acquire_lock(
        &paths.account_switch_lock(),
        crate::claude_desktop::ACCOUNT_LOCK_TIMEOUT,
    )?;
    select_desktop(&r, &library()?, &p, subscription, &DesktopApp)
}
fn select_desktop(
    r: &Path,
    lib: &Path,
    p: &Profile,
    subscription: bool,
    app: &impl AppControl,
) -> Result<()> {
    ensure_ready(r)?;
    private_dir(lib)?;
    let backup = r.join("desktop-before.json");
    let current = read_meta(lib)?;
    let mut meta = meta_value(&current)?;
    if subscription {
        if meta["appliedId"] != p.desktop_id {
            return Err(error("Claude is not using the saved Bedrock connection."));
        }
        let previous: Option<Vec<u8>> = serde_json::from_slice(
            &std::fs::read(&backup).map_err(|_| error("Claude selection backup is missing."))?,
        )
        .map_err(|_| error("Claude selection backup is invalid."))?;
        meta_value(&previous)?;
        return change_selection(r, lib, current, previous, app, write_meta);
    }
    if meta["appliedId"] == p.desktop_id {
        return Ok(());
    }
    let v = values(p)?;
    let exe =
        std::env::current_exe().map_err(|_| error("Could not resolve utility executable."))?;
    let helper = r.join("credential-helper");
    let quoted = exe.to_string_lossy().replace('\'', "'\\''");
    cache::atomic_write(
        &helper,
        format!("#!/bin/sh\nexec '{quoted}' cli claude-token\n").as_bytes(),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
    }
    let config = json!({"inferenceProvider":"bedrock","inferenceCredentialKind":"helper-script","inferenceCredentialHelper":helper,"inferenceBedrockRegion":v["AWS_REGION"],"inferenceModels":[{"name":p.model}]});
    cache::atomic_write(
        &lib.join(format!("{}.json", p.desktop_id)),
        &serde_json::to_vec(&config)?,
    )?;
    meta["appliedId"] = json!(p.desktop_id);
    meta.as_object_mut().unwrap().remove("hybridPointer");
    cache::atomic_write(&backup, &serde_json::to_vec(&current)?)?;
    change_selection(
        r,
        lib,
        current,
        Some(serde_json::to_vec(&meta)?),
        app,
        write_meta,
    )
}

const RECOVERY: &str = "An interrupted Claude connection change needs recovery. Run `ai-usagebar cli claude-recover` before selecting again.";
pub(crate) fn ensure_ready(r: &Path) -> Result<()> {
    if r.join("desktop-pending.json").try_exists()? {
        return Err(error(RECOVERY));
    }
    Ok(())
}
pub fn ensure_desktop_ready() -> Result<()> {
    ensure_ready(&root()?)
}
#[derive(Serialize, Deserialize)]
struct Pending {
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}
fn write_meta(lib: &Path, bytes: &Option<Vec<u8>>) -> Result<()> {
    match bytes {
        Some(b) => cache::atomic_write(&lib.join("_meta.json"), b),
        None => match std::fs::remove_file(lib.join("_meta.json")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(error("Could not restore Claude selection.")),
        },
    }
}
// Keep the small journal until both selection write and relaunch succeed.
// Recovery is explicit, and refuses to overwrite edits made since interruption.
fn change_selection(
    r: &Path,
    lib: &Path,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    app: &impl AppControl,
    write: impl FnOnce(&Path, &Option<Vec<u8>>) -> Result<()>,
) -> Result<()> {
    let pending = r.join("desktop-pending.json");
    cache::atomic_write(
        &pending,
        &serde_json::to_vec(&Pending {
            before: before.clone(),
            after: after.clone(),
        })?,
    )?;
    app.quit().map_err(|_| error(RECOVERY))?;
    let result = (|| {
        if read_meta(lib)? != before {
            return Err(error(
                "Claude selection changed while quitting. Nothing was overwritten; preserve the edit and inspect recovery metadata.",
            ));
        }
        write(lib, &after)
    })();
    let relaunched = app.relaunch();
    if result.is_err() || relaunched.is_err() {
        return Err(error(RECOVERY));
    }
    std::fs::remove_file(pending).map_err(|_| error(RECOVERY))
}
pub fn recover() -> Result<()> {
    let r = root()?;
    private_dir(&r)?;
    let _lock = cache::acquire_lock(&r.join("operation.lock"), Duration::from_secs(2))?;
    let config = crate::config::Config::load()?;
    let paths = crate::claude_desktop::Paths::resolve(&config.anthropic)?;
    let _account_lock = cache::acquire_lock(
        &paths.account_switch_lock(),
        crate::claude_desktop::ACCOUNT_LOCK_TIMEOUT,
    )?;
    recover_at(&r, &library()?, &DesktopApp)
}
fn recover_at(r: &Path, lib: &Path, app: &impl AppControl) -> Result<()> {
    let path = r.join("desktop-pending.json");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(error("Cannot read Claude connection recovery metadata.")),
    };
    let p: Pending = serde_json::from_slice(&bytes)
        .map_err(|_| error("Invalid Claude recovery metadata. Preserve it for manual recovery."))?;
    meta_value(&p.before)?;
    meta_value(&p.after)?;
    let check = || -> Result<()> {
        let current = read_meta(lib)?;
        if current != p.before && current != p.after {
            return Err(error(
                "Claude selection changed after interruption. Preserve it and use manual recovery; nothing was overwritten.",
            ));
        }
        Ok(())
    };
    check()?;
    app.quit().map_err(|_| error(RECOVERY))?;
    let result = check().and_then(|()| write_meta(lib, &p.before));
    let relaunched = app.relaunch();
    result?;
    relaunched.map_err(|_| error(RECOVERY))?;
    std::fs::remove_file(path).map_err(|_| error(RECOVERY))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FakeApp {
        quit_fails: bool,
        relaunch_fails: bool,
        calls: std::cell::RefCell<Vec<&'static str>>,
        on_quit: Option<Box<dyn Fn()>>,
    }
    impl FakeApp {
        fn new(quit_fails: bool, relaunch_fails: bool) -> Self {
            Self {
                quit_fails,
                relaunch_fails,
                calls: Default::default(),
                on_quit: None,
            }
        }
    }
    impl AppControl for FakeApp {
        fn quit(&self) -> Result<()> {
            self.calls.borrow_mut().push("quit");
            if let Some(f) = &self.on_quit {
                f();
            }
            if self.quit_fails {
                Err(error("fixture quit failure"))
            } else {
                Ok(())
            }
        }
        fn relaunch(&self) -> Result<()> {
            self.calls.borrow_mut().push("relaunch");
            if self.relaunch_fails {
                Err(error("fixture launch failure"))
            } else {
                Ok(())
            }
        }
        fn archive(&self, _: &Path, _: &Path, _: &[&str]) -> Result<()> {
            panic!("not used")
        }
        fn restore(&self, _: &Path, _: &Path, _: &[&str]) -> Result<()> {
            panic!("not used")
        }
    }
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Profile) {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path().join("connections");
        let lib = tmp.path().join("library");
        private_dir(&r).unwrap();
        private_dir(&lib).unwrap();
        let source = tmp.path().join("fixture.env");
        std::fs::write(
            &source,
            "AWS_REGION=eu-west-1\nAWS_BEARER_TOKEN_BEDROCK=fixture-secret\n",
        )
        .unwrap();
        let p = Profile {
            label: "Work".into(),
            source,
            model: "fixture-model".into(),
            desktop_id: "5084fe94-a74e-4ea1-a22a-0faf080713ac".into(),
        };
        (tmp, r, lib, p)
    }
    #[test]
    fn status_distinguishes_absent_invalid_unreadable_and_ready_without_secrets() {
        let (_tmp, r, lib, p) = fixture();
        assert_eq!(status_at(&r, &lib).unwrap()["state"], "absent");
        std::fs::create_dir(r.join("bedrock.json")).unwrap();
        assert!(
            status_at(&r, &lib)
                .unwrap_err()
                .to_string()
                .contains("unreadable")
        );
        std::fs::remove_dir(r.join("bedrock.json")).unwrap();
        std::fs::write(r.join("bedrock.json"), "private-invalid-content").unwrap();
        let err = status_at(&r, &lib).unwrap_err().to_string();
        assert!(err.contains("invalid"));
        assert!(!err.contains("private-invalid-content"));
        cache::atomic_write(&r.join("bedrock.json"), &serde_json::to_vec(&p).unwrap()).unwrap();
        assert_eq!(status_at(&r, &lib).unwrap()["state"], "ready");
        for bad in ["{", "null", "[]"] {
            std::fs::write(lib.join("_meta.json"), bad).unwrap();
            assert!(status_at(&r, &lib).is_err());
        }
        std::fs::remove_file(lib.join("_meta.json")).unwrap();
        std::fs::create_dir(r.join("cli-selected")).unwrap();
        assert!(status_at(&r, &lib).is_err());
    }
    #[test]
    fn desktop_round_trip_preserves_exact_selection_and_absence() {
        for before in [
            None,
            Some(b"{ \"appliedId\": \"subscription\", \"hybridPointer\": 7 }".to_vec()),
        ] {
            let (_tmp, r, lib, p) = fixture();
            write_meta(&lib, &before).unwrap();
            let app = FakeApp::new(false, false);
            select_desktop(&r, &lib, &p, false, &app).unwrap();
            assert_eq!(
                meta_value(&read_meta(&lib).unwrap()).unwrap()["appliedId"],
                p.desktop_id
            );
            select_desktop(&r, &lib, &p, true, &app).unwrap();
            assert_eq!(read_meta(&lib).unwrap(), before);
            assert!(!r.join("desktop-pending.json").exists());
            assert_eq!(
                *app.calls.borrow(),
                vec!["quit", "relaunch", "quit", "relaunch"]
            );
        }
    }
    #[test]
    fn failures_keep_journal_and_recovery_restores_before() {
        for failure in ["quit", "write", "relaunch"] {
            let (_tmp, r, lib, _) = fixture();
            let before = Some(b"{\"appliedId\":\"subscription\"}".to_vec());
            let after = Some(b"{\"appliedId\":\"bedrock\"}".to_vec());
            write_meta(&lib, &before).unwrap();
            let app = FakeApp::new(failure == "quit", failure == "relaunch");
            let result = change_selection(
                &r,
                &lib,
                before.clone(),
                after.clone(),
                &app,
                |lib, bytes| {
                    if failure == "write" {
                        Err(error("injected write failure"))
                    } else {
                        write_meta(lib, bytes)
                    }
                },
            );
            assert!(result.is_err());
            assert!(ensure_ready(&r).is_err());
            assert_eq!(
                read_meta(&lib).unwrap(),
                if failure == "relaunch" {
                    after
                } else {
                    before.clone()
                }
            );
            if failure != "quit" {
                assert_eq!(*app.calls.borrow(), vec!["quit", "relaunch"]);
            }
            let recovery = FakeApp::new(false, false);
            recover_at(&r, &lib, &recovery).unwrap();
            assert_eq!(read_meta(&lib).unwrap(), before);
            assert!(!r.join("desktop-pending.json").exists());
        }
    }
    #[test]
    fn interrupted_selection_refuses_external_edits_and_retry_survives_relaunch_failure() {
        let (_tmp, r, lib, _) = fixture();
        let after = Some(b"{\"appliedId\":\"bedrock\"}".to_vec());
        assert!(
            change_selection(
                &r,
                &lib,
                None,
                after.clone(),
                &FakeApp::new(false, true),
                write_meta
            )
            .is_err()
        );
        let external = Some(b"{\"appliedId\":\"external\"}".to_vec());
        write_meta(&lib, &external).unwrap();
        let app = FakeApp::new(false, false);
        assert!(recover_at(&r, &lib, &app).is_err());
        assert!(app.calls.borrow().is_empty());
        assert_eq!(read_meta(&lib).unwrap(), external);
        write_meta(&lib, &after).unwrap();
        assert!(recover_at(&r, &lib, &FakeApp::new(false, true)).is_err());
        assert!(r.join("desktop-pending.json").exists());
        recover_at(&r, &lib, &app).unwrap();
        assert_eq!(read_meta(&lib).unwrap(), None);
    }

    #[test]
    fn selection_change_while_quitting_is_never_overwritten() {
        let (_tmp, r, lib, _) = fixture();
        let after = Some(b"{\"appliedId\":\"bedrock\"}".to_vec());
        let external = Some(b"{\"appliedId\":\"external\"}".to_vec());
        let mut app = FakeApp::new(false, false);
        let changed_lib = lib.clone();
        let changed_value = external.clone();
        app.on_quit = Some(Box::new(move || {
            write_meta(&changed_lib, &changed_value).unwrap()
        }));
        assert!(change_selection(&r, &lib, None, after, &app, write_meta).is_err());
        assert_eq!(read_meta(&lib).unwrap(), external);
        assert_eq!(*app.calls.borrow(), vec!["quit", "relaunch"]);
        assert!(ensure_ready(&r).is_err());
    }

    #[test]
    fn bedrock_launch_pins_all_aliases_without_copying_credentials() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.env");
        let original = "AWS_REGION=eu-west-1\nAWS_BEARER_TOKEN_BEDROCK=fixture-secret\n";
        std::fs::write(&source, original).unwrap();
        let p = Profile {
            label: "fixture".into(),
            source: source.clone(),
            model: "organization-model".into(),
            desktop_id: "unused".into(),
        };
        let mut c = Command::new("claude");
        configure_cli_profile(&mut c, &p).unwrap();
        let env: std::collections::BTreeMap<_, _> = c
            .get_envs()
            .map(|(k, v)| (k.to_str().unwrap(), v.unwrap().to_str().unwrap()))
            .collect();
        assert_eq!(env["CLAUDE_CODE_USE_BEDROCK"], "1");
        for key in [
            "ANTHROPIC_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_SMALL_FAST_MODEL",
        ] {
            assert_eq!(env[key], "organization-model");
        }
        assert_eq!(c.get_args().count(), 0);
        assert_eq!(std::fs::read_to_string(source).unwrap(), original);
        assert!(
            !serde_json::to_string(&p)
                .unwrap()
                .contains("fixture-secret")
        );
    }
}
