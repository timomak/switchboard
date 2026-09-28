//! Cross-Mac native chat replication. Credentials and application databases
//! remain local; iCloud carries immutable, provider-specific session packages.
pub mod claude;
pub mod codex;
mod engine;
#[cfg(test)]
mod engine_tests;

use crate::{AppError, Result};
use engine::{Adapter, Counts, Provider, State, error};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// An adapter-owned native session, never a lossy user/assistant transcript.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct NativeSession {
    pub id: String,
    pub payload: serde_json::Value,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum Action {
    /// Read local sync status without reading or uploading chats.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Enable chat sync through the user's iCloud Drive.
    Enable {
        #[arg(long)]
        json: bool,
        /// Use another private transport directory (for example a test folder).
        #[arg(long)]
        folder: Option<PathBuf>,
    },
    /// Pause syncing. Existing local chats and iCloud snapshots are retained.
    Disable {
        #[arg(long)]
        json: bool,
    },
    /// Sync closed apps now. Running apps are left alone and retried later.
    Run {
        #[arg(long)]
        json: bool,
    },
    /// Map a project folder from the other Mac to its location on this Mac.
    MapPath {
        from: String,
        to: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Settings {
    version: u32,
    enabled: bool,
    folder: PathBuf,
    #[serde(default)]
    mappings: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct Paths {
    home: PathBuf,
    local: PathBuf,
    icloud: PathBuf,
    codex: PathBuf,
    claude: PathBuf,
}

impl Paths {
    fn at(home: PathBuf) -> Self {
        Self {
            local: home.join("Library/Application Support/Switchboard Sync"),
            icloud: home.join("Library/Mobile Documents/com~apple~CloudDocs"),
            codex: home.join(".codex"),
            claude: home.join("Library/Application Support/Claude"),
            home,
        }
    }
    fn resolve() -> Result<Self> {
        let mut paths = Self::at(crate::cache::home_dir()?);
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            paths.codex = PathBuf::from(home);
        }
        Ok(paths)
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
    fn load_settings(&self) -> Result<Settings> {
        if !self.settings().try_exists()? {
            return Ok(Settings {
                version: engine::VERSION,
                enabled: false,
                folder: self.icloud.join("Switchboard/Chat Sync"),
                mappings: vec![],
            });
        }
        let value: Settings = serde_json::from_slice(&engine::read_regular(
            &self.settings(),
            1024 * 1024,
        )?)
        .map_err(|_| {
            error("The chat sync settings are unreadable. Restore them before enabling sync.")
        })?;
        if value.version != engine::VERSION || !value.folder.is_absolute() {
            return Err(error("The chat sync settings use an unsupported format."));
        }
        Ok(value)
    }
    fn save_settings(&self, settings: &Settings) -> Result<()> {
        engine::atomic_write(&self.settings(), &serde_json::to_vec(settings)?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProviderStatus {
    id: String,
    provider: Provider,
    label: String,
    state: String,
    detail: String,
    #[serde(flatten)]
    counts: Counts,
}

impl ProviderStatus {
    fn new(provider: Provider, state: &str, detail: &str) -> Self {
        Self {
            id: provider.key().into(),
            provider,
            label: provider.label().into(),
            state: state.into(),
            detail: detail.into(),
            counts: Counts::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Status {
    enabled: bool,
    available: bool,
    sync_root: PathBuf,
    device_id: Option<String>,
    last_sync_at: Option<String>,
    accounts: Vec<ProviderStatus>,
    message: Option<String>,
    pending: usize,
    conflicts: usize,
}

fn available(paths: &Paths, settings: &Settings) -> bool {
    if settings.folder == paths.icloud.join("Switchboard/Chat Sync") {
        paths.icloud.is_dir()
            && !paths
                .icloud
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
    } else {
        settings.folder.is_dir()
            && !settings
                .folder
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
    }
}

fn status(paths: &Paths, settings: &Settings) -> Result<Status> {
    // This report is a disposable display cache, not the recovery journal.
    // Damaged/older display data must not prevent disabling sync or rebuilding
    // the report; settings and replica-record errors still fail closed.
    let cached = if paths.report().try_exists()? {
        serde_json::from_slice(&engine::read_regular(&paths.report(), 1024 * 1024)?).ok()
    } else {
        None
    };
    let mut value = cached.unwrap_or_else(|| Status {
        enabled: settings.enabled,
        available: available(paths, settings),
        sync_root: settings.folder.clone(),
        device_id: None,
        last_sync_at: None,
        accounts: vec![
            ProviderStatus::new(
                Provider::Codex,
                "ready",
                "Shared Codex history across your accounts.",
            ),
            ProviderStatus::new(
                Provider::Claude,
                "ready",
                "Shared native Cowork history across your accounts.",
            ),
        ],
        message: None,
        pending: 0,
        conflicts: 0,
    });
    value.enabled = settings.enabled;
    value.available = available(paths, settings);
    value.sync_root = settings.folder.clone();
    if !settings.enabled {
        value.message = Some("Chat sync is off. Existing chats and snapshots are kept.".into());
    } else if !value.available {
        value.message =
            Some("The sync folder is unavailable. Enable iCloud Drive on this Mac.".into());
    }
    Ok(value)
}

/// Only human-safe messages intentionally authored by the adapters are sent to
/// the UI. Native SQL errors, JSON contents and paths never become diagnostics.
fn public_error(value: &AppError) -> String {
    match value {
        AppError::Other(message) => crate::display::sanitize_untrusted_line(message),
        _ => "Could not read or restore native chat data. Your existing chats and sync snapshots were kept.".into(),
    }
}

struct NativeAdapter<'a> {
    paths: &'a Paths,
    provider: Provider,
    mappings: Vec<(String, String)>,
}

impl Adapter for NativeAdapter<'_> {
    fn capture(&mut self) -> Result<Vec<NativeSession>> {
        match self.provider {
            Provider::Codex => codex::capture(&self.paths.codex),
            Provider::Claude => claude::capture(&self.paths.claude, &self.paths.home),
        }
    }
    fn restore(&mut self, session: &NativeSession, target_id: &str) -> Result<()> {
        match self.provider {
            Provider::Codex => codex::restore_if_idle(
                &self.paths.codex,
                session,
                target_id,
                &self.mappings,
                &|| provider_stopped(Provider::Codex),
            ),
            Provider::Claude => claude::restore_if_idle(
                &self.paths.claude,
                &self.paths.home,
                session,
                target_id,
                &self.mappings,
                &|| provider_stopped(Provider::Claude),
            ),
        }
    }
    fn ready(&self) -> Result<bool> {
        provider_stopped(self.provider)
    }
}

fn provider_executable(command: &str, provider: Provider) -> bool {
    let command = command.trim().trim_matches(['\'', '"']);
    let name = Path::new(command)
        .file_name()
        .and_then(|p| p.to_str())
        .unwrap_or("");
    match provider {
        Provider::Codex => {
            name.eq_ignore_ascii_case("codex")
                || name.starts_with("Codex Helper")
                || matches!(name, "codex-app-server" | "codex-daemon")
                || command.contains("/Codex.app/Contents/")
        }
        Provider::Claude => {
            name.eq_ignore_ascii_case("claude")
                || name.starts_with("Claude Helper")
                || matches!(name, "claude-code" | "claude-daemon")
                || command.contains("/Claude.app/Contents/")
        }
    }
}

/// Inspect only the executable and the Node-compatible runtime's entrypoint.
/// A chat prompt mentioning another app must not mark that app as running.
fn process_is_provider(command: &str, provider: Provider) -> bool {
    let mut arguments = command.split_whitespace();
    let Some(executable) = arguments.next() else {
        return false;
    };
    if provider_executable(executable, provider) {
        return true;
    }
    let name = Path::new(executable.trim_matches(['\'', '"']))
        .file_name()
        .and_then(|p| p.to_str())
        .unwrap_or("");
    if !matches!(name, "node" | "nodejs" | "bun" | "deno") {
        return false;
    }
    let mut script = None;
    while let Some(argument) = arguments.next() {
        if matches!(argument, "--eval" | "-e" | "--print" | "-p") {
            return false;
        }
        if matches!(
            argument,
            "--require" | "-r" | "--import" | "--loader" | "--experimental-loader"
        ) {
            arguments.next();
            continue;
        }
        if argument == "--" {
            script = arguments.next();
            break;
        }
        if argument.starts_with('-') || (matches!(name, "bun" | "deno") && argument == "run") {
            continue;
        }
        script = Some(argument);
        break;
    }
    let Some(script) = script.map(|s| s.trim_matches(['\'', '"'])) else {
        return false;
    };
    match provider {
        Provider::Codex => script.ends_with("/@openai/codex/bin/codex.js"),
        Provider::Claude => {
            script.ends_with("/@anthropic-ai/claude-code/cli.js")
                || script.ends_with("/@anthropic-ai/claude-code/cli.mjs")
        }
    }
}

fn provider_stopped(provider: Provider) -> Result<bool> {
    if !cfg!(target_os = "macos") {
        return Err(error("Native chat sync is available on macOS."));
    }
    // `comm` preserves app executable paths containing spaces. `args` exposes
    // npm-installed CLI entrypoints whose executable is otherwise just `node`.
    // These process details stay in memory and are never printed or persisted.
    for field in ["comm=", "args="] {
        let output = std::process::Command::new("/bin/ps")
            .args(["-axww", "-o", field])
            .output()
            .map_err(|_| error("Could not check whether the chat apps are closed."))?;
        if !output.status.success() {
            return Err(error("Could not check whether the chat apps are closed."));
        }
        if String::from_utf8_lossy(&output.stdout).lines().any(|line| {
            if field == "comm=" {
                provider_executable(line, provider)
            } else {
                process_is_provider(line, provider)
            }
        }) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn run_sync(paths: &Paths, settings: &Settings) -> Result<Status> {
    let mut report = status(paths, settings)?;
    if !settings.enabled || !report.available {
        return Ok(report);
    }
    engine::private_dir(&paths.local)?;
    let _sync_lock =
        crate::cache::acquire_lock(&paths.local.join("sync.lock"), Duration::from_secs(2))?;
    engine::private_dir(&settings.folder)?;
    let mut state = State::load(&paths.state())?;
    state.save(&paths.state())?;
    report.device_id = Some(state.device_id.clone());
    report.accounts.clear();
    report.pending = 0;
    report.conflicts = 0;
    let mut mappings = settings.mappings.clone();
    mappings.push((
        "__SWITCHBOARD_HOME__".into(),
        paths.home.to_string_lossy().into_owned(),
    ));
    // Longer source prefixes must win over a broad home-directory mapping.
    mappings.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
    for provider in [Provider::Codex, Provider::Claude] {
        let mut item = ProviderStatus::new(provider, "ready", "History is up to date.");
        let result = (|| -> Result<Counts> {
            if !provider_stopped(provider)? {
                return Ok(Counts {
                    pending: 1,
                    ..Counts::default()
                });
            }
            // Coordinate with existing account switching. No authentication
            // file is read, copied, refreshed, or uploaded by this feature.
            let config = crate::config::Config::load()?;
            let claude_paths = crate::claude_desktop::Paths::resolve(&config.anthropic)?;
            let codex_paths = crate::codex_account::store::Paths::resolve()?;
            let lock_path = match provider {
                Provider::Codex => codex_paths.root.join("operation.lock"),
                Provider::Claude => claude_paths.account_switch_lock(),
            };
            engine::private_dir(
                lock_path
                    .parent()
                    .ok_or_else(|| error("Invalid account lock path."))?,
            )?;
            let _account_lock = crate::cache::acquire_lock(&lock_path, Duration::from_secs(2))?;
            let _cli_lock = if provider == Provider::Codex {
                Some(crate::cli_session::exclusive(&codex_paths.root)?)
            } else {
                None
            };
            if !provider_stopped(provider)? {
                return Ok(Counts {
                    pending: 1,
                    ..Counts::default()
                });
            }
            if provider == Provider::Claude {
                claude::reconcile_if_idle(&paths.claude, &paths.home, &mappings, &|| {
                    provider_stopped(Provider::Claude)
                })?;
            }
            let mut adapter = NativeAdapter {
                paths,
                provider,
                mappings: mappings.clone(),
            };
            engine::sync(
                &settings.folder,
                &paths.state(),
                &mut state,
                provider,
                &mut adapter,
            )
        })();
        match result {
            Ok(counts) => {
                item.counts = counts;
                if item.counts.pending > 0 {
                    item.state = "waiting".into();
                    item.detail = format!(
                        "Quit {} and its CLI sessions to sync. Incomplete iCloud downloads retry automatically.",
                        provider.label()
                    );
                } else if item.counts.conflicts > 0 {
                    item.detail =
                        "Both versions of changed chats were kept as separate native chats.".into();
                }
            }
            Err(value) => {
                item.state = "error".into();
                item.detail = public_error(&value);
                item.counts.pending = 1;
            }
        }
        report.pending += item.counts.pending;
        report.conflicts += item.counts.conflicts;
        report.accounts.push(item);
    }
    report.last_sync_at =
        Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    report.message = Some("Enable chat sync on your other Mac with the same iCloud account. Sign into Claude and Codex separately on each Mac.".into());
    engine::atomic_write(&paths.report(), &serde_json::to_vec(&report)?)?;
    Ok(report)
}

fn execute(paths: &Paths, action: &Action) -> Result<Status> {
    let mut settings = paths.load_settings()?;
    if matches!(action, Action::Status { .. }) {
        return status(paths, &settings);
    }
    match action {
        Action::Enable { folder, .. } => {
            if let Some(folder) = folder {
                if !folder.is_absolute() || !folder.is_dir() {
                    return Err(error(
                        "Choose an existing absolute path for the private sync folder.",
                    ));
                }
                if settings.enabled && settings.folder != *folder {
                    return Err(error("Disable sync before changing its folder."));
                }
                if paths.state().exists() && settings.folder != *folder {
                    return Err(error(
                        "This Mac already has a sync archive. Keep its existing folder so revision history and recovery records stay together.",
                    ));
                }
                settings.folder = folder.clone();
            }
            if !available(paths, &settings) {
                return Err(error(
                    "Enable iCloud Drive in System Settings before enabling chat sync.",
                ));
            }
            settings.enabled = true;
            paths.save_settings(&settings)?;
        }
        Action::Disable { .. } => {
            settings.enabled = false;
            paths.save_settings(&settings)?;
        }
        Action::MapPath { from, to, .. } => {
            if !Path::new(from).is_absolute()
                || !Path::new(to).is_absolute()
                || from.contains('\0')
                || to.contains('\0')
            {
                return Err(error(
                    "Project path mappings must use absolute folder paths.",
                ));
            }
            settings.mappings.retain(|(key, _)| key != from);
            settings.mappings.push((from.clone(), to.clone()));
            paths.save_settings(&settings)?;
        }
        Action::Run { .. } => return run_sync(paths, &settings),
        Action::Status { .. } => (),
    }
    status(paths, &settings)
}

pub fn run(action: &Action) -> i32 {
    let result = Paths::resolve().and_then(|paths| execute(&paths, action));
    let json = match action {
        Action::Status { json }
        | Action::Enable { json, .. }
        | Action::Disable { json }
        | Action::Run { json }
        | Action::MapPath { json, .. } => *json,
    };
    match result {
        Ok(report) => {
            if json {
                println!("{}", serde_json::to_string(&report).unwrap_or_default());
            } else {
                println!(
                    "Chat sync: {}",
                    if report.enabled { "enabled" } else { "off" }
                );
                for item in report.accounts {
                    println!("{}: {}", item.label, item.detail);
                }
            }
            0
        }
        Err(value) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"enabled":false,"available":false,"message":public_error(&value),"accounts":[]})
                );
            } else {
                eprintln!("{}", public_error(&value));
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_status_and_run_do_not_create_or_upload_data() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::at(home.path().to_path_buf());
        assert!(
            !execute(&paths, &Action::Status { json: true })
                .unwrap()
                .enabled
        );
        assert!(
            !execute(&paths, &Action::Run { json: true })
                .unwrap()
                .enabled
        );
        assert!(!paths.local.exists());
        assert!(!paths.icloud.exists());
    }
    #[test]
    fn enable_requires_icloud_and_never_uploads_on_its_own() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::at(home.path().to_path_buf());
        assert!(
            execute(
                &paths,
                &Action::Enable {
                    json: true,
                    folder: None
                }
            )
            .is_err()
        );
        std::fs::create_dir_all(&paths.icloud).unwrap();
        assert!(
            execute(
                &paths,
                &Action::Enable {
                    json: true,
                    folder: None
                }
            )
            .unwrap()
            .enabled
        );
        assert!(!paths.state().exists());
        assert!(!paths.icloud.join("Switchboard").exists());
        assert!(
            !execute(&paths, &Action::Disable { json: true })
                .unwrap()
                .enabled
        );
    }
    #[test]
    fn corrupt_display_cache_does_not_block_disabling_or_reading_status() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::at(home.path().to_path_buf());
        std::fs::create_dir_all(&paths.icloud).unwrap();
        execute(
            &paths,
            &Action::Enable {
                json: true,
                folder: None,
            },
        )
        .unwrap();
        std::fs::write(paths.report(), b"interrupted display cache").unwrap();
        let before = std::fs::read(paths.report()).unwrap();
        assert!(
            execute(&paths, &Action::Status { json: true })
                .unwrap()
                .enabled
        );
        assert_eq!(std::fs::read(paths.report()).unwrap(), before);
        assert!(
            !execute(&paths, &Action::Disable { json: true })
                .unwrap()
                .enabled
        );
        assert!(!paths.load_settings().unwrap().enabled);
        assert!(!paths.state().exists());
        assert!(!paths.icloud.join("Switchboard").exists());
    }
    #[test]
    fn existing_recovery_record_prevents_changing_transport_folder() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::at(home.path().to_path_buf());
        std::fs::create_dir_all(&paths.icloud).unwrap();
        execute(
            &paths,
            &Action::Enable {
                json: true,
                folder: None,
            },
        )
        .unwrap();
        State::default().save(&paths.state()).unwrap();
        execute(&paths, &Action::Disable { json: true }).unwrap();
        let replacement = home.path().join("different-transport");
        std::fs::create_dir(&replacement).unwrap();
        assert!(
            execute(
                &paths,
                &Action::Enable {
                    json: true,
                    folder: Some(replacement.clone())
                }
            )
            .is_err()
        );
        assert!(!paths.load_settings().unwrap().enabled);
        assert_ne!(paths.load_settings().unwrap().folder, replacement);
    }
    #[test]
    fn provider_processes_are_separate_and_fail_closed_for_helpers() {
        assert!(process_is_provider(
            "/Applications/Claude.app/Contents/MacOS/Claude",
            Provider::Claude
        ));
        assert!(process_is_provider(
            "/opt/homebrew/bin/codex",
            Provider::Codex
        ));
        assert!(process_is_provider(
            "/Applications/Codex.app/Contents/Frameworks/Codex Helper.app/Contents/MacOS/Codex Helper",
            Provider::Codex
        ));
        assert!(!process_is_provider("ai-usagebar", Provider::Codex));
        assert!(!process_is_provider(
            "/Applications/Claude.app/Contents/MacOS/Claude",
            Provider::Codex
        ));
    }
    #[test]
    fn npm_entrypoints_and_daemons_block_only_their_own_provider() {
        assert!(process_is_provider(
            "/opt/homebrew/bin/node /opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js --resume",
            Provider::Claude
        ));
        assert!(process_is_provider(
            "node --require /fixture/bootstrap.js /fixture/node_modules/@openai/codex/bin/codex.js app-server",
            Provider::Codex
        ));
        assert!(process_is_provider(
            "bun /fixture/node_modules/@anthropic-ai/claude-code/cli.mjs",
            Provider::Claude
        ));
        assert!(process_is_provider(
            "/fixture/codex-daemon --background",
            Provider::Codex
        ));
        assert!(provider_executable(
            "/Applications/Company Tools/Codex.app/Contents/MacOS/Codex",
            Provider::Codex
        ));
        assert!(!process_is_provider(
            "node /fixture/node_modules/@anthropic-ai/claude-code/cli.js",
            Provider::Codex
        ));
        assert!(!process_is_provider(
            "node /fixture/server.js --prompt /fixture/node_modules/@openai/codex/bin/codex.js",
            Provider::Codex
        ));
        assert!(!process_is_provider(
            "/bin/echo /Applications/Claude.app/Contents/MacOS/Claude",
            Provider::Claude
        ));
        assert!(!process_is_provider(
            "node -e 'console.log(\"claude\")'",
            Provider::Claude
        ));
    }
}
