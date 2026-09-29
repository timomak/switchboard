//! Selected personal tools, with private local bindings and immutable iCloud revisions.
mod cowork;
mod engine;
#[cfg(test)]
mod engine_tests;
mod mcp;
mod model;
mod native;
mod skills;
#[cfg(test)]
mod smoke_tests;
mod storage;

use crate::{AppError, Result};
use model::{Content, InventoryCandidate, Kind, Target};
use native::{NativeAdapter, NativeRoots};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use storage::{atomic_write, error, private_dir, read_regular};

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum Category {
    Skills,
    Mcp,
}
#[derive(clap::Subcommand, Clone, Debug)]
pub enum Action {
    Status {
        #[arg(long)]
        json: bool,
    },
    Inventory {
        #[arg(long)]
        json: bool,
    },
    Enable {
        #[arg(long)]
        kind: Category,
        #[arg(long)]
        json: bool,
    },
    Disable {
        #[arg(long)]
        kind: Category,
        #[arg(long)]
        json: bool,
    },
    Run {
        #[arg(long)]
        json: bool,
    },
    Adopt {
        id: String,
        #[arg(long)]
        targets: String,
        #[arg(long)]
        json: bool,
    },
    Targets {
        id: String,
        #[arg(long)]
        targets: String,
        #[arg(long)]
        json: bool,
    },
    SetEnabled {
        id: String,
        #[arg(long, action = clap::ArgAction::Set)]
        enabled: bool,
        #[arg(long)]
        json: bool,
    },
    Remove {
        id: String,
        #[arg(long)]
        json: bool,
    },
    Resolve {
        id: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        json: bool,
    },
    ExportCowork {
        #[arg(long)]
        json: bool,
    },
    /// Record this Mac's user's confirmation; this does not upload a plugin.
    ConfirmCowork {
        #[arg(long)]
        version: String,
        #[arg(long)]
        json: bool,
    },
    /// Bind a required local slot to an environment variable or local path.
    Bind {
        id: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        slot: String,
        #[arg(long, conflicts_with = "path", required_unless_present = "path")]
        env_var: Option<String>,
        #[arg(long, conflicts_with = "env_var")]
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    version: u32,
    sync_skills: bool,
    sync_mcp: bool,
    #[serde(default)]
    bindings: BTreeMap<String, String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            sync_skills: false,
            sync_mcp: false,
            bindings: BTreeMap::new(),
        }
    }
}
impl Settings {
    fn bind(&mut self, item_id: &str, locator: &str, slot: &str, binding: String) {
        self.bindings
            .insert(native::binding_key(item_id, locator, slot), binding);
    }
}
struct Paths {
    home: PathBuf,
    local: PathBuf,
    icloud: PathBuf,
    cloud: PathBuf,
}
impl Paths {
    fn at(home: PathBuf) -> Self {
        let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
        Self {
            local: home.join("Library/Application Support/Switchboard Library"),
            cloud: icloud.join("Switchboard/Tools & Skills"),
            home,
            icloud,
        }
    }
    fn settings(&self) -> PathBuf {
        self.local.join("config.json")
    }
    fn state(&self) -> PathBuf {
        self.local.join("state.json")
    }
    fn report(&self) -> PathBuf {
        self.local.join("status.json")
    }
    fn available(&self) -> bool {
        self.icloud
            .symlink_metadata()
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
    }
    fn load(&self) -> Result<Settings> {
        if !self.settings().try_exists()? {
            return Ok(Settings::default());
        }
        let settings: Settings = serde_json::from_slice(&read_regular(
            &self.settings(),
            1024 * 1024,
        )?)
        .map_err(|_| {
            error("Tools & skills settings are unreadable. Existing installations were kept.")
        })?;
        if settings.version != 1 {
            return Err(error("Update Switchboard to read these library settings."));
        }
        Ok(settings)
    }
    fn save(&self, settings: &Settings) -> Result<()> {
        atomic_write(&self.settings(), &serde_json::to_vec(settings)?)
    }
}

