//! Official CLI launching and a dedicated Codex CLI home. No Desktop tokens
//! are copied into this home; login is independent and history stays together.
use crate::{
    Result, cache,
    codex_account::{
        rpc,
        store::{self, Paths, error},
    },
};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
pub enum Provider {
    Claude,
    Codex,
}
#[derive(clap::ValueEnum, Debug, Clone, Copy)]
pub enum Mode {
    Shared,
    Separate,
}
#[derive(clap::Subcommand, Debug, Clone)]
pub enum Action {
    /// Save one Claude Bedrock connection for Desktop and managed CLI launches.
    ClaudeBedrock {
        label: String,
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        model: String,
    },
    /// Select the saved Claude Bedrock connection, or restore subscription mode.
    ClaudeConnection {
        #[arg(long)]
        desktop: bool,
        #[arg(long)]
        subscription: bool,
    },
    /// Restore the selection before an interrupted Claude Desktop connection change.
    ClaudeRecover,
    /// Credential-helper protocol for Claude Desktop; never run interactively.
    #[command(hide = true)]
    ClaudeToken,
    /// Choose which workspace newly launched Codex CLIs use.
    Mode { mode: Mode },
    /// Launch the official CLI with the selected login. Arguments follow --.
    Launch {
        provider: Provider,
        #[arg(last = true)]
        args: Vec<String>,
    },
}

