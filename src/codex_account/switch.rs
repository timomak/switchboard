//! Transactional auth-file switching, adapted from SwitchService.swift.
use super::{
    desktop, rpc,
    store::{self, Identity, Paths, Profile, error, read_auth},
};
use crate::{Result, cache};
use serde::{Deserialize, Serialize};

// Deliberately no Debug: previous_auth contains credentials.
#[derive(Serialize, Deserialize)]
struct Pending {
    previous_auth: String,
    previous: Identity,
    target: Identity,
}

pub(crate) trait Runtime {
    async fn quit(&mut self) -> Result<()>;
    async fn verify(&mut self, home: &std::path::Path, identity: &Identity) -> Result<()>;
    async fn reopen(&mut self) -> Result<()>;
}
pub(super) struct Desktop;
impl Runtime for Desktop {
    async fn quit(&mut self) -> Result<()> {
        rpc::executable()?;
        desktop::quit().await
    }
    async fn verify(&mut self, home: &std::path::Path, identity: &Identity) -> Result<()> {
        rpc::verify(home, identity).await
    }
    async fn reopen(&mut self) -> Result<()> {
        desktop::reopen().await
    }
}

pub(super) struct Surface {
    pub cli: bool,
    pub home: std::path::PathBuf,
}
impl Runtime for Surface {
    async fn quit(&mut self) -> Result<()> {
        if self.cli {
            crate::cli_session::stop_daemon(&self.home).await
        } else {
            Desktop.quit().await
        }
    }
    async fn verify(&mut self, home: &std::path::Path, identity: &Identity) -> Result<()> {
        rpc::verify(home, identity).await
    }
    async fn reopen(&mut self) -> Result<()> {
        if self.cli {
            Ok(())
        } else {
            Desktop.reopen().await
        }
    }
}

fn outgoing(paths: &Paths) -> Result<(Profile, Vec<u8>)> {
    let bytes = read_auth(&paths.auth())?;
    let identity = Identity::from_auth(&bytes)?;
    let profile = store::profiles(paths)?
        .into_iter()
        .find(|p| p.identity.same_account(&identity))
        .ok_or_else(|| {
            error("Save the current Codex login before switching: ai-usagebar codex-account save LABEL.")
        })?;
    Ok((profile, bytes))
}
fn incoming(paths: &Paths, label: &str) -> Result<(Profile, Vec<u8>)> {
    let profile = store::profile(paths, label)?;
    let bytes = read_auth(&paths.profile(label)?.join("auth.json"))?;
    if !Identity::from_auth(&bytes)?.same_account(&profile.identity) {
        return Err(error(
            "Saved Codex credentials do not match the selected profile. No login was changed.",
        ));
    }
    Ok((profile, bytes))
}
pub(crate) fn preflight(paths: &Paths, label: &str) -> Result<()> {
    paths.ensure_ready()?;
    incoming(paths, label)?;
    outgoing(paths)?;
    Ok(())
}
fn clear_pending(paths: &Paths) -> Result<()> {
    std::fs::remove_file(paths.pending()).map_err(|e| crate::AppError::io_at(paths.pending(), e))
}
fn rollback(paths: &Paths, pending: &Pending) -> Result<()> {
    cache::atomic_write(&paths.auth(), pending.previous_auth.as_bytes())?;
    clear_pending(paths)
}

pub(crate) async fn activate(paths: &Paths, label: &str, runtime: &mut impl Runtime) -> Result<()> {
    let _credential_lock = crate::cache::acquire_lock_async(
        &crate::openai::creds::lock_path(&paths.auth()),
        std::time::Duration::from_secs(45),
    )
    .await?;
    preflight(paths, label)?;
    let (target, target_auth) = incoming(paths, label)?;
    if outgoing(paths)?.0.identity.same_account(&target.identity) {
        return Ok(());
    }
    runtime.quit().await?;
    // The app can refresh credentials while quitting. Read the outgoing login
    // only after it exits; never overwrite a saved account with a different one.
    let result = async {
        let (previous, bytes) = outgoing(paths)?;
        cache::atomic_write(&paths.profile(&previous.label)?.join("auth.json"), &bytes)?;
        let pending = Pending {
            previous_auth: String::from_utf8(bytes)
                .map_err(|_| error("Codex login is not UTF-8."))?,
            previous: previous.identity,
            target: target.identity.clone(),
        };
        cache::atomic_write(&paths.pending(), &serde_json::to_vec(&pending)?)?;
        let activation = async {
            cache::atomic_write(&paths.auth(), &target_auth)?;
            runtime.verify(&paths.home, &target.identity).await?;
            // Keep any token rotation performed by the official helper.
            let verified = read_auth(&paths.auth())?;
            if !Identity::from_auth(&verified)?.same_account(&target.identity) {
                return Err(error("Codex identity changed during verification."));
            }
            cache::atomic_write(&paths.profile(label)?.join("auth.json"), &verified)?;
            clear_pending(paths)
        }
        .await;
        if let Err(cause) = activation {
            if rollback(paths, &pending).is_err() {
                return Err(error(
                    "Switch failed and automatic recovery could not finish. Codex remains closed. \
                     Run ai-usagebar codex-account recover --yes.",
                ));
            }
            return Err(error(&format!(
                "Codex switch failed: {cause}. The previous login was restored."
            )));
        }
        Ok(())
    }
    .await;
    // Do not open an uncertain login.
    if paths.pending().exists() {
        return result;
    }
    let reopened = runtime.reopen().await;
    result?;
    reopened.map_err(|_| {
        error("The Codex profile is active, but the app could not reopen. Open Codex manually.")
    })
}

pub(super) async fn recover(paths: &Paths, runtime: &mut impl Runtime) -> Result<()> {
    let _credential_lock = crate::cache::acquire_lock_async(
        &crate::openai::creds::lock_path(&paths.auth()),
        std::time::Duration::from_secs(45),
    )
    .await?;
    if !paths.pending().exists() {
        return Err(error("There is no interrupted Codex switch to recover."));
    }
    paths.ensure_file_store()?;
    let pending: Pending = serde_json::from_slice(&read_auth(&paths.pending())?)
        .map_err(|_| error("Codex recovery record is invalid; no login was changed."))?;
    if !Identity::from_auth(pending.previous_auth.as_bytes())?.same_account(&pending.previous) {
        return Err(error(
            "Codex recovery credentials do not match their identity.",
        ));
    }
    runtime.quit().await?;
    let result = (|| {
        if paths.auth().exists() {
            let live = Identity::from_auth(&read_auth(&paths.auth())?)?;
            if !live.same_account(&pending.previous) && !live.same_account(&pending.target) {
                return Err(error(
                    "Codex has a different login from the interrupted switch. Recovery refused to overwrite it.",
                ));
            }
        }
        rollback(paths, &pending)
    })();
    if result.is_ok() {
        runtime.reopen().await?;
    }
    result
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