fn roots(home: &Path) -> Result<NativeRoots> {
    let config = crate::config::Config::load()?;
    let mut codex_homes = vec![home.join(".codex")];
    if let Some(path) = std::env::var_os("CODEX_HOME") {
        codex_homes.push(PathBuf::from(path));
    }
    let cli = crate::codex_account::store::Paths::cli_at(home);
    if cli.home.is_dir() {
        codex_homes.push(cli.home);
    }
    let mut claude_homes = vec![home.join(".claude")];
    let mut claude_config_files = vec![home.join(".claude.json")];
    if let Some(path) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        let path = PathBuf::from(path);
        claude_config_files.push(path.join(".claude.json"));
        claude_homes.push(path);
    }
    if let Some(path) = &config.anthropic.credentials_path
        && let Some(parent) = path.parent()
    {
        claude_homes.push(parent.to_path_buf());
        if parent != home.join(".claude") {
            claude_config_files.push(parent.join(".claude.json"));
        }
    }
    claude_homes.extend(
        config
            .anthropic
            .all_accounts()
            .iter()
            .map(|a| a.config_dir()),
    );
    claude_config_files.extend(
        config
            .anthropic
            .all_accounts()
            .iter()
            .map(|a| a.config_dir().join(".claude.json")),
    );
    codex_homes.sort();
    codex_homes.dedup();
    claude_homes.sort();
    claude_homes.dedup();
    claude_config_files.sort();
    claude_config_files.dedup();
    Ok(NativeRoots {
        user_home: home.to_path_buf(),
        codex_homes,
        codex_skill_roots: vec![home.join(".agents/skills")],
        claude_homes,
        claude_config_files,
    })
}

fn targets(value: &str) -> Result<BTreeSet<Target>> {
    value
        .split(',')
        .map(|p| match p.trim() {
            "codex" => Ok(Target::Codex),
            "claude-code" | "claude_code" => Ok(Target::ClaudeCode),
            "cowork" => Ok(Target::Cowork),
            _ => Err(error(
                "Choose Codex, Claude Code or Cowork as a destination.",
            )),
        })
        .collect()
}

fn status(paths: &Paths, settings: &Settings) -> Result<Value> {
    let mut value = if paths.report().try_exists()? {
        serde_json::from_slice(&read_regular(&paths.report(), 4 * 1024 * 1024)?)
            .unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };
    if !value.is_object() {
        value = json!({});
    }
    value["available"] = json!(paths.available());
    value["sync_skills"] = json!(settings.sync_skills);
    value["sync_mcp"] = json!(settings.sync_mcp);
    if !value["items"].is_array() {
        value["items"] = json!([]);
    }
    value.as_object_mut().unwrap().remove("inventory");
    value["cowork"] = cowork::status(&paths.local)?;
    if !settings.sync_skills && !settings.sync_mcp {
        value["message"] = json!(
            "Choose what to sync, then adopt selected personal items. Installed content is kept while sync is off."
        );
    } else if !paths.available() {
        value["message"] = json!("Enable iCloud Drive on this Mac to sync your library.");
    }
    Ok(value)
}

fn public_inventory(candidates: &[InventoryCandidate]) -> Vec<Value> {
    candidates.iter().map(|c| {
        let kind = c.kind;
        let supported = if c.content.is_some() && c.classification == "custom" { vec![Target::Codex, Target::ClaudeCode, Target::Cowork] } else { vec![] };
        json!({"id":c.id,"name":crate::display::sanitize_untrusted_line(&c.name),"kind":kind,"origin":c.source,"classification":c.classification,"detail":c.detail,"supported_targets":supported})
    }).collect()
}

