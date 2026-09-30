//! Legacy native chat archives and capture/restore adapters. Automatic cross-Mac
//! replication is retired; status and disable retain access to existing settings.
pub mod claude;
pub mod codex;
// Keep archive/recovery primitives and their regression tests while the bulk
// replication entry point is retired. Library sync also uses its file utilities.
#[allow(dead_code)]
pub(crate) mod engine;
#[cfg(test)]
mod engine_tests;

use crate::{AppError, Result};
use engine::{Counts, Provider, error};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    /// Retired: automatic chat sync can no longer be enabled.
    Enable {
        #[arg(long)]
        json: bool,
        /// Use another private transport directory (for example a test folder).
        #[arg(long)]
        folder: Option<PathBuf>,
    },
    /// Clear the legacy enabled setting, retaining all chats and archives.
    Disable {
        #[arg(long)]
        json: bool,
    },
    /// Retired: bulk chat synchronization is no longer available.
    Run {
        #[arg(long)]
        json: bool,
    },
    /// Retired: automatic chat-sync path mappings are no longer editable.
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
    local: PathBuf,
    icloud: PathBuf,
}

impl Paths {
    fn at(home: PathBuf) -> Self {
        Self {
            local: home.join("Library/Application Support/Switchboard Sync"),
            icloud: home.join("Library/Mobile Documents/com~apple~CloudDocs"),
        }
    }
    fn resolve() -> Result<Self> {
        Ok(Self::at(crate::cache::home_dir()?))
    }
    fn settings(&self) -> PathBuf {
        self.local.join("config.json")
    }
    #[cfg(test)]
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
            error("The legacy chat sync settings are unreadable. Restore them from backup to inspect or disable them.")
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
    #[serde(default)]
    retired: bool,
    #[serde(default)]
    legacy_enabled: bool,
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
        enabled: false,
        retired: true,
        legacy_enabled: settings.enabled,
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
    value.enabled = false;
    value.retired = true;
    value.legacy_enabled = settings.enabled;
    value.available = available(paths, settings);
    value.sync_root = settings.folder.clone();
    value.message = Some(RETIRED_MESSAGE.into());
    for account in &mut value.accounts {
        account.state = "retired".into();
        account.detail =
            "Historical sync counts only; automatic chat sync has been removed.".into();
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

pub(crate) fn provider_stopped(provider: Provider) -> Result<bool> {
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

const RETIRED_MESSAGE: &str = "Automatic chat sync has been removed. Existing chats, iCloud archives, and recovery records are kept. Use `chat-sync status` to inspect legacy state or `chat-sync disable` to clear its old enabled setting.";

fn execute(paths: &Paths, action: &Action) -> Result<Status> {
    // Reject stale menu-bar clients and scripts before reading any chat stores,
    // archive directory, or old settings. An enabled legacy config is inert.
    if matches!(
        action,
        Action::Enable { .. } | Action::Run { .. } | Action::MapPath { .. }
    ) {
        return Err(error(RETIRED_MESSAGE));
    }
    let mut settings = paths.load_settings()?;
    if matches!(action, Action::Disable { .. }) && settings.enabled {
        settings.enabled = false;
        paths.save_settings(&settings)?;
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
                if let Some(message) = report.message {
                    println!("{message}");
                }
                if report.legacy_enabled {
                    println!("Legacy enabled setting: true (inactive)");
                }
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
                    serde_json::json!({"enabled":false,"retired":true,"available":false,"message":public_error(&value),"accounts":[]})
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
    fn status_and_disable_without_legacy_data_create_nothing() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::at(home.path().to_path_buf());
        for action in [
            Action::Status { json: true },
            Action::Disable { json: true },
        ] {
            let report = execute(&paths, &action).unwrap();
            assert!(!report.enabled);
            assert!(report.retired);
            assert!(!report.legacy_enabled);
        }
        assert!(!paths.local.exists());
        assert!(!paths.icloud.exists());
    }

    #[test]
    fn retired_bulk_commands_cannot_activate_fresh_or_previously_enabled_settings() {
        for previously_enabled in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let paths = Paths::at(home.path().to_path_buf());
            let mut settings = paths.load_settings().unwrap();
            if previously_enabled {
                settings.enabled = true;
                paths.save_settings(&settings).unwrap();
            }
            std::fs::create_dir_all(&paths.icloud).unwrap();
            let before = std::fs::read(paths.settings()).ok();
            for action in [
                Action::Enable {
                    json: true,
                    folder: None,
                },
                Action::Enable {
                    json: true,
                    folder: Some(home.path().join("other-archive")),
                },
                Action::Run { json: true },
                Action::MapPath {
                    json: true,
                    from: "/old/project".into(),
                    to: "/new/project".into(),
                },
            ] {
                let failure = execute(&paths, &action).unwrap_err();
                assert_eq!(public_error(&failure), RETIRED_MESSAGE);
                assert_eq!(std::fs::read(paths.settings()).ok(), before);
                assert!(!paths.state().exists());
                assert!(!paths.report().exists());
                assert!(!settings.folder.exists());
            }
            let report = execute(&paths, &Action::Status { json: true }).unwrap();
            assert!(!report.enabled);
            assert!(report.retired);
            assert_eq!(report.legacy_enabled, previously_enabled);
        }
    }

    #[test]
    fn status_and_disable_preserve_legacy_archives_native_chats_and_recovery_data() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::at(home.path().to_path_buf());
        let mut settings = paths.load_settings().unwrap();
        settings.enabled = true;
        settings
            .mappings
            .push(("/old/project".into(), "/new/project".into()));
        paths.save_settings(&settings).unwrap();
        let original_settings = std::fs::read(paths.settings()).unwrap();
        let archive = settings
            .folder
            .join("codex/fixture-chat/fixture-revision.json");
        let native = home.path().join(".codex/sessions/fixture.jsonl");
        let files = [
            (archive, b"synthetic immutable archive".as_slice()),
            (native, b"synthetic native history".as_slice()),
            (paths.state(), b"synthetic recovery journal".as_slice()),
            (paths.report(), br#"{"enabled":true,"available":true,"sync_root":"/old","device_id":"fixture","last_sync_at":"2026-09-29T10:00:00Z","accounts":[{"id":"codex","provider":"codex","label":"Codex","state":"waiting","detail":"Will retry automatically","exported":2,"imported":1,"conflicts":0,"pending":1}],"message":"Enable on the other Mac","pending":1,"conflicts":0}"#.as_slice()),
        ];
        for (path, contents) in &files {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        let report = execute(&paths, &Action::Status { json: true }).unwrap();
        assert!(!report.enabled);
        assert!(report.legacy_enabled);
        assert_eq!(report.device_id.as_deref(), Some("fixture"));
        assert_eq!(report.accounts[0].counts.exported, 2);
        assert_eq!(report.accounts[0].state, "retired");
        assert_eq!(report.message.as_deref(), Some(RETIRED_MESSAGE));
        assert_eq!(std::fs::read(paths.settings()).unwrap(), original_settings);
        let disabled = execute(&paths, &Action::Disable { json: true }).unwrap();
        assert!(!disabled.enabled);
        assert!(!disabled.legacy_enabled);
        let after = paths.load_settings().unwrap();
        assert!(!after.enabled);
        assert_eq!(after.folder, settings.folder);
        assert_eq!(after.mappings, settings.mappings);
        for (path, contents) in files {
            assert_eq!(std::fs::read(path).unwrap(), contents);
        }
    }

    #[test]
    fn corrupt_display_cache_does_not_block_reading_status_or_disabling() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::at(home.path().to_path_buf());
        let mut settings = paths.load_settings().unwrap();
        settings.enabled = true;
        paths.save_settings(&settings).unwrap();
        std::fs::write(paths.report(), b"interrupted display cache").unwrap();
        let before = std::fs::read(paths.report()).unwrap();
        let report = execute(&paths, &Action::Status { json: true }).unwrap();
        assert!(!report.enabled);
        assert!(report.legacy_enabled);
        assert!(
            !execute(&paths, &Action::Disable { json: true })
                .unwrap()
                .legacy_enabled
        );
        assert_eq!(std::fs::read(paths.report()).unwrap(), before);
        assert!(!paths.load_settings().unwrap().enabled);
        assert!(!paths.state().exists());
        assert!(!settings.folder.exists());
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