pub fn separate_at(paths: &Paths) -> Result<bool> {
    match std::fs::read_to_string(paths.root.join("mode")) {
        Ok(s) if s.trim() == "separate" => Ok(true),
        Ok(s) if s.trim() == "shared" => Ok(false),
        Ok(_) => Err(error(
            "Codex CLI connection is invalid. Choose Shared or Separate again.",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(crate::AppError::io_at(paths.root.join("mode"), e)),
    }
}
pub fn set_mode_at(paths: &Paths, separate: bool) -> Result<()> {
    if separate {
        paths.ensure_ready()?;
        store::Identity::from_auth(&store::read_auth(&paths.auth())?)?;
    }
    store::private_dir(&paths.root)?;
    cache::atomic_write(
        &paths.root.join("mode"),
        if separate { b"separate" } else { b"shared" },
    )
}
pub fn status_default() -> Value {
    (|| -> Result<Value> {
        let paths = Paths::resolve_cli()?;
        let mut value = store::status(&paths)?;
        value["mode"] = json!(if separate_at(&paths)? {
            "separate"
        } else {
            "shared"
        });
        value["home"] = json!(paths.home);
        Ok(value)
    })()
    .unwrap_or_else(|_| json!({"mode":"unknown", "error":"Could not read Codex CLI profiles."}))
}

/// A launcher holds a shared lease until its CLI exits. Mutations need the
/// exclusive lease, so they never stop an app-server underneath managed work.
fn session_file(root: &Path) -> Result<File> {
    store::private_dir(root)?;
    let p = root.join("session.lock");
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&p)
        .map_err(|e| crate::AppError::io_at(&p, e))
}
pub fn exclusive(root: &Path) -> Result<File> {
    let file = session_file(root)?;
    file.try_lock_exclusive().map_err(|_| {
        error("Close the CLI sessions opened by AI Usage Bar before changing their account.")
    })?;
    Ok(file)
}

pub async fn stop_daemon(home: &Path) -> Result<()> {
    // An absent socket means no daemon needs stopping. Never address the
    // default Desktop home; callers supply the dedicated CLI home only.
    if !home
        .join("app-server-control/app-server-control.sock")
        .exists()
    {
        return Ok(());
    }
    let mut c = tokio::process::Command::new(executable(Provider::Codex)?);
    c.args(["app-server", "daemon", "stop"])
        .env("CODEX_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(20), c.status())
        .await
        .map_err(|_| error("The Codex CLI service did not stop. Close its sessions and retry."))?
        .map_err(|_| error("Could not stop the dedicated Codex CLI service."))?;
    if !status.success() {
        return Err(error("Could not stop the dedicated Codex CLI service."));
    }
    Ok(())
}

fn executable(provider: Provider) -> Result<PathBuf> {
    let name = match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    };
    let home = cache::home_dir()?;
    let mut dirs = vec![
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Some(path) = dirs.into_iter().map(|p| p.join(name)).find(|p| p.is_file()) {
        return Ok(path);
    }
    if matches!(provider, Provider::Codex) {
        return rpc::executable();
    }
    Err(error("Install the official Claude CLI before opening it."))
}

fn launch(provider: Provider, args: &[String]) -> Result<i32> {
    let paths = Paths::resolve_cli()?;
    let home = cache::home_dir()?;
    let separate = matches!(provider, Provider::Codex) && separate_at(&paths)?;
    let root = if matches!(provider, Provider::Claude) {
        home.join(".claude-acc/claude-cli-runtime")
    } else if separate {
        paths.root.clone()
    } else {
        home.join(".claude-acc/codex-profiles")
    };
    store::private_dir(&root)?;
    let _operation = cache::acquire_lock(&root.join("operation.lock"), Duration::from_secs(2))?;
    let lease = session_file(&root)?;
    FileExt::try_lock_shared(&lease)
        .map_err(|_| error("An account operation is in progress. Try opening the CLI again."))?;
    let mut c = Command::new(executable(provider)?);
    // Overrides inherited from a terminal must not silently defeat the account
    // the launcher says it selected. Values are never logged or passed as args.
    for key in [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_MANTLE",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "AWS_BEARER_TOKEN_BEDROCK",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_PROFILE",
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
        "OPENAI_API_KEY",
        "CODEX_SQLITE_HOME",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
    ] {
        c.env_remove(key);
    }
    if matches!(provider, Provider::Codex) {
        if separate {
            paths.ensure_ready()?;
            c.env("CODEX_HOME", &paths.home);
        } else {
            c.env("CODEX_HOME", Paths::resolve()?.home);
        }
    }
    if matches!(provider, Provider::Claude) {
        crate::claude_connection::configure_cli(&mut c)?;
    }
    drop(_operation);
    let status = c
        .args(args)
        .status()
        .map_err(|_| error("Could not launch the official CLI."))?;
    drop(lease);
    Ok(status.code().unwrap_or(1))
}

pub async fn run(action: &Action) -> i32 {
    let result = match action {
        Action::ClaudeBedrock {
            label,
            source,
            model,
        } => crate::claude_connection::register(label, source, model).map(|()| 0),
        Action::ClaudeConnection {
            desktop,
            subscription,
        } => crate::claude_connection::select(*desktop, *subscription).map(|()| 0),
        Action::ClaudeRecover => crate::claude_connection::recover().map(|()| 0),
        Action::ClaudeToken => crate::claude_connection::token().map(|()| 0),
        Action::Mode { mode } => (|| {
            let paths = Paths::resolve_cli()?;
            let _lock = paths.lock()?;
            set_mode_at(&paths, matches!(mode, Mode::Separate))?;
            println!("Codex CLI connection updated for new launches.");
            Ok(0)
        })(),
        Action::Launch { provider, args } => launch(*provider, args),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!(
                "{}",
                crate::display::sanitize_untrusted_line(&e.to_string())
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_home_does_not_overlap_desktop_and_mode_defaults_shared() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::cli_at(tmp.path());
        assert!(!paths.home.starts_with(tmp.path().join(".codex")));
        assert!(!separate_at(&paths).unwrap());
        assert!(set_mode_at(&paths, true).is_err());
        assert!(!tmp.path().join(".codex").exists());
        set_mode_at(&paths, false).unwrap();
        assert!(!separate_at(&paths).unwrap());
    }
    #[test]
    fn running_session_blocks_account_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let lease = session_file(tmp.path()).unwrap();
        FileExt::try_lock_shared(&lease).unwrap();
        assert!(exclusive(tmp.path()).is_err());
        drop(lease);
        assert!(exclusive(tmp.path()).is_ok());
    }
}