fn public_items(items: &[model::ItemStatus]) -> Vec<Value> {
    items.iter().map(|entry| {
        let state = if !entry.conflicts.is_empty() { "conflict".to_owned() }
            else if !entry.item.active() { "disabled".to_owned() }
            else if entry.destinations.is_empty() { "needs_setup".into() }
            else { entry.destinations.iter().find(|d| d.status != model::InstallStatus::Ready).map(|d| serde_json::to_value(d.status).unwrap_or_default().as_str().unwrap_or("needs_setup").to_owned()).unwrap_or_else(|| "ready".into()) };
        let destinations: Vec<_> = [Target::Codex,Target::ClaudeCode,Target::Cowork].into_iter().filter_map(|target| {
            let rows: Vec<_> = entry.destinations.iter().filter(|d|d.target==target).collect();
            let worst = rows.iter().find(|d|d.status!=model::InstallStatus::Ready).or_else(||rows.first())?;
            let details: BTreeSet<_> = rows.iter().map(|d|d.detail.as_str()).collect();
            Some(json!({"target":target,"state":worst.status,"detail":details.into_iter().collect::<Vec<_>>().join(" ")}))
        }).collect();
        json!({"id":entry.item.id,"name":crate::display::sanitize_untrusted_line(&entry.item.name),"kind":entry.item.content.kind(),"enabled":entry.item.enabled,"deleted":entry.item.deleted,"targets":entry.item.targets,"state":state,"detail":entry.item.requirements.join(" "),"destinations":destinations,"conflicts":entry.conflicts.iter().map(|r|json!({"revision":r,"label":format!("Version {}",&r[..12.min(r.len())])})).collect::<Vec<_>>()})
    }).collect()
}

fn idle(target: Target) -> Result<bool> {
    use crate::chat_sync::engine::Provider;
    match target {
        Target::Codex => crate::chat_sync::provider_stopped(Provider::Codex),
        Target::ClaudeCode => crate::chat_sync::provider_stopped(Provider::Claude),
        Target::Cowork => Ok(true),
    }
}

fn sync(
    paths: &Paths,
    settings: &Settings,
    adapter: &mut NativeAdapter<'_>,
    mut report: Value,
) -> Result<Value> {
    if (!settings.sync_skills && !settings.sync_mcp) || !paths.available() {
        return Ok(report);
    }
    private_dir(&paths.cloud)?;
    // Match account/provider switching locks, without touching authentication.
    let mut lock_paths = vec![paths.home.join(".ai-usagebar-account-switch.lock")];
    let codex = crate::codex_account::store::Paths::resolve()?;
    let cli = crate::codex_account::store::Paths::cli_at(&paths.home);
    let config = crate::config::Config::load()?;
    lock_paths
        .push(crate::claude_desktop::Paths::resolve(&config.anthropic)?.account_switch_lock());
    lock_paths.push(codex.root.join("operation.lock"));
    lock_paths.push(cli.root.join("operation.lock"));
    lock_paths.sort();
    lock_paths.dedup();
    let mut locks = Vec::new();
    for path in lock_paths {
        private_dir(
            path.parent()
                .ok_or_else(|| error("Invalid operation lock."))?,
        )?;
        locks.push(crate::cache::acquire_lock(&path, Duration::from_secs(2))?);
    }
    let mut kinds = BTreeSet::new();
    if settings.sync_skills {
        kinds.insert(Kind::Skill);
    }
    if settings.sync_mcp {
        kinds.insert(Kind::Mcp);
    }
    let mut state = engine::State::load(&paths.state())?;
    let result = engine::run(&paths.cloud, &paths.state(), &mut state, &kinds, adapter)?;
    cowork::check_updates(&paths.cloud, &paths.local)?;
    report["items"] = json!(public_items(&result.items));
    report["last_sync_at"] =
        json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    report["message"] = json!(if result.conflicts > 0 {
        "Conflicting edits are preserved. Choose a version before it is installed."
    } else if result.pending > 0 {
        "Some items need setup, a closed app, or an iCloud download. Check each destination below."
    } else {
        "Your selected library is up to date. Cowork plugin imports are handled separately."
    });
    report["cowork"] = cowork::status(&paths.local)?;
    atomic_write(&paths.report(), &serde_json::to_vec(&report)?)?;
    Ok(report)
}

