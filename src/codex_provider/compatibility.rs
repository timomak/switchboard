//! Conservative admission guard for saved tasks bound to a provider ID.
//! No database/rollout mutation, credential retention, or model migration.
use super::*;
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read},
};

const INSPECT: &str = "Could not inspect saved Codex task provider bindings. No connection change was applied; preserve the task stores and resolve their readability/schema before retrying.";
const BOUND: &str = "Saved Codex tasks still reference a provider this change would remove or rebind. No connection change was applied. Keep the current connection until a reviewed provider-retention or per-task migration plan is available; task identities and provider values are omitted for privacy.";

fn references(home: &Path) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for entry in std::fs::read_dir(home).map_err(|_| error(INSPECT))? {
        let entry = entry.map_err(|_| error(INSPECT))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Official versioned state databases. Never choose an arbitrary SQLite
        // file, ignore WAL, or infer a migration between schema versions.
        if name
            .strip_prefix("state_")
            .and_then(|s| s.strip_suffix(".sqlite"))
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            if !entry.file_type().map_err(|_| error(INSPECT))?.is_file() {
                return Err(error(INSPECT));
            }
            let db = rusqlite::Connection::open_with_flags(
                entry.path(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|_| error(INSPECT))?;
            db.busy_timeout(std::time::Duration::from_secs(2))
                .map_err(|_| error(INSPECT))?;
            let mut query = db
                .prepare("SELECT DISTINCT model_provider FROM threads")
                .map_err(|_| error(INSPECT))?;
            let rows = query
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|_| error(INSPECT))?;
            for row in rows {
                let id = row.map_err(|_| error(INSPECT))?;
                if id.trim().is_empty() {
                    return Err(error(INSPECT));
                }
                ids.insert(id);
            }
        }
    }
    for name in ["sessions", "archived_sessions"] {
        let dir = home.join(name);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => rollouts(&dir, &mut ids)?,
            Ok(_) => return Err(error(INSPECT)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(error(INSPECT)),
        }
    }
    Ok(ids)
}
fn rollouts(dir: &Path, ids: &mut BTreeSet<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|_| error(INSPECT))? {
        let entry = entry.map_err(|_| error(INSPECT))?;
        let kind = entry.file_type().map_err(|_| error(INSPECT))?;
        if kind.is_symlink() {
            return Err(error(INSPECT));
        }
        if kind.is_dir() {
            rollouts(&entry.path(), ids)?;
        } else if entry.path().extension().is_some_and(|x| x == "jsonl") {
            if !kind.is_file() {
                return Err(error(INSPECT));
            }
            // Parse only the first session metadata line; never parse conversation bodies.
            let file = std::fs::File::open(entry.path()).map_err(|_| error(INSPECT))?;
            let mut first = String::new();
            BufReader::new(file.take(1_048_577))
                .read_line(&mut first)
                .map_err(|_| error(INSPECT))?;
            if first.len() > 1_048_576 {
                return Err(error(INSPECT));
            }
            let meta: Value = serde_json::from_str(&first).map_err(|_| error(INSPECT))?;
            if meta["type"] != "session_meta" {
                return Err(error(INSPECT));
            }
            let id = meta
                .pointer("/payload/model_provider")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| error(INSPECT))?;
            ids.insert(id.into());
        }
    }
    Ok(())
}
pub(super) fn ensure_transition(
    home: &Path,
    before: Option<&str>,
    after: Option<&str>,
    environment_changed: bool,
) -> Result<()> {
    let parse = |s: Option<&str>| {
        toml::from_str::<toml::Value>(s.unwrap_or("")).map_err(|_| {
            error("Cannot compare Codex provider definitions; configuration is invalid.")
        })
    };
    let before = parse(before)?;
    let after = parse(after)?;
    let old = before.get("model_providers");
    let new = after.get("model_providers");
    if old == new && !environment_changed {
        return Ok(());
    }
    for id in references(home)? {
        let previous = old.and_then(|p| p.get(&id));
        let next = new.and_then(|p| p.get(&id));
        if previous != next || ((previous.is_some() || next.is_some()) && environment_changed) {
            return Err(error(BOUND));
        }
    }
    Ok(())
}
