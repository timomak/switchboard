//! Codex's official app-server sign-in and identity protocol. Ported from
//! CodexClient.swift; no provider proxy, usage API, updater, or alternate UI.
use super::store::{Identity, error, read_auth};
use crate::Result;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{Instant, timeout_at},
};

pub struct Session {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    queued: VecDeque<Value>,
}
const REQUEST: Duration = Duration::from_secs(25);

pub fn executable() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("CODEX_CLI_PATH") {
        let p = PathBuf::from(p);
        if p.is_absolute() && p.is_file() {
            return Ok(p);
        }
        return Err(error("CODEX_CLI_PATH must point to the Codex executable."));
    }
    let mut candidates = vec![
        PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|p| p.join("codex")));
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
    ]);
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| error("Codex executable not found. Install Codex or set CODEX_CLI_PATH."))
}

impl Session {
    pub async fn start(executable: &Path, home: &Path) -> Result<Self> {
        let mut child = Command::new(executable)
            .args([
                "app-server",
                "--stdio",
                "-c",
                "cli_auth_credentials_store=\"file\"",
            ])
            .current_dir(home)
            .env("CODEX_HOME", home)
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY")
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("CODEX_SQLITE_HOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| error("Could not start the Codex app-server."))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| error("Codex app-server input unavailable."))?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| error("Codex app-server output unavailable."))?,
        );
        let mut s = Self {
            child,
            input,
            output,
            queued: VecDeque::new(),
        };
        s.request("initialize",0,json!({"clientInfo":{"name":"ai_usagebar","title":"AI Usage Bar","version":env!("CARGO_PKG_VERSION")}})).await?;
        s.send(json!({"method":"initialized","params":{}})).await?;
        Ok(s)
    }
    async fn send(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .await
            .map_err(|_| error("Codex app-server connection closed."))
    }
    async fn receive(
        &mut self,
        matches: impl Fn(&Value) -> bool,
        duration: Duration,
    ) -> Result<Value> {
        if let Some(i) = self.queued.iter().position(&matches) {
            return Ok(self.queued.remove(i).unwrap());
        }
        let deadline = Instant::now() + duration;
        loop {
            let mut line = Vec::new();
            // Read bounded chunks; an unexpected subprocess cannot grow memory indefinitely.
            loop {
                let chunk = timeout_at(deadline, self.output.fill_buf())
                    .await
                    .map_err(|_| error("Codex did not respond in time; no sign-in was saved."))?
                    .map_err(|_| error("Could not read Codex app-server response."))?;
                if chunk.is_empty() {
                    return Err(error(
                        "Codex app-server exited before completing the operation.",
                    ));
                }
                let end = chunk.iter().position(|b| *b == b'\n');
                let count = end.map_or(chunk.len(), |i| i + 1);
                if line.len() + count > 1_048_576 {
                    return Err(error("Codex app-server response was too large."));
                }
                line.extend_from_slice(&chunk[..count]);
                self.output.consume(count);
                if end.is_some() {
                    break;
                }
            }
            let message: Value = serde_json::from_slice(&line)
                .map_err(|_| error("Invalid Codex app-server response."))?;
            if matches(&message) {
                return Ok(message);
            }
            if message.get("method").and_then(Value::as_str) == Some("account/login/completed") {
                if self.queued.len() >= 8 {
                    return Err(error("Unexpected Codex login notifications."));
                }
                self.queued.push_back(message);
            }
        }
    }
    pub async fn request(&mut self, method: &str, id: u64, params: Value) -> Result<Value> {
        self.send(json!({"id":id,"method":method,"params":params}))
            .await?;
        let response = self
            .receive(|v| v["id"].as_u64() == Some(id), REQUEST)
            .await?;
        if response.get("error").is_some() {
            return Err(error(
                "Codex rejected the account operation. Sign in again or update Codex.",
            ));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| error("Codex response had no result."))
    }
    pub async fn login(&mut self) -> Result<()> {
        let start = self
            .request(
                "account/login/start",
                1,
                json!({"type":"chatgpt","useHostedLoginSuccessPage":true,"appBrand":"codex"}),
            )
            .await?;
        let url = start["authUrl"]
            .as_str()
            .ok_or_else(|| error("Codex did not return a sign-in URL."))?;
        let url = reqwest::Url::parse(url)
            .map_err(|_| error("Codex returned an invalid sign-in URL."))?;
        if url.scheme() != "https" || url.host_str() != Some("auth.openai.com") {
            return Err(error("Codex returned an unexpected sign-in destination."));
        }
        let opened = Command::new("/usr/bin/open")
            .arg(url.as_str())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|_| error("Could not open the Codex sign-in page."))?;
        if !opened.success() {
            return Err(error("Could not open the Codex sign-in page."));
        }
        println!("Complete sign-in in your browser. Your running Codex account stays unchanged.");
        let login_id = start["loginId"].clone();
        let result = self
            .receive(
                |v| v["method"] == "account/login/completed" && v["params"]["loginId"] == login_id,
                Duration::from_secs(600),
            )
            .await?;
        if result["params"]["success"] != true {
            return Err(error("Codex sign-in was cancelled or failed."));
        }
        Ok(())
    }
    pub async fn verify(&mut self, home: &Path, expected: &Identity) -> Result<()> {
        let result = self
            .request("account/read", 2, json!({"refreshToken":false}))
            .await?;
        let account = &result["account"];
        if account.is_null() || account["type"] != "chatgpt" {
            return Err(error("Codex could not confirm a ChatGPT login."));
        }
        let actual = Identity::from_auth(&read_auth(&home.join("auth.json"))?)?;
        if !actual.same_account(expected) {
            return Err(error(
                "Codex reported a different account; the switch was rolled back.",
            ));
        }
        if let Some(email) = account["email"].as_str()
            && actual
                .email
                .as_ref()
                .is_some_and(|e| !e.eq_ignore_ascii_case(email))
        {
            return Err(error(
                "Codex account identity did not match its saved login.",
            ));
        }
        for key in ["accountId", "accountID", "chatgptAccountId", "id"] {
            if let Some(id) = account[key].as_str()
                && id != expected.account_id
            {
                return Err(error(
                    "Codex workspace identity did not match the selected profile.",
                ));
            }
        }
        Ok(())
    }
    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