fn execute(paths: &Paths, action: &Action) -> Result<Value> {
    let mut settings = paths.load()?;
    let report = status(paths, &settings)?;
    if matches!(action, Action::Status { .. })
        || (matches!(action, Action::Run { .. }) && !settings.sync_skills && !settings.sync_mcp)
    {
        return Ok(report);
    }
    let ready = |target| idle(target);
    let mut adapter = NativeAdapter::new(
        roots(&paths.home)?,
        paths.local.join("native"),
        settings.bindings.clone(),
        &ready,
    );
    if matches!(action, Action::Inventory { .. }) {
        let mut report = report;
        report["inventory"] = json!(public_inventory(&adapter.inventory()?));
        return Ok(report);
    }
    private_dir(&paths.local)?;
    let _lock = crate::cache::acquire_lock(&paths.local.join("sync.lock"), Duration::from_secs(2))?;
    match action {
        Action::Enable { kind, .. } | Action::Disable { kind, .. } => {
            let enable = matches!(action, Action::Enable { .. });
            if enable && !paths.available() {
                return Err(error(
                    "Enable iCloud Drive in System Settings before enabling library sync.",
                ));
            }
            match kind {
                Category::Skills => settings.sync_skills = enable,
                Category::Mcp => settings.sync_mcp = enable,
            }
            paths.save(&settings)?;
        }
        Action::Run { .. } => return sync(paths, &settings, &mut adapter, report),
        Action::ConfirmCowork { version, .. } => cowork::confirm(&paths.local, version)?,
        Action::ExportCowork { .. } => {
            if !paths.available() {
                return Err(error(
                    "Wait for iCloud Drive before exporting the Cowork plugin.",
                ));
            }
            cowork::export(&paths.cloud, &paths.local)?;
        }
        Action::Bind {
            id,
            target,
            slot,
            env_var,
            path,
            ..
        } => {
            let selected = targets(target)?;
            if selected.len() != 1 || selected.contains(&Target::Cowork) {
                return Err(error("Choose one local app for this binding."));
            }
            let entry = engine::list(&paths.cloud)?
                .into_iter()
                .find(|e| e.item.id == *id)
                .ok_or_else(|| error("This library item is unavailable."))?;
            let Content::Mcp { definition } = &entry.item.content else {
                return Err(error("Local bindings apply to MCP setups."));
            };
            if !mcp::binding_slots(definition)?.contains(slot) {
                return Err(error("This MCP setup does not require that binding slot."));
            }
            let binding = if let Some(name) = env_var {
                if name.is_empty()
                    || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                    || name.as_bytes()[0].is_ascii_digit()
                {
                    return Err(error(
                        "Use a valid environment variable name, never a credential value.",
                    ));
                }
                format!("env:{name}")
            } else {
                let path = path
                    .as_ref()
                    .ok_or_else(|| error("Choose a local path or environment variable."))?;
                if !path.is_absolute() || !path.exists() {
                    return Err(error("Choose an existing absolute local path."));
                }
                format!("path:{}", path.display())
            };
            let target = *selected.iter().next().unwrap();
            for (locator, _) in adapter.binding_destinations(&entry.item, target)? {
                settings.bind(&entry.item.id, &locator, slot, binding.clone());
            }
            paths.save(&settings)?;
        }
        Action::Adopt { .. }
        | Action::Targets { .. }
        | Action::SetEnabled { .. }
        | Action::Remove { .. }
        | Action::Resolve { .. } => {
            if !paths.available() {
                return Err(error(
                    "Wait for iCloud Drive before changing your shared library.",
                ));
            }
            private_dir(&paths.cloud)?;
            let mut state = engine::State::load(&paths.state())?;
            match action {
                Action::Adopt {
                    id,
                    targets: destinations,
                    ..
                } => {
                    let candidate = adapter.inventory()?.into_iter().find(|c| c.id == *id).ok_or_else(||error("This item changed or moved. Refresh the inventory and choose it again."))?;
                    if !candidate.content.as_ref().is_some_and(|c| match c.kind() {
                        Kind::Skill => settings.sync_skills,
                        Kind::Mcp => settings.sync_mcp,
                    }) {
                        return Err(error("Enable sync for this kind before adopting it."));
                    }
                    engine::select(
                        &paths.cloud,
                        &paths.state(),
                        &mut state,
                        &candidate,
                        targets(destinations)?,
                    )?;
                }
                Action::Targets {
                    id, targets: value, ..
                } => {
                    engine::update(&paths.cloud, id, Some(targets(value)?), None, false)?;
                }
                Action::SetEnabled { id, enabled, .. } => {
                    engine::update(&paths.cloud, id, None, Some(*enabled), false)?;
                }
                Action::Remove { id, .. } => {
                    engine::update(&paths.cloud, id, None, None, true)?;
                }
                Action::Resolve { id, revision, .. } => {
                    engine::resolve(&paths.cloud, id, revision)?;
                }
                _ => unreachable!(),
            }
            let mut report = report;
            cowork::check_updates(&paths.cloud, &paths.local)?;
            report["items"] = json!(public_items(&engine::list(&paths.cloud)?));
            atomic_write(&paths.report(), &serde_json::to_vec(&report)?)?;
            return sync(paths, &settings, &mut adapter, report);
        }
        Action::Status { .. } | Action::Inventory { .. } => unreachable!(),
    }
    status(paths, &settings)
}

