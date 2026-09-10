//! Graceful lifecycle port of DesktopController.swift. Never force-kill Codex:
//! its active-task dialog and session writes must finish before auth changes.
use super::store::error;
use crate::Result;
use std::{process::Stdio, time::Duration};
use tokio::{
    process::Command,
    time::{Instant, sleep, timeout},
};

async fn script(code: &str) -> Result<String> {
    let mut command = Command::new("/usr/bin/osascript");
    command
        .args(["-e", code])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let result=timeout(Duration::from_secs(30),command.output()).await
        .map_err(|_|error("Codex is still quitting. Finish active tasks and try again; its login was not changed."))?
        .map_err(|_|error("Could not contact Codex Desktop."))?;
    if !result.status.success() {
        return Err(error(
            "Codex Desktop rejected the quit request. Finish active tasks and try again.",
        ));
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().into())
}
async fn running() -> Result<bool> {
    match script("application id \"com.openai.codex\" is running")
        .await?
        .as_str()
    {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(error(
            "Could not determine whether Codex Desktop is running.",
        )),
    }
}
pub async fn quit() -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    if !running().await? {
        return Ok(());
    }
    script("tell application id \"com.openai.codex\" to quit").await?;
    while running().await? {
        if Instant::now() >= deadline {
            return Err(error(
                "Codex did not finish quitting. Finish active tasks and try again; its login was not changed.",
            ));
        }
        sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}
pub async fn reopen() -> Result<()> {
    let result = Command::new("/usr/bin/open")
        .args(["-b", "com.openai.codex"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|_| error("Open Codex Desktop manually to continue."))?;
    if !result.success() {
        return Err(error("Open Codex Desktop manually to continue."));
    }
    Ok(())
}
