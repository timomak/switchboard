//! Transactional local connection selection. Never writes authentication,
//! threads, pins, projects, or workflow stores. All credentials stay off argv.
use super::*;
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Snapshot {
    config: Option<String>,
    env: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Active {
    home: PathBuf,
    label: String,
    model: String,
    baseline: Snapshot,
    applied: Snapshot,
}
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    home: PathBuf,
    before: Snapshot,
    after: Snapshot,
    previous: Option<Active>,
    next: Option<Active>,
}
fn runtime(root: &Path) -> PathBuf {
    root.join("runtime")
}
fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(error("Could not read connection configuration.")),
    }
}
fn snapshot(home: &Path) -> Result<Snapshot> {
    for name in ["config.toml", ".env"] {
        if std::fs::symlink_metadata(home.join(name)).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(error(
                "Connection selection requires regular config and .env files; symlink targets were not changed.",
            ));
        }
    }
    Ok(Snapshot {
        config: read_optional(&home.join("config.toml"))?,
        env: read_optional(&home.join(".env"))?,
    })
}
fn active(root: &Path) -> Result<Option<Active>> {
    read_optional(&runtime(root).join("active.json"))?
        .map(|s| {
            serde_json::from_str(&s).map_err(|_| error("Connection recovery metadata is invalid."))
        })
        .transpose()
}
fn persist<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    cache::atomic_write(
        path,
        &serde_json::to_vec(value)
            .map_err(|_| error("Could not encode connection recovery data."))?,
    )
}
fn write_optional(path: &Path, value: &Option<String>) -> Result<()> {
    match value {
        Some(s) => cache::atomic_write(path, s.as_bytes()),
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(error("Could not restore connection configuration.")),
        },
    }
}
fn write_snapshot(home: &Path, state: &Snapshot) -> Result<()> {
    write_optional(&home.join("config.toml"), &state.config)?;
    write_optional(&home.join(".env"), &state.env)
}
fn write_active(root: &Path, state: &Option<Active>) -> Result<()> {
    match state {
        Some(s) => persist(&runtime(root).join("active.json"), s),
        None => write_optional(&runtime(root).join("active.json"), &None),
    }
}
fn pending(root: &Path) -> PathBuf {
    runtime(root).join("pending.json")
}
// Preserve unrelated Codex settings while protecting fields owned by the connection.
fn reconciled_baseline(a: &Active, home: &Path, current: &Snapshot) -> Result<Snapshot> {
    if a.home != home || a.applied.env != current.env {
        return Err(error(
            "Connection home or environment changed outside the switcher.",
        ));
    }
    let parse = |text: &Option<String>| -> Result<toml::Value> {
        toml::from_str(text.as_deref().unwrap_or(""))
            .map_err(|_| error("Codex configuration is invalid."))
    };
    let applied = parse(&a.applied.config)?;
    let live = parse(&current.config)?;
    let owned = [
        "model_provider",
        "model",
        "model_providers",
        "forced_login_method",
        "service_tier",
        "model_reasoning_effort",
        "model_reasoning_summary",
        "model_verbosity",
    ];
    for key in owned {
        if applied.get(key) != live.get(key) {
            return Err(error(&format!(
                "Codex config.toml field `{key}` changed outside the switcher. No settings were overwritten. Restore that field to the selected connection setting, or preserve your edits and use manual recovery; values are omitted for privacy."
            )));
        }
    }
    if applied == live {
        return Ok(a.baseline.clone());
    }
    let mut baseline: toml_edit::DocumentMut =
        a.baseline
            .config
            .as_deref()
            .unwrap_or("")
            .parse()
            .map_err(|_| error("Saved configuration is invalid."))?;
    let live_doc: toml_edit::DocumentMut = current
        .config
        .as_deref()
        .unwrap_or("")
        .parse()
        .map_err(|_| error("Codex configuration is invalid."))?;
    let keys: std::collections::BTreeSet<_> = applied
        .as_table()
        .into_iter()
        .flat_map(|t| t.keys())
        .chain(live.as_table().into_iter().flat_map(|t| t.keys()))
        .collect();
    for key in keys {
        if !owned.contains(&key.as_str()) && applied.get(key) != live.get(key) {
            if let Some(item) = live_doc.get(key) {
                baseline[key] = item.clone();
            } else {
                baseline.remove(key);
            }
        }
    }
    Ok(Snapshot {
        config: Some(baseline.to_string()),
        env: a.baseline.env.clone(),
    })
}
pub(super) fn status(root: &Path, home: &Path) -> Result<Value> {
    let current = active(root)?;
    if let Some(a) = &current {
        reconciled_baseline(a, home, &snapshot(home)?)?;
    }
    Ok(
        json!({"active_label":current.as_ref().map(|a|&a.label),"active_model":current.as_ref().map(|a|&a.model),"recovery_required":pending(root).exists()}),
    )
}
fn plan(profile: &Profile, home: &Path, baseline: &Snapshot) -> Result<Snapshot> {
    let model = profile
        .model
        .as_ref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            error("Enter a model ID or Azure deployment name before using this connection.")
        })?;
    let values = validate_source(profile)?;
    let bindings = profile.bindings();
    let report = verify(profile, home)?;
    let mut proposal: toml_edit::DocumentMut = report["proposed_config"]
        .as_str()
        .ok_or_else(|| error("Missing provider configuration."))?
        .parse()
        .map_err(|_| error("Provider configuration is invalid."))?;
    let mut config: toml_edit::DocumentMut = baseline
        .config
        .as_deref()
        .unwrap_or("")
        .parse()
        .map_err(|_| error("Current Codex configuration is invalid."))?;
    let provider = proposal["model_provider"]
        .as_str()
        .ok_or_else(|| error("Missing provider ID."))?
        .to_owned();
    config["model_provider"] = toml_edit::value(provider.clone());
    config["model"] = toml_edit::value(model.clone());
    for key in [
        "forced_login_method",
        "service_tier",
        "model_reasoning_effort",
        "model_reasoning_summary",
        "model_verbosity",
    ] {
        config.remove(key);
    }
    if config.get("model_providers").is_some_and(|i| !i.is_table()) {
        return Err(error(
            "The model_providers configuration must be a TOML table.",
        ));
    }
    if config.get("model_providers").is_none() {
        config["model_providers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    config["model_providers"][&provider] = proposal["model_providers"]
        .as_table_mut()
        .and_then(|t| t.remove(&provider))
        .ok_or_else(|| error("Missing provider adapter."))?;
    let mut environment = BTreeMap::new();
    match profile.provider {
        Provider::Bedrock => {
            environment.insert(
                "AWS_BEARER_TOKEN_BEDROCK".to_string(),
                values[&bindings.credential_key].clone(),
            );
            environment.insert(
                "AWS_REGION".to_string(),
                values[bindings
                    .region_key
                    .as_deref()
                    .ok_or_else(|| error("Missing AWS region mapping."))?]
                .clone(),
            );
        }
        _ => {
            environment.insert(
                profile.runtime_key()?,
                values[&bindings.credential_key].clone(),
            );
        }
    }
    let mut env = String::new();
    for line in baseline.env.as_deref().unwrap_or("").lines() {
        let candidate = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if let Some((key, value)) = candidate.split_once('=')
            && environment.contains_key(key.trim())
        {
            let value = value.trim();
            if value.starts_with(['\'', '"']) && !value[1..].contains(value.as_bytes()[0] as char) {
                return Err(error(
                    "A replaced environment field spans multiple lines. Review .env before switching.",
                ));
            }
            continue;
        }
        env.push_str(line);
        env.push('\n');
    }
    env.push_str("# Connection selected by AI Usage Bar; restored on return to subscription.\n");
    for (key, value) in environment {
        env.push_str(&key);
        env.push('=');
        env.push_str(
            &serde_json::to_string(&value)
                .map_err(|_| error("Could not encode environment field."))?,
        );
        env.push('\n');
    }
    Ok(Snapshot {
        config: Some(config.to_string()),
        env: Some(env),
    })
}
// Retain only referenced managed definitions and their exact private credential lines.
// Legacy adapters keep the conservative admission guard; never invent their identity.
fn retain_referenced(home: &Path, before: &Snapshot, target: &mut Snapshot) -> Result<()> {
    let old: toml_edit::DocumentMut = before
        .config
        .as_deref()
        .unwrap_or("")
        .parse()
        .map_err(|_| error("Codex configuration is invalid."))?;
    let mut new: toml_edit::DocumentMut = target
        .config
        .as_deref()
        .unwrap_or("")
        .parse()
        .map_err(|_| error("Codex configuration is invalid."))?;
    let mut changed = false;
    for id in super::compatibility::references(home)? {
        let Some(key) = managed_key(&id) else {
            continue;
        };
        let Some(definition) = old.get("model_providers").and_then(|v| v.get(&id)) else {
            continue;
        };
        if definition.get("env_key").and_then(|v| v.as_str()) != Some(&key) {
            return Err(error(
                "Saved connection credential binding changed; preserve config.toml and use manual recovery.",
            ));
        }
        let line = super::compatibility::credential_line(before.env.as_deref(), &key)?;
        let existing = new.get("model_providers").and_then(|v| v.get(&id));
        if existing.is_none() {
            if new.get("model_providers").is_none() {
                new["model_providers"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            if !new["model_providers"].is_table() {
                return Err(error(
                    "The model_providers configuration must be a TOML table.",
                ));
            }
            new["model_providers"][&id] = definition.clone();
            changed = true;
        }
        // Do not overwrite a proposed key: the compatibility check must detect rebinding.
        if super::compatibility::optional_credential_line(target.env.as_deref(), &key)?.is_none() {
            let env = target.env.get_or_insert_default();
            if !env.is_empty() && !env.ends_with('\n') {
                env.push('\n');
            }
            env.push_str(line);
            env.push('\n');
        }
    }
    if changed {
        target.config = Some(new.to_string());
    }
    Ok(())
}
fn prepare(root: &Path, home: &Path, profile: Option<&Profile>) -> Result<Pending> {
    if pending(root).exists() {
        return Err(error("Recover the interrupted connection change first."));
    }
    let before = snapshot(home)?;
    let previous = active(root)?;
    let mut baseline = match &previous {
        Some(a) => reconciled_baseline(a, home, &before)?,
        None => before.clone(),
    };
    retain_referenced(home, &before, &mut baseline)?;
    let (after, next) = if let Some(profile) = profile {
        let after = plan(profile, home, &baseline)?;
        let next = Active {
            home: home.to_path_buf(),
            label: profile.label.clone(),
            model: profile.model.clone().unwrap_or_default(),
            baseline,
            applied: after.clone(),
        };
        (after, Some(next))
    } else {
        if previous.is_none() {
            return Err(error("Codex is already using its original configuration."));
        }
        (baseline, None)
    };
    ensure_task_compatibility(home, &before, &after)?;
    Ok(Pending {
        home: home.to_path_buf(),
        before,
        after,
        previous,
        next,
    })
}
fn ensure_task_compatibility(home: &Path, before: &Snapshot, after: &Snapshot) -> Result<()> {
    super::compatibility::ensure_transition(
        home,
        before.config.as_deref(),
        after.config.as_deref(),
        before.env.as_deref(),
        after.env.as_deref(),
    )
}
fn commit(root: &Path, p: &Pending) -> Result<()> {
    if snapshot(&p.home)? != p.before {
        return Err(error(
            "Codex configuration changed while quitting. Nothing was overwritten.",
        ));
    }
    // Reconcile references created during shutdown before journaling the exact write.
    let mut p = p.clone();
    retain_referenced(&p.home, &p.before, &mut p.after)?;
    if let Some(next) = &mut p.next {
        retain_referenced(&p.home, &p.before, &mut next.baseline)?;
        next.applied = p.after.clone();
    }
    ensure_task_compatibility(&p.home, &p.before, &p.after)?;
    private_dir(&runtime(root))?;
    // Retain a private immutable recovery record for each attempted change.
    let backup = runtime(root).join(format!(
        "backup-{}.json",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    persist(&backup, &p)?;
    persist(&pending(root), &p)?;
    write_snapshot(&p.home, &p.after)?;
    write_active(root, &p.next)?;
    std::fs::remove_file(pending(root))
        .map_err(|_| error("Connection applied; recovery journal could not be cleared."))
}
fn recover(root: &Path, home: &Path) -> Result<()> {
    recover_with(root, home, write_snapshot)
}
fn recover_with(
    root: &Path,
    home: &Path,
    write: impl FnOnce(&Path, &Snapshot) -> Result<()>,
) -> Result<()> {
    let data = read_optional(&pending(root))?
        .ok_or_else(|| error("There is no interrupted connection change."))?;
    let p: Pending =
        serde_json::from_str(&data).map_err(|_| error("Connection recovery record is invalid."))?;
    if p.home != home {
        return Err(error("Recovery belongs to a different Codex home."));
    }
    let actual = snapshot(home)?;
    if (actual.config != p.before.config && actual.config != p.after.config)
        || (actual.env != p.before.env && actual.env != p.after.env)
    {
        return Err(error(
            "Configuration changed after the interrupted operation. Preserve it and use manual recovery.",
        ));
    }
    let mut restored = p.before.clone();
    retain_referenced(home, &actual, &mut restored)?;
    ensure_task_compatibility(home, &actual, &restored)?;
    let mut previous = p.previous.clone();
    if let Some(previous) = &mut previous {
        retain_referenced(home, &actual, &mut previous.baseline)?;
        previous.applied = restored.clone();
    }
    // Retention can make the rollback differ from the original before image.
    // Journal that exact target before writing, so another interruption is recoverable.
    persist(
        &pending(root),
        &Pending {
            home: home.to_path_buf(),
            before: restored.clone(),
            after: actual,
            previous: previous.clone(),
            next: p.next,
        },
    )?;
    write(home, &restored)?;
    write_active(root, &previous)?;
    std::fs::remove_file(pending(root))
        .map_err(|_| error("Recovered configuration; journal could not be cleared."))
}
async fn stop_daemon(home: &Path) -> Result<()> {
    if !home
        .join("app-server-control/app-server-control.sock")
        .exists()
    {
        return Ok(());
    }
    let mut c = tokio::process::Command::new(crate::codex_account::rpc::executable()?);
    c.args(["app-server", "daemon", "stop"])
        .env("CODEX_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(20), c.status())
        .await
        .map_err(|_| error("Codex background service did not stop; configuration is unchanged."))?
        .map_err(|_| error("Could not stop the Codex background service."))?;
    if !status.success() {
        return Err(error(
            "Close remaining Codex sessions before changing providers.",
        ));
    }
    Ok(())
}
struct ClosedDesktop;
impl crate::codex_account::switch::Runtime for ClosedDesktop {
    async fn quit(&mut self) -> Result<()> {
        Ok(())
    }
    async fn reopen(&mut self) -> Result<()> {
        Ok(())
    }
    async fn verify(
        &mut self,
        home: &Path,
        identity: &crate::codex_account::store::Identity,
    ) -> Result<()> {
        crate::codex_account::rpc::verify(home, identity).await
    }
}
pub(super) async fn run(action: &Action) -> Result<()> {
    let root = root()?;
    let paths = crate::codex_account::store::Paths::resolve()?;
    let _lock = paths.lock()?;
    let _sessions = crate::cli_session::exclusive(&paths.root)?;
    paths.ensure_ready()?;
    let (yes, is_recovery) = match action {
        Action::Use { yes, .. } | Action::Subscription { yes, .. } => (*yes, false),
        Action::Recover { yes } => (*yes, true),
        _ => return Err(error("Invalid connection action.")),
    };
    if !yes {
        return Err(error(
            "Finish Codex tasks and CLI sessions, then confirm the restart with --yes.",
        ));
    }
    let account = match action {
        Action::Subscription { account, .. } => account.as_deref(),
        _ => None,
    };
    if let Some(label) = account {
        crate::codex_account::switch::preflight(&paths, label)?;
    }
    let change = if is_recovery {
        None
    } else {
        let profile = if let Action::Use { label, model, .. } = action {
            let mut p = load(&root, label)?;
            if model.is_some() {
                p.model = model.clone();
            }
            Some(p)
        } else {
            None
        };
        Some(prepare(&root, &paths.home, profile.as_ref())?)
    };
    crate::codex_account::desktop::quit().await?;
    if let Err(e) = stop_daemon(&paths.home).await {
        let _ = crate::codex_account::desktop::reopen().await;
        return Err(e);
    }
    let result = if let Some(change) = &change {
        commit(&root, change)
    } else {
        recover(&root, &paths.home)
    };
    if result.is_err()
        && !is_recovery
        && pending(&root).exists()
        && recover(&root, &paths.home).is_err()
    {
        return Err(error(
            "Connection change was interrupted. Use Recover connection before reopening Codex.",
        ));
    }
    if is_recovery && result.is_err() && pending(&root).exists() {
        return Err(error(
            "Connection recovery is unresolved. Codex remains closed; preserve outside edits and retry Recover connection after resolving the reported configuration conflict.",
        ));
    }
    if result.is_ok()
        && let Some(label) = account
    {
        let switched =
            crate::codex_account::switch::activate(&paths, label, &mut ClosedDesktop).await;
        if switched.is_err() {
            if !paths.pending().exists() {
                let _ = crate::codex_account::desktop::reopen().await;
            }
            return switched;
        }
    }
    let reopened = crate::codex_account::desktop::reopen().await;
    result?;
    reopened?;
    println!(
        "Codex connection updated. Local history, pins, and workflow records were not changed."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn managed_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Profile) {
        let (tmp, home, root, profile) = fixture();
        stage(&root, &profile).unwrap();
        let profile = load(&root, &profile.label).unwrap();
        (tmp, home, root, profile)
    }

    #[test]
    fn managed_adapters_return_to_subscription_with_saved_tasks_and_isolated_keys() {
        for adapter in [Provider::Compatible, Provider::Azure] {
            let (_tmp, home, root, mut profile) = managed_fixture();
            profile.provider = adapter;
            if matches!(adapter, Provider::Azure) {
                std::fs::write(
                    &profile.source,
                    "TEAM_TOKEN=fixture-secret\nTEAM_ENDPOINT=https://fixture.openai.azure.com\n",
                )
                .unwrap();
            }
            let id = profile.provider_id().unwrap();
            let key = profile.runtime_key().unwrap();
            commit(&root, &prepare(&root, &home, Some(&profile)).unwrap()).unwrap();
            task_fixture(&home, &id, true, true);
            let rollout = home.join("sessions/2026/09/09/fixture.jsonl");
            let body = std::fs::read(&rollout).unwrap();
            let db = std::fs::read(home.join("state_5.sqlite")).unwrap();
            let definition = read_config(&home).unwrap()["model_providers"][&id].clone();
            commit(&root, &prepare(&root, &home, None).unwrap()).unwrap();
            assert_eq!(
                read_config(&home).unwrap()["model_provider"].as_str(),
                Some("openai")
            );
            assert_eq!(
                read_config(&home).unwrap()["model_providers"][&id],
                definition
            );
            assert!(status(&root, &home).unwrap()["active_label"].is_null());
            let retained_env = snapshot(&home).unwrap().env.unwrap();
            assert!(retained_env.contains(&format!("{key}=\"fixture-secret\"")));
            assert!(!retained_env.contains("TEAM_TOKEN="));
            reopen_fixture(&home);

            let mut second = profile.clone();
            second.label = "second".into();
            second.source = home.join("second.env");
            std::fs::write(&second.source, if matches!(adapter, Provider::Azure) {
                "TEAM_TOKEN=second-fixture-secret\nTEAM_ENDPOINT=https://second.openai.azure.com\n"
            } else {
                "TEAM_TOKEN=second-fixture-secret\nTEAM_ENDPOINT=https://second.example/v1\n"
            }).unwrap();
            stage(&root, &second).unwrap();
            let second = load(&root, "second").unwrap();
            assert_ne!(id, second.provider_id().unwrap());
            commit(&root, &prepare(&root, &home, Some(&second)).unwrap()).unwrap();
            let archived = home.join("archived_sessions");
            std::fs::create_dir(&archived).unwrap();
            std::fs::write(archived.join("second.jsonl"),
                format!("{}\n", json!({"type":"session_meta","payload":{"model_provider":second.provider_id().unwrap()}}))).unwrap();
            commit(&root, &prepare(&root, &home, None).unwrap()).unwrap();
            let env = snapshot(&home).unwrap().env.unwrap();
            assert!(env.contains(&key));
            assert!(env.contains(&second.runtime_key().unwrap()));
            assert_eq!(
                read_config(&home).unwrap()["model_providers"][&id],
                definition
            );
            assert_eq!(std::fs::read(rollout).unwrap(), body);
            assert_eq!(std::fs::read(home.join("state_5.sqlite")).unwrap(), db);
        }
    }

    #[test]
    fn managed_references_created_during_shutdown_are_retained_before_journaling() {
        for (db, rollout) in [(true, false), (false, true)] {
            let (_tmp, home, root, profile) = managed_fixture();
            commit(&root, &prepare(&root, &home, Some(&profile)).unwrap()).unwrap();
            let back = prepare(&root, &home, None).unwrap();
            task_fixture(&home, &profile.provider_id().unwrap(), db, rollout);
            if rollout {
                std::fs::rename(home.join("sessions"), home.join("archived_sessions")).unwrap();
            }
            commit(&root, &back).unwrap();
            assert_eq!(
                read_config(&home).unwrap()["model_provider"].as_str(),
                Some("openai")
            );
            assert!(
                read_config(&home).unwrap()["model_providers"]
                    .get(profile.provider_id().unwrap())
                    .is_some()
            );
            assert!(!pending(&root).exists());
        }
    }

    #[test]
    fn managed_identity_survives_label_change_but_rebinding_and_missing_keys_are_refused() {
        let (_tmp, home, root, mut profile) = managed_fixture();
        commit(&root, &prepare(&root, &home, Some(&profile)).unwrap()).unwrap();
        task_fixture(&home, &profile.provider_id().unwrap(), true, true);
        profile.label = "renamed".into();
        commit(&root, &prepare(&root, &home, Some(&profile)).unwrap()).unwrap();
        let before = snapshot(&home).unwrap();
        for source in [
            "TEAM_TOKEN=changed-account\nTEAM_ENDPOINT=https://fixture.example/v1\n",
            "TEAM_TOKEN=fixture-secret\nTEAM_ENDPOINT=https://changed.example/v1\n",
        ] {
            std::fs::write(&profile.source, source).unwrap();
            assert!(prepare(&root, &home, Some(&profile)).is_err());
            assert!(snapshot(&home).unwrap() == before);
        }
        std::fs::remove_file(&profile.source).unwrap();
        // Subscription return uses the retained runtime credential, not the missing source.
        commit(&root, &prepare(&root, &home, None).unwrap()).unwrap();
        let before = snapshot(&home).unwrap();
        let mut missing = before.clone();
        missing.env = None;
        assert!(ensure_task_compatibility(&home, &before, &missing).is_err());
        assert!(retain_referenced(&home, &missing, &mut before.clone()).is_err());
    }

    #[test]
    fn managed_recovery_retains_saved_dependencies_and_is_repeatable_without_writes() {
        let (_tmp, home, root, profile) = managed_fixture();
        let change = prepare(&root, &home, Some(&profile)).unwrap();
        commit(&root, &change).unwrap();
        task_fixture(&home, &profile.provider_id().unwrap(), true, true);
        persist(&pending(&root), &change).unwrap();
        recover(&root, &home).unwrap();
        assert_eq!(
            read_config(&home).unwrap()["model_provider"].as_str(),
            Some("openai")
        );
        reopen_fixture(&home);
        let before = snapshot(&home).unwrap();
        assert!(recover(&root, &home).is_err());
        assert!(snapshot(&home).unwrap() == before);
    }

    #[test]
    fn managed_recovery_interrupted_after_config_write_can_retry() {
        let (_tmp, home, root, profile) = managed_fixture();
        let change = prepare(&root, &home, Some(&profile)).unwrap();
        commit(&root, &change).unwrap();
        task_fixture(&home, &profile.provider_id().unwrap(), true, true);
        persist(&pending(&root), &change).unwrap();
        assert!(
            recover_with(&root, &home, |home, state| {
                write_optional(&home.join("config.toml"), &state.config)?;
                Err(error("fixture interrupted write"))
            })
            .is_err()
        );
        assert!(pending(&root).exists());
        recover(&root, &home).unwrap();
        reopen_fixture(&home);
        assert_eq!(
            read_config(&home).unwrap()["model_provider"].as_str(),
            Some("openai")
        );
        assert!(!pending(&root).exists());
    }

    #[test]
    fn bedrock_saved_tasks_still_guard_the_shared_aws_environment() {
        let (_tmp, home, root, mut profile) = fixture();
        profile.provider = Provider::Bedrock;
        profile.bindings = None;
        std::fs::write(
            &profile.source,
            "AWS_BEARER_TOKEN_BEDROCK=synthetic-aws\nAWS_REGION=us-east-1\n",
        )
        .unwrap();
        stage(&root, &profile).unwrap();
        let profile = load(&root, &profile.label).unwrap();
        assert!(profile.routing_id.is_none());
        commit(&root, &prepare(&root, &home, Some(&profile)).unwrap()).unwrap();
        task_fixture(&home, "amazon-bedrock", true, true);
        let before = snapshot(&home).unwrap();
        assert!(prepare(&root, &home, None).is_err());
        assert!(snapshot(&home).unwrap() == before);
    }
    #[test]
    fn plugin_updates_preserve_selection_and_survive_return() {
        let (_tmp, home, root, profile) = fixture();
        let change = prepare(&root, &home, Some(&profile)).unwrap();
        commit(&root, &change).unwrap();
        let config_path = home.join("config.toml");
        let updated = std::fs::read_to_string(&config_path).unwrap()
            + "\n[plugins.example]\nenabled = true\n";
        std::fs::write(&config_path, updated).unwrap();
        assert_eq!(status(&root, &home).unwrap()["active_label"], "team");
        let restore = prepare(&root, &home, None).unwrap();
        commit(&root, &restore).unwrap();
        let restored: toml::Value =
            toml::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
        assert_eq!(restored["model_provider"].as_str(), Some("openai"));
        assert_eq!(
            restored["plugins"]["example"]["enabled"].as_bool(),
            Some(true)
        );
        assert!(!home.join(".env").exists());
    }

    #[test]
    fn owned_field_conflicts_name_only_the_field_and_preserve_guard() {
        let home = PathBuf::from("/fixture");
        for key in [
            "model",
            "model_provider",
            "model_providers",
            "forced_login_method",
            "service_tier",
            "model_reasoning_effort",
            "model_reasoning_summary",
            "model_verbosity",
        ] {
            let baseline = Snapshot {
                config: Some(String::new()),
                env: None,
            };
            let applied = Snapshot {
                config: Some(format!("{key}='original'")),
                env: None,
            };
            let a = Active {
                home: home.clone(),
                label: "fixture".into(),
                model: "fixture".into(),
                baseline,
                applied,
            };
            let live = Snapshot {
                config: Some(format!("{key}='private-marker'")),
                env: None,
            };
            let message = reconciled_baseline(&a, &home, &live)
                .err()
                .unwrap()
                .to_string();
            assert!(message.contains(&format!("`{key}`")));
            assert!(!message.contains("private-marker"));
            assert!(!message.contains("original"));
        }
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Profile) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("codex");
        let root = tmp.path().join("connections");
        std::fs::create_dir(&home).unwrap();
        private_dir(&root).unwrap();
        std::fs::write(home.join("config.toml"),"# retain comments\nmodel_provider='openai'\nmodel='original-model'\nforced_login_method='chatgpt'\n[features]\ncustom=true\n").unwrap();
        for f in ["auth.json", "state.sqlite", ".codex-global-state.json"] {
            std::fs::write(home.join(f), "untouched").unwrap();
        }
        std::fs::create_dir(home.join("automations")).unwrap();
        std::fs::write(home.join("automations/task.toml"), "unchanged-workflow").unwrap();
        let source = tmp.path().join("source.env");
        std::fs::write(
            &source,
            "TEAM_TOKEN=fixture-secret\nTEAM_ENDPOINT=https://fixture.example/v1\n",
        )
        .unwrap();
        let profile = Profile {
            routing_id: None,
            label: "team".into(),
            provider: Provider::Compatible,
            source,
            model: Some("organization-model".into()),
            bindings: Some(Bindings {
                credential_key: "TEAM_TOKEN".into(),
                endpoint_key: Some("TEAM_ENDPOINT".into()),
                region_key: None,
            }),
        };
        (tmp, home, root, profile)
    }
    fn task_fixture(home: &Path, provider: &str, db: bool, rollout: bool) {
        if db {
            let conn = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
            conn.execute_batch("CREATE TABLE IF NOT EXISTS threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)").unwrap();
            conn.execute(
                "INSERT INTO threads VALUES ('fixture-task', ?1)",
                [provider],
            )
            .unwrap();
        }
        if rollout {
            let dir = home.join("sessions/2026/09/09");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("fixture.jsonl"), format!("{}\n{{\"type\":\"event_msg\",\"payload\":{{\"message\":\"fixture conversation body\"}}}}\n", json!({"type":"session_meta","payload":{"id":"fixture-task","model_provider":provider}}))).unwrap();
        }
    }
    fn reopen_fixture(home: &Path) {
        let conn = rusqlite::Connection::open_with_flags(
            home.join("state_5.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let id: String = conn
            .query_row(
                "SELECT model_provider FROM threads WHERE id='fixture-task'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let config = read_config(home).unwrap();
        assert!(
            config["model_providers"].get(&id).is_some(),
            "saved provider must remain resolvable"
        );
        assert!(
            home.join(".env").is_file(),
            "runtime credential environment must remain available"
        );
    }
    #[test]
    fn provider_bound_task_blocks_subscription_return_and_can_still_resolve() {
        let (_tmp, home, root, profile) = fixture();
        commit(&root, &prepare(&root, &home, Some(&profile)).unwrap()).unwrap();
        task_fixture(&home, "custom", true, true);
        let before = snapshot(&home).unwrap();
        let rollout = home.join("sessions/2026/09/09/fixture.jsonl");
        let body = std::fs::read(&rollout).unwrap();
        let db = std::fs::read(home.join("state_5.sqlite")).unwrap();
        let err = prepare(&root, &home, None).err().unwrap().to_string();
        assert!(err.contains("Saved Codex tasks"));
        assert!(
            !err.contains("fixture-task")
                && !err.contains("fixture.example")
                && !err.contains("fixture-secret")
        );
        assert!(snapshot(&home).unwrap() == before);
        assert_eq!(std::fs::read(rollout).unwrap(), body);
        assert_eq!(std::fs::read(home.join("state_5.sqlite")).unwrap(), db);
        reopen_fixture(&home);
    }
    #[test]
    fn another_endpoint_cannot_rebind_custom_even_with_same_source_field_names() {
        let (_tmp, home, root, profile) = fixture();
        commit(&root, &prepare(&root, &home, Some(&profile)).unwrap()).unwrap();
        task_fixture(&home, "custom", true, true);
        let second_source = home.join("second-source.env");
        std::fs::write(
            &second_source,
            "TEAM_TOKEN=second-fixture-secret\nTEAM_ENDPOINT=https://second.example/v1\n",
        )
        .unwrap();
        let second = Profile {
            label: "second".into(),
            source: second_source,
            ..profile
        };
        let before = snapshot(&home).unwrap();
        assert!(prepare(&root, &home, Some(&second)).is_err());
        assert!(snapshot(&home).unwrap() == before);
        reopen_fixture(&home);
    }
    #[test]
    fn binding_created_after_preparation_is_rechecked_before_commit() {
        for (db, rollout) in [(true, false), (false, true)] {
            let (_tmp, home, root, profile) = fixture();
            commit(&root, &prepare(&root, &home, Some(&profile)).unwrap()).unwrap();
            let back = prepare(&root, &home, None).unwrap();
            task_fixture(&home, "custom", db, rollout);
            if rollout {
                std::fs::rename(home.join("sessions"), home.join("archived_sessions")).unwrap();
            }
            assert!(commit(&root, &back).is_err());
            assert!(!pending(&root).exists());
            assert_eq!(status(&root, &home).unwrap()["active_label"], "team");
        }
    }
    #[test]
    fn recovery_cannot_bypass_saved_task_compatibility() {
        let (_tmp, home, root, profile) = fixture();
        let change = prepare(&root, &home, Some(&profile)).unwrap();
        commit(&root, &change).unwrap();
        task_fixture(&home, "custom", true, true);
        persist(&pending(&root), &change).unwrap();
        let before = snapshot(&home).unwrap();
        assert!(recover(&root, &home).is_err());
        assert!(pending(&root).exists());
        assert!(snapshot(&home).unwrap() == before);
        reopen_fixture(&home);
    }

    #[test]
    fn unreadable_task_schema_and_credential_rebinding_fail_closed() {
        let (_tmp, home, root, profile) = fixture();
        commit(&root, &prepare(&root, &home, Some(&profile)).unwrap()).unwrap();
        std::fs::write(home.join("state_5.sqlite"), "fixture-invalid-db").unwrap();
        assert!(prepare(&root, &home, None).is_err());
        std::fs::remove_file(home.join("state_5.sqlite")).unwrap();
        {
            let db = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
            db.execute_batch("CREATE TABLE threads (unknown_schema TEXT)")
                .unwrap();
        }
        assert!(prepare(&root, &home, None).is_err());
        std::fs::remove_file(home.join("state_5.sqlite")).unwrap();
        task_fixture(&home, "custom", true, true);
        let before = snapshot(&home).unwrap();
        let after = Snapshot {
            config: before.config.clone(),
            env: Some("TEAM_TOKEN=another-fixture".into()),
        };
        assert!(ensure_task_compatibility(&home, &before, &after).is_err());
    }

    #[test]
    fn switch_and_return_preserve_workspace_and_original_config() {
        let (_tmp, home, root, profile) = fixture();
        let before = snapshot(&home).unwrap();
        let p = prepare(&root, &home, Some(&profile)).unwrap();
        commit(&root, &p).unwrap();
        let conf = read_config(&home).unwrap();
        assert_eq!(conf["model"].as_str(), Some("organization-model"));
        assert!(conf.get("forced_login_method").is_none());
        assert_eq!(conf["features"]["custom"].as_bool(), Some(true));
        assert_eq!(status(&root, &home).unwrap()["active_label"], "team");
        let back = prepare(&root, &home, None).unwrap();
        commit(&root, &back).unwrap();
        assert!(snapshot(&home).unwrap() == before);
        for f in ["auth.json", "state.sqlite", ".codex-global-state.json"] {
            assert_eq!(std::fs::read_to_string(home.join(f)).unwrap(), "untouched");
        }
        assert_eq!(
            std::fs::read_to_string(home.join("automations/task.toml")).unwrap(),
            "unchanged-workflow"
        );
    }
    #[test]
    fn interrupted_write_recovers_and_external_edits_are_not_overwritten() {
        let (_tmp, home, root, profile) = fixture();
        let before = snapshot(&home).unwrap();
        let p = prepare(&root, &home, Some(&profile)).unwrap();
        private_dir(&runtime(&root)).unwrap();
        persist(&pending(&root), &p).unwrap();
        write_optional(&home.join("config.toml"), &p.after.config).unwrap();
        recover(&root, &home).unwrap();
        assert!(snapshot(&home).unwrap() == before);
        commit(&root, &prepare(&root, &home, Some(&profile)).unwrap()).unwrap();
        std::fs::write(home.join("config.toml"), "model='new-user-choice'\n").unwrap();
        assert!(prepare(&root, &home, None).is_err());
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            "model='new-user-choice'\n"
        );
    }
    #[test]
    fn missing_model_and_invalid_source_do_not_write_anything() {
        let (_tmp, home, root, mut profile) = fixture();
        let before = snapshot(&home).unwrap();
        profile.model = None;
        assert!(prepare(&root, &home, Some(&profile)).is_err());
        assert!(snapshot(&home).unwrap() == before);
        assert!(!runtime(&root).exists());
    }
}