fn public_error(value: &AppError) -> String {
    match value { AppError::Other(message) => crate::display::sanitize_untrusted_line(message), _ => "Tools & skills could not finish this operation. Existing files and library revisions were kept.".into() }
}

pub fn run(action: &Action) -> i32 {
    let json = match action {
        Action::Status { json }
        | Action::Inventory { json }
        | Action::Enable { json, .. }
        | Action::Disable { json, .. }
        | Action::Run { json }
        | Action::Adopt { json, .. }
        | Action::Targets { json, .. }
        | Action::SetEnabled { json, .. }
        | Action::Remove { json, .. }
        | Action::Resolve { json, .. }
        | Action::ExportCowork { json }
        | Action::ConfirmCowork { json, .. }
        | Action::Bind { json, .. } => *json,
    };
    let result = crate::cache::home_dir().and_then(|home| execute(&Paths::at(home), action));
    match result {
        Ok(report) => {
            if json {
                println!("{report}");
            } else {
                println!(
                    "{}",
                    report["message"]
                        .as_str()
                        .unwrap_or("Tools & skills settings updated.")
                );
            }
            0
        }
        Err(value) => {
            let message = public_error(&value);
            if json {
                println!(
                    "{}",
                    json!({"available":false,"sync_skills":false,"sync_mcp":false,"message":message,"items":[],"inventory":[],"cowork":{"state":"install_required","detail":"Installation has not been confirmed."}})
                );
            } else {
                eprintln!("{message}");
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_status_and_run_are_read_only() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::at(home.path().to_path_buf());
        for action in [Action::Status { json: true }, Action::Run { json: true }] {
            let report = execute(&paths, &action).unwrap();
            assert_eq!(report["sync_skills"], false);
            assert_eq!(report["sync_mcp"], false);
        }
        assert!(!paths.local.exists());
        assert!(!paths.icloud.exists());
    }
    #[test]
    fn targets_are_explicit_and_separate() {
        assert_eq!(
            targets("codex,cowork").unwrap(),
            BTreeSet::from([Target::Codex, Target::Cowork])
        );
        assert!(targets("").is_err());
        assert!(targets("claude").is_err());
    }
}