pub async fn verify(home: &Path, expected: &Identity) -> Result<()> {
    let mut session = Session::start(&executable()?, home).await?;
    let result = session.verify(home, expected).await;
    session.stop().await;
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn notification_before_response_is_retained_and_subprocess_is_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("fake-codex");
        std::fs::write(&exe,"#!/bin/sh\nread first\nprintf '%s\\n' '{\"method\":\"account/login/completed\",\"params\":{\"success\":true,\"loginId\":\"a\"}}' '{\"id\":0,\"result\":{}}'\nread second\nread third\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut s = Session::start(&exe, dir.path()).await.unwrap();
        let v = s
            .receive(
                |v| v["method"] == "account/login/completed",
                Duration::from_millis(30),
            )
            .await
            .unwrap();
        assert_eq!(v["params"]["success"], true);
        s.stop().await;
    }
}

#[cfg(all(test, unix))]
mod identity_tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn official_account_response_must_match_the_saved_login() {
        let dir = tempfile::tempdir().unwrap();
        let claims = json!({"sub":"fixture-user","email":"fixture@example.test"});
        let auth = json!({"tokens":{"id_token":format!("header.{}.sig",URL_SAFE_NO_PAD.encode(claims.to_string())),"account_id":"fixture-org","access_token":"test","refresh_token":"test"}});
        let bytes = serde_json::to_vec(&auth).unwrap();
        crate::cache::atomic_write(&dir.path().join("auth.json"), &bytes).unwrap();
        let expected = Identity::from_auth(&bytes).unwrap();
        for (email, succeeds) in [
            ("fixture@example.test", true),
            ("different@example.test", false),
        ] {
            let exe = dir.path().join("fake-codex");
            let result = json!({"id":2,"result":{"account":{"type":"chatgpt","email":email}}});
            std::fs::write(&exe,format!("#!/bin/sh\nread first\nprintf '%s\\n' '{{\"id\":0,\"result\":{{}}}}'\nread second\nread third\nprintf '%s\\n' '{result}'\nread fourth\n")).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
            let mut session = Session::start(&exe, dir.path()).await.unwrap();
            assert_eq!(
                session.verify(dir.path(), &expected).await.is_ok(),
                succeeds
            );
            session.stop().await;
        }
    }
}
