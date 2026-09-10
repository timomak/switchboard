//! Local Codex profile storage. Adapted from Codex Account Switcher for Mac's
//! AccountStore.swift (MIT; attribution in THIRD_PARTY_NOTICES.md).
//! Only auth.json moves: the official app keeps one shared CODEX_HOME.
use crate::{AppError, Result, cache};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn error(message: &str) -> AppError {
    AppError::Other(message.into())
}

#[derive(Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub home: PathBuf,
}
impl Paths {
    pub fn cli_at(home: &Path) -> Self {
        let root = home.join(".claude-acc/codex-cli");
        Self {
            home: root.join("home"),
            root,
        }
    }
    pub fn resolve_cli() -> Result<Self> {
        Ok(Self::cli_at(&cache::home_dir()?))
    }

    pub fn resolve() -> Result<Self> {
        let home = cache::home_dir()?;
        Ok(Self {
            root: home.join(".claude-acc/codex-profiles"),
            home: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex")),
        })
    }
    pub fn auth(&self) -> PathBuf {
        self.home.join("auth.json")
    }
    pub fn profile(&self, label: &str) -> Result<PathBuf> {
        crate::config::validate_account_label(label).map_err(|_| {
            error(
                "Use a non-empty Codex profile name without path separators or control characters.",
            )
        })?;
        Ok(self.root.join("profiles").join(label))
    }
    pub fn pending(&self) -> PathBuf {
        self.root.join("pending-switch.json")
    }
    pub fn ensure_ready(&self) -> Result<()> {
        if self.pending().exists() {
            return Err(error(
                "An interrupted Codex switch needs recovery. Run `ai-usagebar codex-account recover` first.",
            ));
        }
        self.ensure_file_store()
    }
    pub fn ensure_file_store(&self) -> Result<()> {
        // A file swap cannot update Desktop when it is configured to use Keychain.
        match std::fs::read_to_string(self.home.join("config.toml")) {
            Ok(s) => {
                let config: toml::Value = toml::from_str(&s)
                    .map_err(|_| error("Codex config.toml is invalid; no credentials changed."))?;
                if let Some(mode) = config
                    .get("cli_auth_credentials_store")
                    .and_then(|v| v.as_str())
                    && mode != "file"
                {
                    return Err(error(
                        "Codex switching requires file-backed authentication. This Codex home uses another credential store; its settings and credentials were not changed.",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AppError::io_at(self.home.join("config.toml"), e)),
        }
        Ok(())
    }
    pub fn lock(&self) -> Result<cache::LockGuard> {
        private_dir(&self.root)?;
        cache::acquire_lock(
            &self.root.join("operation.lock"),
            std::time::Duration::from_secs(2),
        )
    }
}

pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(|e| AppError::io_at(path, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| AppError::io_at(path, e))?;
    }
    Ok(())
}

// Metadata only. Never derive Debug for a structure containing credentials.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub account_id: String,
    pub user_id: String,
    pub email: Option<String>,
}
impl Identity {
    pub fn same_account(&self, other: &Self) -> bool {
        self.account_id == other.account_id && self.user_id == other.user_id
    }
    pub fn from_auth(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|_| error("Codex auth.json is not valid JSON."))?;
        let token = value
            .pointer("/tokens/id_token")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                error("Save a ChatGPT login first; API-key profiles are not supported.")
            })?;
        let claims = crate::jwt::claims(token)
            .ok_or_else(|| error("Codex login has no readable identity."))?;
        let auth = &claims["https://api.openai.com/auth"];
        let account_id = value
            .pointer("/tokens/account_id")
            .and_then(Value::as_str)
            .or_else(|| auth["chatgpt_account_id"].as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Codex login has no account ID."))?;
        let user_id = auth["chatgpt_user_id"]
            .as_str()
            .or_else(|| claims["sub"].as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Codex login has no user ID."))?;
        for name in ["access_token", "refresh_token"] {
            if value["tokens"][name].as_str().is_none_or(str::is_empty) {
                return Err(error("Codex login is incomplete."));
            }
        }
        Ok(Self {
            account_id: account_id.into(),
            user_id: user_id.into(),
            email: claims["email"].as_str().map(str::to_string),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub label: String,
    pub identity: Identity,
}

pub fn read_auth(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| AppError::io_at(path, e))
}

fn profile_listing(paths: &Paths) -> Result<(Vec<Profile>, Vec<Value>)> {
    let entries = match std::fs::read_dir(paths.root.join("profiles")) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((vec![], vec![])),
        Err(e) => return Err(AppError::io_at(paths.root.join("profiles"), e)),
    };
    let mut result = vec![];
    let mut problems = vec![];
    for entry in entries {
        let entry = entry.map_err(|_| error("Could not list Codex profiles."))?;
        if !entry
            .file_type()
            .map_err(|_| error("Could not inspect Codex profile."))?
            .is_dir()
        {
            continue;
        }
        let label = entry.file_name().to_string_lossy().into_owned();
        match read_profile(paths, &label) {
            Ok(p) => result.push(p),
            Err(_) => problems.push(json!({"label": crate::display::sanitize_untrusted_line(&label),
                "error": "Saved profile is unreadable or invalid. Restore its profile.json from a trusted backup before selecting it."})),
        }
    }
    result.sort_by(|a, b| a.label.cmp(&b.label));
    problems.sort_by_key(|p| p["label"].as_str().unwrap_or_default().to_owned());
    Ok((result, problems))
}

pub fn profiles(paths: &Paths) -> Result<Vec<Profile>> {
    Ok(profile_listing(paths)?.0)
}

fn read_profile(paths: &Paths, label: &str) -> Result<Profile> {
    let meta = paths.profile(label)?.join("profile.json");
    let mut p: Profile = serde_json::from_slice(&read_auth(&meta)?).map_err(|_| {
        error("Saved Codex profile is invalid; restore its profile.json before selecting it.")
    })?;
    p.label = label.into();
    Ok(p)
}

pub fn profile(paths: &Paths, label: &str) -> Result<Profile> {
    read_profile(paths, label)
}

/// Complete a first CLI login (or resume setup after the saved profile was
/// committed). Never overwrite an existing live login, including an unmanaged one.
pub fn initialize_cli_home(paths: &Paths, label: &str) -> Result<()> {
    paths.ensure_ready()?;
    if paths.auth().exists() {
        return Err(error(
            "A CLI login already exists. Use account switching instead.",
        ));
    }
    let profile = profile(paths, label)?;
    let bytes = read_auth(&paths.profile(label)?.join("auth.json"))?;
    if !Identity::from_auth(&bytes)?.same_account(&profile.identity) {
        return Err(error(
            "CLI profile identity does not match its credentials.",
        ));
    }
    private_dir(&paths.home)?;
    cache::atomic_write(&paths.auth(), &bytes)
}

pub fn save_profile(paths: &Paths, label: &str, bytes: &[u8]) -> Result<()> {
    let destination = paths.profile(label)?;
    let identity = Identity::from_auth(bytes)?;
    if destination.exists() {
        return Err(error("That profile label is already in use."));
    }
    let (saved, problems) = profile_listing(paths)?;
    if !problems.is_empty() {
        return Err(error(
            "Restore invalid Codex profile metadata before saving another account so duplicate identities can be checked.",
        ));
    }
    if saved.iter().any(|p| p.identity.same_account(&identity)) {
        return Err(error(
            "This Codex account is already saved. Select its existing profile.",
        ));
    }
    let parent = paths.root.join("profiles");
    private_dir(&parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".new-")
        .tempdir_in(&paths.root)
        .map_err(|e| AppError::io_at(&paths.root, e))?;
    cache::atomic_write(&stage.path().join("auth.json"), bytes)?;
    cache::atomic_write(
        &stage.path().join("profile.json"),
        &serde_json::to_vec_pretty(&Profile {
            label: label.into(),
            identity,
        })?,
    )?;
    std::fs::rename(stage.path(), &destination).map_err(|e| AppError::io_at(&destination, e))?;
    Ok(())
}

pub fn status(paths: &Paths) -> Result<Value> {
    let (saved, problems) = profile_listing(paths)?;
    let live = match read_auth(&paths.auth()) {
        Ok(b) => Some(Identity::from_auth(&b)?),
        Err(_) if !paths.auth().exists() => None,
        Err(e) => return Err(e),
    };
    let active = live
        .as_ref()
        .and_then(|id| saved.iter().find(|p| p.identity.same_account(id)));
    let rows: Vec<_>=saved.iter().map(|p|json!({"label":p.label,"email":p.identity.email,"active":active.is_some_and(|a|a.label==p.label)})).collect();
    Ok(
        json!({"available":cfg!(target_os="macos"),"active_label":active.map(|p|&p.label),"has_login":live.is_some(),"profiles":rows,"profile_errors":problems,"recovery_required":paths.pending().exists()}),
    )
}
