//! Independent Codex Desktop profiles, sharing the official app's local data.
pub(crate) mod desktop;
pub(crate) mod rpc;
pub(crate) mod store;
pub(crate) mod switch;

use crate::Result;
use serde_json::{Value, json};
use store::{Paths, error};

#[derive(clap::Subcommand, Debug, Clone)]
pub enum Action {
    /// List saved profiles and the current login, without exposing credentials.
    Status {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        cli: bool,
    },
    /// Save the account currently signed into Codex Desktop.
    Save {
        label: String,
        #[arg(long)]
        cli: bool,
    },
    /// Sign into an additional account in an isolated official Codex login.
    Add {
        label: String,
        #[arg(long)]
        cli: bool,
    },
    /// Switch only auth.json, keeping shared chats, projects and routines.
    Switch {
        label: String,
        /// Validate the switch without quitting Codex or changing credentials.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        cli: bool,
        /// Confirm that Codex Desktop may quit and reopen.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Restore the previous login after an interrupted switch.
    Recover {
        #[arg(long)]
        cli: bool,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Remove an inactive saved profile; does not delete any local chats.
    Remove {
        label: String,
        #[arg(long)]
        cli: bool,
    },
}

pub fn status_default() -> Value {
    let paths = Paths::resolve();
    if let Ok(paths) = &paths
        && let Ok(status) = store::status(paths)
    {
        return status;
    }
    json!({
        "available": cfg!(target_os = "macos"),
        "recovery_required": paths.as_ref().is_ok_and(|p| p.pending().exists()),
        "error": "Could not read Codex profiles. Run ai-usagebar codex-account status for details."
    })
}

pub async fn run(action: &Action) -> i32 {
    match execute(action).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!(
                "{}",
                crate::display::sanitize_untrusted_line(&e.to_string())
            );
            1
        }
    }
}

async fn execute(action: &Action) -> Result<()> {
    let cli = match action {
        Action::Status { cli, .. }
        | Action::Save { cli, .. }
        | Action::Add { cli, .. }
        | Action::Switch { cli, .. }
        | Action::Recover { cli, .. }
        | Action::Remove { cli, .. } => *cli,
    };
    let paths = if cli {
        Paths::resolve_cli()?
    } else {
        Paths::resolve()?
    };
    if let Action::Status { json, .. } = action {
        let status = store::status(&paths)?;
        if *json {
            println!("{status}");
        } else {
            println!(
                "Active Codex profile: {}",
                crate::display::sanitize_untrusted_line(
                    status["active_label"].as_str().unwrap_or("not saved")
                )
            );
            for p in store::profiles(&paths)? {
                println!("  {}", crate::display::sanitize_untrusted_line(&p.label));
            }
            if let Some(problems) = status["profile_errors"].as_array() {
                for p in problems {
                    println!(
                        "  {}: {}",
                        p["label"].as_str().unwrap_or("Invalid profile"),
                        p["error"].as_str().unwrap_or("Restore profile metadata.")
                    );
                }
            }
            if paths.pending().exists() {
                println!("Recovery required: ai-usagebar codex-account recover --yes");
            }
        }
        return Ok(());
    }
    if !cfg!(target_os = "macos") {
        return Err(error("Codex Desktop switching is available on macOS."));
    }
    let _lock = paths.lock()?;
    let _session_guard = crate::cli_session::exclusive(&paths.root)?;
    let mut runtime = switch::Surface {
        cli,
        home: paths.home.clone(),
    };
    if let Action::Recover { yes, .. } = action {
        if !yes {
            return Err(error(
                "Recovery restarts Codex Desktop. Finish active tasks, then run again with --yes.",
            ));
        }
        switch::recover(&paths, &mut runtime).await?;
        println!("Previous Codex login restored.");
        return Ok(());
    }
    paths.ensure_ready()?;
    match action {
        Action::Save { label, .. } => {
            let _credential_lock = crate::cache::acquire_lock_async(
                &crate::openai::creds::lock_path(&paths.auth()),
                std::time::Duration::from_secs(45),
            )
            .await?;
            store::save_profile(&paths, label, &store::read_auth(&paths.auth())?)?;
            println!("Current Codex login saved. No app restart was needed.");
        }
        Action::Add { label, .. } => {
            if paths.profile(label)?.exists() {
                return Err(error("That profile label is already in use."));
            }
            // No copy of the live config, sessions, auth, plugins or routines.
            let login = tempfile::Builder::new()
                .prefix(".login-")
                .tempdir_in(&paths.root)
                .map_err(|e| crate::AppError::io_at(&paths.root, e))?;
            let mut session = rpc::Session::start(&rpc::executable()?, login.path()).await?;
            let result = async {
                session.login().await?;
                let bytes = store::read_auth(&login.path().join("auth.json"))?;
                let identity = store::Identity::from_auth(&bytes)?;
                session.verify(login.path(), &identity).await?;
                // Read again in case the official client rotated a token.
                store::save_profile(
                    &paths,
                    label,
                    &store::read_auth(&login.path().join("auth.json"))?,
                )
            }
            .await;
            session.stop().await;
            result?;
            if cli && !paths.auth().exists() {
                store::initialize_cli_home(&paths, label)?;
                crate::cli_session::set_mode_at(&paths, true)?;
            }
            println!("Codex profile saved. Select it to switch accounts.");
        }
        Action::Switch {
            label,
            dry_run,
            yes,
            ..
        } => {
            if cli && !paths.auth().exists() {
                let profile = store::profile(&paths, label)?;
                if *dry_run {
                    println!("First CLI login can be activated. Desktop is unchanged.");
                    return Ok(());
                }
                if !yes {
                    return Err(error("Run again with --yes to select this CLI account."));
                }
                rpc::verify(&paths.profile(label)?, &profile.identity).await?;
                store::initialize_cli_home(&paths, label)?;
                crate::cli_session::set_mode_at(&paths, true)?;
                println!("First CLI account selected. Open the CLI to continue.");
                return Ok(());
            }
            switch::preflight(&paths, label)?;
            if *dry_run {
                println!(
                    "Switch validated. {}",
                    if cli {
                        "Only the CLI login would change."
                    } else {
                        "Only auth.json would change; Codex Desktop would restart."
                    }
                );
            } else {
                if !yes {
                    return Err(error(
                        "Switching restarts Codex Desktop. Finish active tasks, then run again with --yes.",
                    ));
                }
                switch::activate(&paths, label, &mut runtime).await?;
                if cli {
                    crate::cli_session::set_mode_at(&paths, true)?;
                }
                println!("Codex profile activated.");
            }
        }
        Action::Remove { label, .. } => {
            let profile = store::profile(&paths, label)?;
            if paths.auth().exists()
                && store::Identity::from_auth(&store::read_auth(&paths.auth())?)?
                    .same_account(&profile.identity)
            {
                return Err(error(
                    "Switch to another account before removing the active profile.",
                ));
            }
            let dir = paths.profile(label)?;
            std::fs::remove_dir_all(&dir).map_err(|e| crate::AppError::io_at(&dir, e))?;
            println!("Inactive Codex profile removed. Local chats were kept.");
        }
        Action::Status { .. } | Action::Recover { .. } => unreachable!(),
    }
    Ok(())
}
