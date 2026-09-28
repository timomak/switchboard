//! Native Cowork session transport. The Desktop loader reads an index and the
//! CLI transcript tree, not a rendered chat export. Keep those together, leave
//! authentication and runtime state behind, and preserve account ownership.
//!
//! The layout was checked against the installed Claude Desktop application:
//! account/organisation and session directories can use full or short IDs;
//! `agent` sessions live one directory deeper; transcript lookup scans the
//! session's `.claude/projects` tree by `cliSessionId`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::NativeSession;
use crate::error::{AppError, Result};

const STORE: &str = "local-agent-mode-sessions";
const ORIGIN: &str = ".switchboard-origin.json";
const BASELINE: &str = ".switchboard-baseline.json";
const SPACE_BASELINE: &str = ".switchboard-space-baseline.json";
const STAGE: &str = ".switchboard-cowork-transaction-";
const HOME: &str = "__SWITCHBOARD_HOME__";
const BODY: &str = "__SWITCHBOARD_SESSION__";
const SCOPE: &str = "__SWITCHBOARD_SCOPE__";
const DATA: &str = "__SWITCHBOARD_CLAUDE_DATA__";
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SESSION_BYTES: usize = 320 * 1024 * 1024;
const MAX_FILES: usize = 50_000;
const MAX_DEPTH: usize = 64;
const MAX_CAPTURE_BYTES: usize = 1024 * 1024 * 1024;
const LOCAL_INDEX_FIELDS: &[&str] = &[
    "remoteMcpServersConfig",
    "artifactHostGrant",
    "otelConfig",
    "orgOtlpContentCapture",
    "outboundCCRRemoteId",
    "pendingNotifications",
    "pendingStartMessages",
    "chromeTabGroupId",
    "pluginInstallPaths",
    "cuSelectedDisplayId",
    "cuLastScreenshotDims",
    "globalMemoryBaselineHash",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Owner {
    account: String,
    organization: String,
    #[serde(default)]
    source_home: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct File {
    data: String,
    #[serde(default)]
    executable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Space {
    record: Value,
    files: BTreeMap<String, File>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Payload {
    version: u32,
    owner: Owner,
    index: Value,
    files: BTreeMap<String, File>,
    #[serde(default)]
    space: Option<Space>,
}

#[derive(Clone)]
struct Location {
    scope: PathBuf,
    index: PathBuf,
    body: PathBuf,
    owner: Owner,
}

fn fail(message: &str) -> AppError {
    AppError::Other(format!("Cowork chat sync: {message}"))
}

fn digest(value: &impl Serialize) -> Result<String> {
    struct Sink(Sha256);
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink(Sha256::new());
    serde_json::to_writer(&mut sink, value)?;
    Ok(sink
        .0
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn fork_id(seed: &str) -> String {
    let bytes = Sha256::digest(seed.as_bytes());
    let mut id = [0; 16];
    id.copy_from_slice(&bytes[..16]);
    id[6] = (id[6] & 15) | 0x40;
    id[8] = (id[8] & 63) | 0x80;
    uuid::Uuid::from_bytes(id).to_string()
}

fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn session_id(value: &str) -> bool {
    value.starts_with("local_") && component(value) && value.len() > 6
}

fn relative(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(fail("unsafe relative path in native session package"));
    }
    Ok(path.to_path_buf())
}

/// Do not follow a link anywhere underneath the explicitly supplied root.
fn check_path(root: &Path, path: &Path) -> Result<()> {
    let tail = path
        .strip_prefix(root)
        .map_err(|_| fail("native path escapes the Claude data directory"))?;
    let mut current = root.to_path_buf();
    for part in std::iter::once(None).chain(tail.components().map(Some)) {
        if let Some(part) = part {
            if !matches!(part, Component::Normal(_)) {
                return Err(fail("unsafe native path component"));
            }
            current.push(part);
        }
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(fail(
                    "a native session path is a symbolic link; resolve it before syncing",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn children(root: &Path, dir: &Path) -> Result<Vec<PathBuf>> {
    check_path(root, dir)?;
    let mut paths = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .take(MAX_FILES + 1)
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if paths.len() > MAX_FILES {
        return Err(fail("native directory exceeds the supported entry limit"));
    }
    paths.sort();
    Ok(paths)
}

fn scopes(data_dir: &Path) -> Result<Vec<(PathBuf, Owner)>> {
    let mut result = Vec::new();
    for account in children(data_dir, &data_dir.join(STORE))? {
        let Some(account_name) = account.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !component(account_name)
            || matches!(
                account_name,
                "plugin-cache" | "skills-plugin" | "imported-staging"
            )
        {
            continue;
        }
        check_path(data_dir, &account)?;
        if !account.is_dir() {
            continue;
        }
        for organization in children(data_dir, &account)? {
            let Some(org_name) = organization.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !component(org_name) {
                continue;
            }
            check_path(data_dir, &organization)?;
            if organization.is_dir() {
                result.push((
                    organization.clone(),
                    Owner {
                        account: account_name.to_string(),
                        organization: org_name.to_string(),
                        source_home: String::new(),
                    },
                ));
            }
        }
    }
    Ok(result)
}

fn short_id(id: &str) -> Option<&str> {
    let uuid = id.strip_prefix("local_")?;
    uuid::Uuid::parse_str(uuid).ok()?;
    uuid.get(..8)
}

fn body_path(data_dir: &Path, parent: &Path, id: &str) -> Result<PathBuf> {
    let full = parent.join(id);
    check_path(data_dir, &full)?;
    if full.exists() {
        return Ok(full);
    }
    // A short directory is owned by its existing full-ID index. A different
    // incoming UUID with the same eight-character prefix must get a full path.
    if parent.join(format!("{id}.json")).is_file()
        && let Some(short) = short_id(id)
    {
        let short = parent.join(short);
        check_path(data_dir, &short)?;
        if short.is_dir() {
            return Ok(short);
        }
    }
    Ok(full)
}

fn locations(data_dir: &Path) -> Result<Vec<Location>> {
    let mut result = Vec::new();
    for (scope, owner) in scopes(data_dir)? {
        for parent in [scope.clone(), scope.join("agent")] {
            for index in children(data_dir, &parent)? {
                let Some(id) = index.file_stem().and_then(|name| name.to_str()) else {
                    continue;
                };
                if index
                    .extension()
                    .is_none_or(|extension| extension != "json")
                    || !session_id(id)
                {
                    continue;
                }
                check_path(data_dir, &index)?;
                if !index.is_file() {
                    return Err(fail("session index is not a regular file"));
                }
                result.push(Location {
                    scope: scope.clone(),
                    body: body_path(data_dir, &parent, id)?,
                    index,
                    owner: owner.clone(),
                });
            }
        }
    }
    Ok(result)
}

fn json(path: &Path) -> Result<Value> {
    // JSON errors must not echo private source lines or session content.
    serde_json::from_slice(&read_bounded(path, 10 * 1024 * 1024)?)
        .map_err(|_| fail("an index has invalid JSON; the original was left intact"))
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > limit {
        return Err(fail(
            "native file exceeds the supported size limit or is not regular; it was kept locally",
        ));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(fail("native file grew beyond the supported size limit"));
    }
    Ok(bytes)
}

fn replace_path(value: &str, mappings: &[(String, String)]) -> String {
    for (from, to) in mappings {
        if value == from {
            return to.clone();
        }
        if let Some(tail) = value
            .strip_prefix(from)
            .filter(|tail| tail.starts_with('/'))
        {
            return format!("{to}{tail}");
        }
    }
    value.to_string()
}

fn map_tree(value: &mut Value, mappings: &[(String, String)]) {
    match value {
        Value::String(text) => *text = replace_path(text, mappings),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| map_tree(value, mappings)),
        Value::Object(values) => {
            let old = std::mem::take(values);
            for (key, mut value) in old {
                map_tree(&mut value, mappings);
                values.insert(replace_path(&key, mappings), value);
            }
        }
        _ => {}
    }
}

fn map_index(index: &mut Value, mappings: &[(String, String)]) {
    for key in [
        "cwd",
        "userSelectedFolders",
        "userApprovedFileAccessPaths",
        "resolvedFolderKinds",
        "folderMountNames",
        "fsDetectedFiles",
        "projectContexts",
        "readOnlyPluginPaths",
        "midSessionReadOnlyPaths",
        "transcriptFilePath",
    ] {
        if let Some(value) = index.get_mut(key) {
            map_tree(value, mappings);
        }
    }
    // Operational prompts can contain the storage root inline. User messages
    // and native transcript/audit bytes deliberately remain unchanged.
    for key in [
        "systemPrompt",
        "systemPromptRendererAppends",
        "memoryGuidelinesTemplate",
    ] {
        if let Some(value) = index.get_mut(key) {
            map_prompt(value, mappings);
        }
    }
}

fn map_prompt(value: &mut Value, mappings: &[(String, String)]) {
    match value {
        Value::String(text) => {
            for (from, to) in mappings {
                *text = text.replace(from, to);
            }
        }
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| map_prompt(value, mappings)),
        _ => {}
    }
}

fn scrub_index(index: &mut Value) -> Result<()> {
    let values = index
        .as_object_mut()
        .ok_or_else(|| fail("unsupported session index schema"))?;
    // Treat whole secret-bearing settings as local. Splicing an old token into
    // a changed server descriptor could accidentally grant it to another host.
    values.retain(|key, value| !local_index_field(key, value));
    Ok(())
}

fn secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['_', '-'], "");
    matches!(
        key.as_str(),
        "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "apikey"
            | "authorization"
            | "password"
            | "clientsecret"
            | "privatekey"
            | "oauth"
            | "oauthtokencache"
            | "oauthtokencachev2"
    )
}

fn contains_secret(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields
            .iter()
            .any(|(key, value)| secret_key(key) || contains_secret(value)),
        Value::Array(values) => values.iter().any(contains_secret),
        _ => false,
    }
}

fn local_index_field(key: &str, value: &Value) -> bool {
    LOCAL_INDEX_FIELDS.contains(&key) || secret_key(key) || contains_secret(value)
}

fn preserve_local_index(incoming: &mut Value, existing: &Value) -> Result<()> {
    let same_transcript = incoming.get("cliSessionId") == existing.get("cliSessionId");
    let target = incoming
        .as_object_mut()
        .ok_or_else(|| fail("unsupported incoming session index"))?;
    let current = existing
        .as_object()
        .ok_or_else(|| fail("unsupported destination session index"))?;
    for (key, value) in current {
        if local_index_field(key, value)
            && (same_transcript
                || !matches!(
                    key.as_str(),
                    "pendingStartMessages" | "pendingNotifications"
                ))
        {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

fn remove_secrets(value: &mut Value) {
    match value {
        Value::Object(values) => {
            values.retain(|key, _| !secret_key(key));
            values.values_mut().for_each(remove_secrets);
        }
        Value::Array(values) => values.iter_mut().for_each(remove_secrets),
        _ => {}
    }
}

fn allowed_file(path: &Path) -> bool {
    let parts: Vec<_> = path.iter().filter_map(|part| part.to_str()).collect();
    matches!(
        parts.as_slice(),
        ["audit.jsonl" | ".audit-key"]
            | ["outputs" | "uploads" | ".projects", ..]
            | [".claude", "projects" | "tasks" | "plans" | "memory", ..]
            | [".claude", "CLAUDE.md"]
    )
}

fn collect_tree(
    root: &Path,
    dir: &Path,
    base: &Path,
    out: &mut BTreeMap<String, File>,
) -> Result<()> {
    let mut bytes = out.values().map(|file| file.data.len()).sum::<usize>();
    collect_tree_bounded(root, dir, base, out, &mut bytes, 0)
}

fn collect_tree_bounded(
    root: &Path,
    dir: &Path,
    base: &Path,
    out: &mut BTreeMap<String, File>,
    bytes: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(fail("native file tree is too deep; it was kept locally"));
    }
    for path in children(root, dir)? {
        check_path(root, &path)?;
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            collect_tree_bounded(root, &path, base, out, bytes, depth + 1)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(base)
                .map_err(|_| fail("file escaped session directory"))?;
            let name = rel
                .to_str()
                .ok_or_else(|| fail("session contains a non-UTF-8 filename"))?;
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            if out.len() >= MAX_FILES
                || meta.len() > MAX_FILE_BYTES
                || bytes.saturating_add((meta.len() as usize).saturating_mul(4).div_ceil(3))
                    > MAX_SESSION_BYTES * 4 / 3
            {
                return Err(fail(
                    "native session exceeds the 320 MiB or 50000-file sync limit; it was kept locally",
                ));
            }
            let data = STANDARD.encode(read_bounded(&path, MAX_FILE_BYTES)?);
            *bytes = bytes.saturating_add(data.len());
            out.insert(name.to_string(), File { data, executable });
        } else {
            return Err(fail("session data contains an unsupported special file"));
        }
    }
    Ok(())
}

fn capture_files(data_dir: &Path, body: &Path) -> Result<BTreeMap<String, File>> {
    if !body.is_dir() {
        return Err(fail(
            "native session body is missing; cannot export an index alone",
        ));
    }
    let mut files = BTreeMap::new();
    for name in [
        "outputs",
        "uploads",
        ".projects",
        ".claude/projects",
        ".claude/tasks",
        ".claude/plans",
        ".claude/memory",
    ] {
        let dir = body.join(name);
        check_path(data_dir, &dir)?;
        if dir.exists() {
            collect_tree(data_dir, &dir, body, &mut files)?;
        }
    }
    for name in ["audit.jsonl", ".audit-key", ".claude/CLAUDE.md"] {
        let path = body.join(name);
        check_path(data_dir, &path)?;
        if path.exists() {
            if !path.is_file() {
                return Err(fail("native transcript or audit key is not a regular file"));
            }
            files.insert(
                name.into(),
                File {
                    data: STANDARD.encode(read_bounded(&path, MAX_FILE_BYTES)?),
                    executable: false,
                },
            );
        }
    }
    if files.values().map(|file| file.data.len()).sum::<usize>() > MAX_SESSION_BYTES * 4 / 3 {
        return Err(fail(
            "native session exceeds the 320 MiB sync limit; it was kept locally",
        ));
    }
    Ok(files)
}

fn check_transcript(index: &Value, files: &BTreeMap<String, File>) -> Result<()> {
    let id = index
        .get("cliSessionId")
        .and_then(Value::as_str)
        .filter(|id| component(id))
        .ok_or_else(|| fail("native session has no valid CLI transcript ID"))?;
    let expected = format!("{id}.jsonl");
    if !files.keys().any(|path| {
        path.starts_with(".claude/projects/")
            && Path::new(path)
                .file_name()
                .is_some_and(|name| name == expected.as_str())
    }) {
        return Err(fail(
            "native CLI transcript is missing; cannot restore full history",
        ));
    }
    Ok(())
}

fn capture_location(data_dir: &Path, home: &Path, location: &Location) -> Result<NativeSession> {
    let mut index = json(&location.index)?;
    let id = index
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| session_id(id))
        .ok_or_else(|| fail("native session has an invalid identity"))?
        .to_string();
    if location
        .index
        .file_stem()
        .is_none_or(|name| name != id.as_str())
    {
        return Err(fail("native index filename and identity disagree"));
    }
    let files = capture_files(data_dir, &location.body)?;
    check_transcript(&index, &files)?;
    let mappings = vec![
        (location.body.to_string_lossy().into_owned(), BODY.into()),
        (location.scope.to_string_lossy().into_owned(), SCOPE.into()),
        (data_dir.to_string_lossy().into_owned(), DATA.into()),
        (home.to_string_lossy().into_owned(), HOME.into()),
    ];
    scrub_index(&mut index)?;
    map_index(&mut index, &mappings);
    let origin = location.body.join(ORIGIN);
    check_path(data_dir, &origin)?;
    let owner = if origin.exists() {
        serde_json::from_value(json(&origin)?).map_err(|_| fail("invalid sync ownership marker"))?
    } else {
        let mut owner = location.owner.clone();
        owner.source_home = home.to_string_lossy().into_owned();
        owner
    };
    let space = if let Some(id) = index
        .get("spaceId")
        .and_then(Value::as_str)
        .filter(|id| component(id))
    {
        let registry = location.scope.join("spaces.json");
        check_path(data_dir, &registry)?;
        if registry.exists() {
            let registry = json(&registry)?;
            let record = registry
                .get("spaces")
                .and_then(Value::as_array)
                .ok_or_else(|| fail("unsupported Cowork spaces registry"))?
                .iter()
                .find(|record| record.get("id").and_then(Value::as_str) == Some(id))
                .cloned();
            if let Some(mut record) = record {
                remove_secrets(&mut record);
                map_tree(&mut record, &mappings);
                let base = location.scope.join("spaces").join(id);
                let mut files = BTreeMap::new();
                // Space memory is native context. Other space subdirectories
                // contain runtime/plugin configuration, which stays local.
                let memory = base.join("memory");
                if memory.exists() {
                    collect_tree(data_dir, &memory, &base, &mut files)?;
                }
                Some(Space { record, files })
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    Ok(NativeSession {
        id,
        payload: serde_json::to_value(Payload {
            version: 1,
            owner,
            index,
            files,
            space,
        })?,
    })
}

/// Capture every local Cowork scope without accessing cookies, Keychain, app
/// databases, credentials, or user chat text for inspection. Opaque transcript
/// and attachment bytes are the native payload, never rendered/rewritten.
pub fn capture(data_dir: &Path, home: &Path) -> Result<Vec<NativeSession>> {
    if !data_dir.exists() {
        return Ok(Vec::new());
    }
    ensure_no_pending(data_dir)?;
    replica_groups(data_dir, home)?.into_values().map(|replicas| {
        let candidates = active_versions(&replicas)?;
        if candidates.len() != 1 {
            return Err(fail("local account scopes contain divergent copies of one chat; run sync with Claude stopped to preserve native conflict copies"));
        }
        Ok(candidates.into_values().next().expect("one candidate"))
    }).collect()
}

struct Replica {
    session: NativeSession,
    baseline: Option<String>,
}

fn replica_groups(data_dir: &Path, home: &Path) -> Result<BTreeMap<String, Vec<Replica>>> {
    replica_groups_matching(data_dir, home, None)
}

fn replica_groups_matching(
    data_dir: &Path,
    home: &Path,
    only: Option<&str>,
) -> Result<BTreeMap<String, Vec<Replica>>> {
    let mut result: BTreeMap<String, Vec<Replica>> = BTreeMap::new();
    fn size(value: &Value) -> usize {
        match value {
            Value::String(value) => value.len(),
            Value::Array(values) => values
                .iter()
                .map(size)
                .sum::<usize>()
                .saturating_add(values.len() * 32),
            Value::Object(values) => values
                .iter()
                .map(|(key, value)| key.len().saturating_add(size(value)))
                .sum::<usize>()
                .saturating_add(values.len() * 32),
            _ => 32,
        }
    }
    let mut total = 0usize;
    for location in locations(data_dir)? {
        if only.is_some_and(|id| location.index.file_stem().is_none_or(|name| name != id)) {
            continue;
        }
        let session = capture_location(data_dir, home, &location)?;
        total = total.saturating_add(size(&session.payload));
        if total > MAX_CAPTURE_BYTES {
            return Err(fail(
                "native account histories exceed the 1 GiB capture limit; all original history was kept locally",
            ));
        }
        let baseline = location.body.join(BASELINE);
        check_path(data_dir, &baseline)?;
        let baseline = if baseline.exists() {
            Some(
                json(&baseline)?
                    .as_str()
                    .ok_or_else(|| fail("invalid local baseline"))?
                    .to_string(),
            )
        } else {
            None
        };
        result
            .entry(session.id.clone())
            .or_default()
            .push(Replica { session, baseline });
    }
    Ok(result)
}

/// Clean copies are observations, not competing edits. Archive flags and files
/// can change without advancing a transcript timestamp, so compare full native
/// contents with each scope's last successfully installed baseline.
fn active_versions(replicas: &[Replica]) -> Result<BTreeMap<String, NativeSession>> {
    let mut all = BTreeMap::new();
    let mut dirty = BTreeMap::new();
    for replica in replicas {
        let hash = digest(&replica.session)?;
        all.insert(hash.clone(), replica.session.clone());
        if replica.baseline.as_deref() != Some(hash.as_str()) {
            dirty.insert(hash, replica.session.clone());
        }
    }
    if all.len() == 1 {
        return Ok(all);
    }
    if dirty.is_empty() { Ok(all) } else { Ok(dirty) }
}

#[derive(Serialize, Deserialize)]
struct Operation {
    target: PathBuf,
    had_original: bool,
    expected: String,
    original: Option<String>,
}

fn valid_operation(operation: &Operation) -> bool {
    let hash =
        |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !hash(&operation.expected)
        || operation.had_original != operation.original.is_some()
        || operation
            .original
            .as_deref()
            .is_some_and(|value| !hash(value))
    {
        return false;
    }
    let Some(path) = operation.target.to_str() else {
        return false;
    };
    if relative(path).is_err() {
        return false;
    }
    let parts: Vec<_> = operation
        .target
        .iter()
        .filter_map(|part| part.to_str())
        .collect();
    let leaf = |name: &str| {
        session_id(name.strip_suffix(".json").unwrap_or(name))
            || (name.len() == 8 && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
            || name == "archived-sessions.idx"
    };
    match parts.as_slice() {
        [STORE, account, org, name] => {
            component(account) && component(org) && (leaf(name) || *name == "spaces.json")
        }
        [STORE, account, org, "agent", name] => component(account) && component(org) && leaf(name),
        [STORE, account, org, "spaces", space] => {
            component(account) && component(org) && component(space)
        }
        _ => false,
    }
}

/// Hash native transaction trees without following runtime symlinks. This is
/// local recovery metadata and never transported or displayed. A user may have
/// reopened Claude after a crash: never remove their post-crash edits.
fn tree_hash(path: &Path) -> Result<String> {
    fn walk(path: &Path, hash: &mut Sha256, count: &mut usize, depth: usize) -> Result<()> {
        *count += 1;
        if *count > MAX_FILES * 2 || depth > MAX_DEPTH {
            return Err(fail("native recovery tree exceeds supported limits"));
        }
        let meta = std::fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            hash.update(b"link");
            hash.update(std::fs::read_link(path)?.as_os_str().as_encoded_bytes());
        } else if meta.is_dir() {
            hash.update(b"directory");
            let mut entries = std::fs::read_dir(path)?
                .take(MAX_FILES + 1)
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            if entries.len() > MAX_FILES {
                return Err(fail("native recovery directory exceeds supported limits"));
            }
            entries.sort();
            for entry in entries {
                let name = entry
                    .file_name()
                    .expect("directory entry")
                    .as_encoded_bytes();
                hash.update((name.len() as u64).to_le_bytes());
                hash.update(name);
                walk(&entry, hash, count, depth + 1)?;
            }
        } else if meta.is_file() {
            hash.update(b"file");
            hash.update(meta.len().to_le_bytes());
            let mut file = std::fs::File::open(path)?;
            let mut bytes = [0; 64 * 1024];
            loop {
                let read = file.read(&mut bytes)?;
                if read == 0 {
                    break;
                }
                hash.update(&bytes[..read]);
            }
        } else {
            return Err(fail("unsupported special file in native recovery tree"));
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    walk(path, &mut hash, &mut 0, 0)?;
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn sync_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_lineage(path: &Path, root: &Path) -> Result<()> {
    let mut current = Some(path);
    while let Some(directory) = current {
        sync_directory(directory)?;
        if directory == root {
            return Ok(());
        }
        current = directory.parent();
    }
    Err(fail("native synchronization parent escaped data root"))
}

fn require_idle(ready: &dyn Fn() -> Result<bool>) -> Result<()> {
    if ready()? {
        Ok(())
    } else {
        Err(fail(
            "Claude became active; native history changes were deferred until it is stopped",
        ))
    }
}

fn sync_tree(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            sync_tree(&entry?.path())?;
        }
        sync_directory(path)?;
    } else if meta.is_file() {
        std::fs::File::open(path)?.sync_all()?;
    }
    Ok(())
}

fn pending(data_dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(children(data_dir, data_dir)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(STAGE))
        })
        .collect())
}

fn ensure_no_pending(data_dir: &Path) -> Result<()> {
    if !pending(data_dir)?.is_empty() {
        return Err(fail(
            "an interrupted native restore needs recovery with Claude stopped before capture",
        ));
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(fail("refusing to remove a symbolic link during restore"))
        }
        Ok(meta) if meta.is_dir() => {
            std::fs::remove_dir_all(path)?;
            Ok(())
        }
        Ok(_) => {
            std::fs::remove_file(path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn rollback(data_dir: &Path, stage: &Path, operations: &[Operation]) -> Result<()> {
    // Preflight every operation before rolling any of them back. Both changed
    // current data and original backups remain intact on a recovery conflict.
    for (i, operation) in operations.iter().enumerate() {
        if !valid_operation(operation) {
            return Err(fail(
                "invalid native recovery target; all data was preserved",
            ));
        }
        let target = data_dir.join(&operation.target);
        let backup = stage.join(format!("old-{i}"));
        let staged = stage.join(format!("new-{i}"));
        check_path(data_dir, &target)?;
        check_path(data_dir, &backup)?;
        check_path(data_dir, &staged)?;
        if backup.exists() && operation.original.as_deref() != Some(tree_hash(&backup)?.as_str()) {
            return Err(fail(
                "native recovery backup changed; original and current data were preserved",
            ));
        }
        if (backup.exists() || (!operation.had_original && !staged.exists()))
            && target.exists()
            && tree_hash(&target)? != operation.expected
        {
            return Err(fail(
                "native history changed after an interrupted restore; current history and rollback copies were preserved for recovery",
            ));
        }
    }
    for (i, operation) in operations.iter().enumerate().rev() {
        let target = data_dir.join(&operation.target);
        let backup = stage.join(format!("old-{i}"));
        let staged = stage.join(format!("new-{i}"));
        check_path(data_dir, &target)?;
        check_path(data_dir, &backup)?;
        if backup.exists() {
            remove_path(&target)?;
            std::fs::rename(backup, target)?;
        } else if !operation.had_original && !staged.exists() {
            remove_path(&target)?;
        }
        sync_directory(
            data_dir
                .join(&operation.target)
                .parent()
                .ok_or_else(|| fail("invalid recovery parent"))?,
        )?;
        sync_directory(stage)?;
    }
    remove_path(stage)?;
    sync_directory(data_dir)
}

/// Recover an interrupted transaction only while Claude is stopped. Capture
/// itself stays read-only and reports pending recovery instead of hiding it.
pub fn recover(data_dir: &Path) -> Result<()> {
    if !data_dir.exists() {
        return Ok(());
    }
    for stage in pending(data_dir)? {
        check_path(data_dir, &stage)?;
        let committed = stage.join("committed");
        check_path(data_dir, &committed)?;
        if committed.exists() {
            remove_path(&stage)?;
            continue;
        }
        let journal = stage.join("journal.json");
        if !journal.exists() {
            remove_path(&stage)?;
            continue;
        }
        check_path(data_dir, &journal)?;
        let operations: Vec<Operation> = serde_json::from_value(json(&journal)?)
            .map_err(|_| fail("invalid recovery journal; preserve the transaction directory"))?;
        for operation in &operations {
            relative(
                operation
                    .target
                    .to_str()
                    .ok_or_else(|| fail("invalid recovery path"))?,
            )?;
            if !valid_operation(operation) {
                return Err(fail("recovery journal target is outside Cowork history"));
            }
        }
        rollback(data_dir, &stage, &operations)?;
    }
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    std::fs::create_dir_all(path.parent().ok_or_else(|| fail("invalid staging path"))?)?;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
        )?;
    }
    file.sync_all()?;
    Ok(())
}

// Preserve destination-local runtime/configuration while installing a newer
// native history. These bytes never enter the transport payload. For example,
// a session's freshly issued destination credential must not be replaced by
// the source machine's credential, nor erased by an ordinary history update.
fn keep_local_files(
    source: &Path,
    staged: &Path,
    base: &Path,
    space: bool,
    budget: &mut (usize, u64),
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(fail(
            "destination runtime tree is too deep; all original data was kept",
        ));
    }
    for entry in std::fs::read_dir(source)? {
        budget.0 += 1;
        if budget.0 > MAX_FILES {
            return Err(fail(
                "destination runtime has too many files; all original data was kept",
            ));
        }
        let path = entry?.path();
        let rel = path
            .strip_prefix(base)
            .map_err(|_| fail("invalid local preservation path"))?;
        if (space && (rel.starts_with("memory") || rel == Path::new(SPACE_BASELINE)))
            || (!space
                && (allowed_file(rel) || rel == Path::new(ORIGIN) || rel == Path::new(BASELINE)))
        {
            continue;
        }
        let dest = staged.join(rel);
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            std::fs::create_dir_all(&dest)?;
            keep_local_files(&path, staged, base, space, budget, depth + 1)?;
        } else if meta.file_type().is_symlink() {
            #[cfg(unix)]
            {
                std::fs::create_dir_all(
                    dest.parent()
                        .ok_or_else(|| fail("invalid local symlink path"))?,
                )?;
                std::os::unix::fs::symlink(std::fs::read_link(path)?, dest)?;
            }
            #[cfg(not(unix))]
            return Err(fail(
                "cannot preserve a destination runtime symlink on this platform",
            ));
        } else if meta.is_file() {
            budget.1 = budget.1.saturating_add(meta.len());
            if meta.len() > MAX_FILE_BYTES || budget.1 > MAX_SESSION_BYTES as u64 {
                return Err(fail(
                    "destination runtime exceeds safe staging limits; all original data was kept",
                ));
            }
            std::fs::create_dir_all(
                dest.parent()
                    .ok_or_else(|| fail("invalid local file path"))?,
            )?;
            std::fs::copy(&path, &dest)?;
            std::fs::set_permissions(dest, meta.permissions())?;
        } else {
            return Err(fail("destination runtime has an unsupported special file"));
        }
    }
    Ok(())
}

fn install(
    data_dir: &Path,
    writes: BTreeMap<PathBuf, BTreeMap<PathBuf, (Vec<u8>, bool)>>,
    ready: &dyn Fn() -> Result<bool>,
) -> Result<()> {
    if writes.is_empty() {
        return Ok(());
    }
    require_idle(ready)?;
    let mut originals = BTreeMap::new();
    for target in writes.keys() {
        check_path(data_dir, target)?;
        originals.insert(
            target.clone(),
            if target.exists() {
                Some(tree_hash(target)?)
            } else {
                None
            },
        );
    }
    let stage = tempfile::Builder::new()
        .prefix(STAGE)
        .tempdir_in(data_dir)?;
    let mut operations = Vec::new();
    for (i, (target, files)) in writes.iter().enumerate() {
        check_path(data_dir, target)?;
        let staged = stage.path().join(format!("new-{i}"));
        if target.is_dir()
            && (files.contains_key(Path::new(ORIGIN))
                || files.contains_key(Path::new(SPACE_BASELINE)))
        {
            std::fs::create_dir_all(&staged)?;
            keep_local_files(
                target,
                &staged,
                target,
                files.contains_key(Path::new(SPACE_BASELINE)),
                &mut (0, 0),
                0,
            )?;
        }
        for (relative, (bytes, executable)) in files {
            let path = if relative.as_os_str().is_empty() {
                staged.clone()
            } else {
                staged.join(relative)
            };
            write_file(&path, bytes, *executable)?;
        }
        if files.contains_key(Path::new(ORIGIN)) {
            // An empty native working folder is meaningful: Claude's saved
            // cwd points here even before the first generated attachment.
            for name in ["outputs", "uploads", "host-cwd", ".claude/projects"] {
                let directory = staged.join(name);
                if std::fs::symlink_metadata(&directory).is_err() {
                    std::fs::create_dir_all(&directory)?;
                }
            }
        }
        operations.push(Operation {
            target: target
                .strip_prefix(data_dir)
                .map_err(|_| fail("invalid write target"))?
                .to_path_buf(),
            had_original: originals[target].is_some(),
            expected: tree_hash(&staged)?,
            original: originals[target].clone(),
        });
    }
    write_file(
        &stage.path().join("journal.json"),
        &serde_json::to_vec(&operations)?,
        false,
    )?;
    // Make the staged data and journal directory entries durable before any
    // original native files are moved out of the application's namespace.
    sync_tree(stage.path())?;
    sync_directory(data_dir)?;
    // Once mutations start, retaining the journal on rollback failure matters
    // more than automatic temporary-directory cleanup.
    let stage = stage.keep();
    let result = (|| -> Result<()> {
        require_idle(ready)?;
        for operation in &operations {
            let target = data_dir.join(&operation.target);
            let current = if target.exists() {
                Some(tree_hash(&target)?)
            } else {
                None
            };
            if current != operation.original {
                return Err(fail(
                    "native history changed during staging; its new contents were preserved",
                ));
            }
        }
        for (i, operation) in operations.iter().enumerate() {
            require_idle(ready)?;
            let target = data_dir.join(&operation.target);
            check_path(data_dir, &target)?;
            let current = if target.exists() {
                Some(tree_hash(&target)?)
            } else {
                None
            };
            if current != operation.original {
                return Err(fail(
                    "native history changed before installation; its new contents were preserved",
                ));
            }
            std::fs::create_dir_all(
                target
                    .parent()
                    .ok_or_else(|| fail("invalid target parent"))?,
            )?;
            sync_lineage(
                target
                    .parent()
                    .ok_or_else(|| fail("invalid native parent"))?,
                data_dir,
            )?;
            if operation.had_original {
                std::fs::rename(&target, stage.join(format!("old-{i}")))?;
            }
            std::fs::rename(stage.join(format!("new-{i}")), target)?;
            sync_directory(
                data_dir
                    .join(&operation.target)
                    .parent()
                    .ok_or_else(|| fail("invalid native target parent"))?,
            )?;
            sync_directory(&stage)?;
        }
        for (target, files) in &writes {
            if !unchanged(data_dir, target, files)? {
                return Err(fail("native restore read-back validation failed"));
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        if !ready()? {
            return Err(fail(
                "Claude opened during native restore; complete history and rollback copies were preserved for recovery after it stops",
            ));
        }
        rollback(data_dir, &stage, &operations).map_err(|_| fail("restore failed and rollback needs recovery; retain the native transaction directory"))?;
        return Err(error);
    }
    // A committed marker distinguishes cleanup interrupted after installation
    // from an incomplete transaction; never roll a completed restore back.
    write_file(&stage.join("committed"), b"1", false)?;
    sync_directory(&stage)?;
    remove_path(&stage)?;
    sync_directory(data_dir)
}

fn decoded(
    files: &BTreeMap<String, File>,
    session_files: bool,
) -> Result<BTreeMap<PathBuf, (Vec<u8>, bool)>> {
    let mut result = BTreeMap::new();
    let mut total = 0usize;
    for (name, file) in files {
        if result.len() >= MAX_FILES || file.data.len() > (MAX_FILE_BYTES as usize) * 4 / 3 + 4 {
            return Err(fail("native package exceeds the supported file limit"));
        }
        let path = relative(name)?;
        if path.components().count() > MAX_DEPTH {
            return Err(fail("native package file path is too deep"));
        }
        if session_files && !allowed_file(&path) {
            return Err(fail("package contains a disallowed native file"));
        }
        if !session_files && !path.starts_with("memory") {
            return Err(fail("space package contains a non-memory runtime file"));
        }
        if !session_files
            && path.iter().any(|part| {
                part.to_str().is_some_and(|part| {
                    matches!(
                        part,
                        ".credentials.json" | "config.json" | "Cookies" | ".env"
                    )
                })
            })
        {
            return Err(fail(
                "space package contains an authentication/configuration file",
            ));
        }
        let bytes = STANDARD
            .decode(&file.data)
            .map_err(|_| fail("invalid base64 native file"))?;
        total = total.saturating_add(bytes.len());
        if total > MAX_SESSION_BYTES {
            return Err(fail("native package exceeds the 320 MiB session limit"));
        }
        result.insert(path, (bytes, file.executable));
    }
    Ok(result)
}

fn unchanged(
    root: &Path,
    target: &Path,
    files: &BTreeMap<PathBuf, (Vec<u8>, bool)>,
) -> Result<bool> {
    if !target.exists() {
        return Ok(false);
    }
    check_path(root, target)?;
    if target.is_dir()
        && (files.contains_key(Path::new(ORIGIN)) || files.contains_key(Path::new(SPACE_BASELINE)))
    {
        let current = capture_files(root, target)?;
        let wanted: BTreeSet<_> = files
            .keys()
            .filter(|path| allowed_file(path))
            .cloned()
            .collect();
        let present: BTreeSet<_> = current.keys().map(PathBuf::from).collect();
        if wanted != present {
            return Ok(false);
        }
    }
    for (rel, (bytes, _)) in files {
        let path = target.join(rel);
        let path = if rel.as_os_str().is_empty() {
            target.to_path_buf()
        } else {
            path
        };
        check_path(root, &path)?;
        if !path.is_file() || std::fs::read(path)? != *bytes {
            return Ok(false);
        }
    }
    Ok(true)
}

fn replacements_for(
    data: &Path,
    home: &Path,
    scope: &Path,
    body: &Path,
    owner: &Owner,
    mappings: &[(String, String)],
) -> Vec<(String, String)> {
    let mut result = vec![
        (BODY.into(), body.to_string_lossy().into_owned()),
        (SCOPE.into(), scope.to_string_lossy().into_owned()),
        (DATA.into(), data.to_string_lossy().into_owned()),
        (HOME.into(), home.to_string_lossy().into_owned()),
    ];
    let source_home = [(owner.source_home.clone(), HOME.into())];
    result.extend(mappings.iter().map(|(from, to)| {
        (
            if owner.source_home.is_empty() {
                from.clone()
            } else {
                replace_path(from, &source_home)
            },
            to.clone(),
        )
    }));
    result.sort_by_key(|item| std::cmp::Reverse(item.0.len()));
    result
}

fn read_space(data: &Path, scope: &Path, id: &str) -> Result<Option<Space>> {
    let path = scope.join("spaces.json");
    check_path(data, &path)?;
    if !path.exists() {
        return Ok(None);
    }
    let registry = json(&path)?;
    let record = registry
        .get("spaces")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("unsupported destination spaces registry"))?
        .iter()
        .find(|record| record.get("id").and_then(Value::as_str) == Some(id))
        .cloned();
    let Some(record) = record else {
        return Ok(None);
    };
    let base = scope.join("spaces").join(id);
    let mut files = BTreeMap::new();
    if base.join("memory").exists() {
        collect_tree(data, &base.join("memory"), &base, &mut files)?;
    }
    Ok(Some(Space { record, files }))
}

/// Project snapshots bundled with different chats can arrive out of order.
/// Without independent project ancestry, differing snapshots get separate native
/// project IDs on every account scope; an older chat cannot roll memory back.
fn choose_space(
    data: &Path,
    home: &Path,
    scopes: &[(PathBuf, Owner)],
    payload: &Payload,
    target: &str,
    mappings: &[(String, String)],
) -> Result<Option<Space>> {
    let Some(space) = &payload.space else {
        return Ok(None);
    };
    decoded(&space.files, false)?;
    let original_id = space
        .record
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| component(id))
        .ok_or_else(|| fail("invalid space identity"))?;
    if payload.index.get("spaceId").and_then(Value::as_str) != Some(original_id) {
        return Err(fail("session and project identities disagree"));
    }
    let mut candidate = space.clone();
    for attempt in 0..32 {
        if attempt > 0 {
            candidate.record["id"] = Value::String(fork_id(&format!(
                "{original_id}:{}:{attempt}",
                digest(space)?
            )));
        }
        let id = candidate.record["id"]
            .as_str()
            .expect("validated project id");
        let mut conflict = false;
        for (scope, _) in scopes {
            let parent =
                if payload.index.get("sessionType").and_then(Value::as_str) == Some("agent") {
                    scope.join("agent")
                } else {
                    scope.clone()
                };
            let body = body_path(data, &parent, target)?;
            let mut incoming = candidate.clone();
            map_tree(
                &mut incoming.record,
                &replacements_for(data, home, scope, &body, &payload.owner, mappings),
            );
            if let Some(current) = read_space(data, scope, id)?
                && current != incoming
            {
                conflict = true;
                break;
            }
        }
        if !conflict {
            return Ok(Some(candidate));
        }
    }
    Err(fail(
        "unable to allocate a distinct native project for incoming conflicting context",
    ))
}

/// Restore full native history to every existing local account/org scope. The
/// caller must stop Claude first. The sync engine selects conflicts/fast-forwards;
/// this adapter refuses already divergent local replicas and rolls back failures.
pub fn restore(
    data_dir: &Path,
    home: &Path,
    session: &NativeSession,
    target_id: &str,
    mappings: &[(String, String)],
) -> Result<()> {
    restore_if_idle(data_dir, home, session, target_id, mappings, &|| Ok(true))
}

pub fn restore_if_idle(
    data_dir: &Path,
    home: &Path,
    session: &NativeSession,
    target_id: &str,
    mappings: &[(String, String)],
    ready: &dyn Fn() -> Result<bool>,
) -> Result<()> {
    restore_inner(data_dir, home, session, target_id, mappings, None, ready)
}

fn restore_inner(
    data_dir: &Path,
    home: &Path,
    session: &NativeSession,
    target_id: &str,
    mappings: &[(String, String)],
    expected_replicas: Option<&[Replica]>,
    ready: &dyn Fn() -> Result<bool>,
) -> Result<()> {
    require_idle(ready)?;
    if !session_id(&session.id) || !session_id(target_id) {
        return Err(fail("invalid native session ID"));
    }
    recover(data_dir)?;
    let payload: Payload = serde_json::from_value(session.payload.clone())
        .map_err(|_| fail("unsupported native package schema"))?;
    if payload.version != 1
        || payload.index.get("sessionId").and_then(Value::as_str) != Some(&session.id)
    {
        return Err(fail("native package identity/version mismatch"));
    }
    if !component(&payload.owner.account) || !component(&payload.owner.organization) {
        return Err(fail("invalid source owner"));
    }
    check_transcript(&payload.index, &payload.files)?;
    let mut index = payload.index.clone();
    scrub_index(&mut index)?;
    let decoded_files = decoded(&payload.files, true)?;
    let scopes = scopes(data_dir)?;
    if scopes.is_empty() {
        return Err(fail(
            "pending: sign in to Claude and create a local Cowork chat on this Mac before native restore",
        ));
    }
    // Prove every replica agrees before any incoming version may replace it.
    if expected_replicas.is_none()
        && let Some(replicas) =
            replica_groups_matching(data_dir, home, Some(target_id))?.get(target_id)
        && active_versions(replicas)?.len() > 1
    {
        return Err(fail(
            "local replicas diverged; reconcile and preserve conflict copies before restore",
        ));
    }
    let selected_space = choose_space(data_dir, home, &scopes, &payload, target_id, mappings)?;
    if let Some(space) = &selected_space {
        index["spaceId"] = space.record["id"].clone();
    }
    let mut writes = BTreeMap::new();
    for (scope, _) in scopes {
        let parent = if index.get("sessionType").and_then(Value::as_str) == Some("agent") {
            scope.join("agent")
        } else {
            scope.clone()
        };
        let body = body_path(data_dir, &parent, target_id)?;
        let replacements =
            replacements_for(data_dir, home, &scope, &body, &payload.owner, mappings);
        let mut index = index.clone();
        index["sessionId"] = Value::String(target_id.into());
        map_index(&mut index, &replacements);
        let mut body_files = decoded_files.clone();
        body_files.insert(
            PathBuf::from(ORIGIN),
            (serde_json::to_vec(&payload.owner)?, false),
        );
        if !unchanged(data_dir, &body, &body_files)? {
            writes.insert(body, body_files);
        }
        if let Some(space) = &selected_space {
            let mut record = space.record.clone();
            let id = record
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| component(id))
                .ok_or_else(|| fail("invalid space identity"))?
                .to_string();
            map_tree(&mut record, &replacements);
            let registry_path = scope.join("spaces.json");
            check_path(data_dir, &registry_path)?;
            let mut registry = if registry_path.exists() {
                json(&registry_path)?
            } else {
                serde_json::json!({"spaces":[]})
            };
            let rows = registry
                .get_mut("spaces")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| fail("unsupported destination spaces registry"))?;
            if let Some(existing) = rows
                .iter_mut()
                .find(|row| row.get("id").and_then(Value::as_str) == Some(id.as_str()))
            {
                *existing = record.clone();
            } else {
                rows.push(record.clone());
            }
            let files = BTreeMap::from([(PathBuf::new(), (serde_json::to_vec(&registry)?, false))]);
            if !unchanged(data_dir, &registry_path, &files)? {
                writes.insert(registry_path, files);
            }
            let physical = Space {
                record,
                files: space.files.clone(),
            };
            let space_path = scope.join("spaces").join(id);
            let mut files = decoded(&space.files, false)?;
            files.insert(
                PathBuf::from(SPACE_BASELINE),
                (serde_json::to_vec(&digest(&physical)?)?, false),
            );
            let current = read_space(
                data_dir,
                &scope,
                physical.record["id"].as_str().expect("validated space"),
            )?;
            if current.as_ref() != Some(&physical) || !unchanged(data_dir, &space_path, &files)? {
                writes.insert(space_path, files);
            }
        }
        let index_path = parent.join(format!("{target_id}.json"));
        check_path(data_dir, &index_path)?;
        if index_path.exists() {
            preserve_local_index(&mut index, &json(&index_path)?)?;
        }
        let files = BTreeMap::from([(PathBuf::new(), (serde_json::to_vec(&index)?, false))]);
        if !unchanged(data_dir, &index_path, &files)? {
            writes.insert(index_path.clone(), files);
        }
        // The loader supports absent hints; a stale hint must not hide a newly
        // restored index. Rebuild it from all records plus this incoming one.
        let hint = parent.join("archived-sessions.idx");
        if hint.exists() {
            let mut archived = BTreeSet::new();
            for path in children(data_dir, &parent)? {
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                    && path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .is_some_and(session_id)
                {
                    check_path(data_dir, &path)?;
                    let doc = if path == index_path {
                        index.clone()
                    } else {
                        json(&path)?
                    };
                    if doc.get("isArchived").and_then(Value::as_bool) == Some(true)
                        && let Some(id) = doc.get("sessionId").and_then(Value::as_str)
                    {
                        archived.insert(id.to_string());
                    }
                }
            }
            if index.get("isArchived").and_then(Value::as_bool) == Some(true) {
                archived.insert(target_id.into());
            } else {
                archived.remove(target_id);
            }
            let bytes = serde_json::to_vec(&serde_json::json!({"v":1,"archived":archived}))?;
            let files = BTreeMap::from([(PathBuf::new(), (bytes, false))]);
            if !unchanged(data_dir, &hint, &files)? {
                writes.insert(hint, files);
            }
        }
    }
    if let Some(expected) = expected_replicas {
        let current = replica_groups_matching(data_dir, home, Some(target_id))?;
        let current = current.get(target_id).map(Vec::as_slice).unwrap_or(&[]);
        let fingerprints = |replicas: &[Replica]| -> Result<Vec<String>> {
            let mut hashes = replicas
                .iter()
                .map(|replica| digest(&replica.session))
                .collect::<Result<Vec<_>>>()?;
            hashes.sort();
            Ok(hashes)
        };
        if fingerprints(current)? != fingerprints(expected)? {
            return Err(fail(
                "native account history changed during reconciliation; all versions were preserved",
            ));
        }
    }
    require_idle(ready)?;
    install(data_dir, writes, ready)?;
    // These local receipts never enter the transport payload. Interrupted
    // receipt writes leave complete native data and at worst an unobserved copy.
    for location in locations(data_dir)?.into_iter().filter(|location| {
        location
            .index
            .file_stem()
            .is_some_and(|name| name == target_id)
    }) {
        let installed = capture_location(data_dir, home, &location)?;
        crate::cache::atomic_write(
            &location.body.join(BASELINE),
            &serde_json::to_vec(&digest(&installed)?)?,
        )?;
    }
    Ok(())
}

/// Fill newly captured local account scopes even when no remote revision is
/// pending. Called only with Claude stopped, before the engine captures state.
pub fn reconcile(data_dir: &Path, home: &Path, mappings: &[(String, String)]) -> Result<()> {
    reconcile_if_idle(data_dir, home, mappings, &|| Ok(true))
}

pub fn reconcile_if_idle(
    data_dir: &Path,
    home: &Path,
    mappings: &[(String, String)],
    ready: &dyn Fn() -> Result<bool>,
) -> Result<()> {
    require_idle(ready)?;
    recover(data_dir)?;
    for (id, replicas) in replica_groups(data_dir, home)? {
        let mut versions = active_versions(&replicas)?.into_iter();
        let Some((_, selected)) = versions.next() else {
            continue;
        };
        // Preserve every independently edited version before updating the
        // original. Stable fork IDs make an interrupted retry idempotent.
        for (hash, alternative) in versions {
            let target = format!("local_{}", fork_id(&format!("{id}:{hash}")));
            restore_inner(data_dir, home, &alternative, &target, mappings, None, ready)?;
        }
        restore_inner(
            data_dir,
            home,
            &selected,
            &id,
            mappings,
            Some(&replicas),
            ready,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "local_11111111-2222-4333-8444-555555555555";
    const CLI: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    const FORK: &str = "local_99999999-8888-4777-8666-555555555555";

    fn put(path: &Path, bytes: impl AsRef<[u8]>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn scope(data: &Path, account: &str) -> PathBuf {
        let scope = data.join(STORE).join(account).join("org-a");
        std::fs::create_dir_all(&scope).unwrap();
        scope
    }

    fn fixture(data: &Path, home: &Path, short: bool, agent: bool) -> (PathBuf, PathBuf) {
        let scope = scope(data, "account-source");
        let parent = if agent {
            scope.join("agent")
        } else {
            scope.clone()
        };
        let body = parent.join(if short { "11111111" } else { ID });
        let mut index = json!({
            "sessionId":ID,"cliSessionId":CLI,"createdAt":1,"lastActivityAt":2,
            "cwd":body.join("outputs"),"processName":"native-fixture", "hostLoopMode":true,
            "title":"Synthetic session", "isArchived":true,
            "userSelectedFolders":[home.join("Documents/project")],
            "initialMessage":"Keep the literal /Users/source in this message",
            "systemPrompt":format!("Files live at {}/outputs",body.display()),
            "accountName":"Synthetic source owner", "emailAddress":"source@example.invalid",
            "unknownFutureField":{"enabled":true},
            "remoteMcpServersConfig":[{"authorization":"secret-mcp-credential"}],
            "artifactHostGrant":"secret-grant", "pendingStartMessages":["never replay"],
            "apiKey":"secret-api-key"
        });
        if agent {
            index["sessionType"] = json!("agent");
        }
        put(
            &parent.join(format!("{ID}.json")),
            serde_json::to_vec(&index).unwrap(),
        );
        put(
            &body.join(format!(".claude/projects/-sessions-fixture/{CLI}.jsonl")),
            b"{\"type\":\"user\",\"message\":{\"content\":\"synthetic\"}}\n",
        );
        put(
            &body.join(format!(
                ".claude/projects/-sessions-fixture/{CLI}/subagents/agent-one.jsonl"
            )),
            b"native subagent history\n",
        );
        put(&body.join("outputs/report.bin"), [0, 255, 1, 10]);
        put(&body.join("uploads/input.txt"), "attached synthetic input");
        put(
            &body.join("audit.jsonl"),
            "audit bytes and their HMAC stay unchanged\n",
        );
        put(&body.join(".audit-key"), "synthetic-audit-integrity-key");
        put(
            &body.join(".claude/.credentials.json"),
            "secret-source-credential",
        );
        put(&body.join(".claude/.claude.json"), "source-machine-config");
        put(
            &body.join(".claude/telemetry/private.log"),
            "private raw diagnostics",
        );
        put(&data.join("config.json"), "global-source-auth");
        (scope, body)
    }

    fn package(session: &NativeSession) -> Payload {
        serde_json::from_value(session.payload.clone()).unwrap()
    }

    #[test]
    fn capture_is_native_complete_and_excludes_authentication_and_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("source");
        let home = Path::new("/Users/source");
        let (_, body) = fixture(&data, home, false, false);
        let captured = capture(&data, home).unwrap();
        assert_eq!(captured.len(), 1);
        let payload = package(&captured[0]);
        assert_eq!(payload.index["cwd"], format!("{BODY}/outputs"));
        assert_eq!(
            payload.index["userSelectedFolders"][0],
            format!("{HOME}/Documents/project")
        );
        assert_eq!(
            payload.index["initialMessage"],
            "Keep the literal /Users/source in this message"
        );
        assert_eq!(payload.index["unknownFutureField"]["enabled"], true);
        for key in [
            "remoteMcpServersConfig",
            "artifactHostGrant",
            "pendingStartMessages",
            "apiKey",
        ] {
            assert!(payload.index.get(key).is_none());
        }
        assert!(
            payload
                .files
                .keys()
                .any(|name| name.ends_with("subagents/agent-one.jsonl"))
        );
        assert!(payload.files.contains_key("uploads/input.txt"));
        assert!(
            !payload.files.keys().any(|name| name.contains("credentials")
                || name.contains("telemetry")
                || name.ends_with(".claude.json"))
        );
        assert_eq!(
            STANDARD.decode(&payload.files["audit.jsonl"].data).unwrap(),
            std::fs::read(body.join("audit.jsonl")).unwrap()
        );
        assert_eq!(
            STANDARD
                .decode(&payload.files["outputs/report.bin"].data)
                .unwrap(),
            [0, 255, 1, 10]
        );
    }

    #[test]
    fn native_restore_fans_out_without_auth_and_roundtrips_in_every_account() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fixture(&source, Path::new("/Users/source"), false, false);
        let incoming = capture(&source, Path::new("/Users/source"))
            .unwrap()
            .remove(0);
        let a = scope(&destination, "account-a");
        let b = scope(&destination, "account-b");
        put(
            &destination.join("config.json"),
            "destination-auth-stays-local",
        );
        for scope in [&a, &b] {
            put(
                &scope.join("archived-sessions.idx"),
                r#"{"v":1,"archived":[]}"#,
            );
        }
        restore(&destination, Path::new("/Users/new"), &incoming, ID, &[]).unwrap();
        for scope in [&a, &b] {
            let index = json(&scope.join(format!("{ID}.json"))).unwrap();
            assert_eq!(
                index["cwd"],
                scope.join(ID).join("outputs").to_str().unwrap()
            );
            assert_eq!(
                index["userSelectedFolders"][0],
                "/Users/new/Documents/project"
            );
            assert_eq!(index["accountName"], "Synthetic source owner");
            assert_eq!(
                std::fs::read(scope.join(ID).join("outputs/report.bin")).unwrap(),
                [0, 255, 1, 10]
            );
            assert!(!scope.join(ID).join(".claude/.credentials.json").exists());
            assert!(
                json(&scope.join("archived-sessions.idx")).unwrap()["archived"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(ID))
            );
        }
        assert_eq!(
            std::fs::read(destination.join("config.json")).unwrap(),
            b"destination-auth-stays-local"
        );
        assert_eq!(
            capture(&destination, Path::new("/Users/new")).unwrap(),
            vec![incoming.clone()]
        );
        // Installing the same version again does not change mtimes.
        let index = a.join(format!("{ID}.json"));
        let modified = std::fs::metadata(&index).unwrap().modified().unwrap();
        restore(&destination, Path::new("/Users/new"), &incoming, ID, &[]).unwrap();
        assert_eq!(
            std::fs::metadata(index).unwrap().modified().unwrap(),
            modified
        );
    }

    #[test]
    fn short_agent_storage_is_captured_and_shared_locally() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Claude");
        let home = Path::new("/Users/source");
        let (_, original_body) = fixture(&data, home, true, true);
        let added = scope(&data, "account-new");
        reconcile(&data, home, &[]).unwrap();
        assert!(added.join("agent").join(format!("{ID}.json")).exists());
        assert!(added.join("agent").join(ID).join("audit.jsonl").exists());
        assert!(
            original_body.join(".claude/.credentials.json").exists(),
            "destination-local credentials are retained, never transported"
        );
        assert_eq!(capture(&data, home).unwrap().len(), 1);
    }

    #[test]
    fn account_local_connector_state_survives_reconcile_without_entering_transport() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Claude");
        let home = Path::new("/Users/source");
        let (origin, body) = fixture(&data, home, false, false);
        let origin_index = origin.join(format!("{ID}.json"));
        let mut original = json(&origin_index).unwrap();
        original["futureConnector"] =
            json!({"server":"source", "auth":{"accessToken":"source-token"}});
        put(&origin_index, serde_json::to_vec(&original).unwrap());
        let destination = scope(&data, "account-other");
        let destination_index = destination.join(format!("{ID}.json"));
        reconcile(&data, home, &[]).unwrap();
        let preserved = json(&origin_index).unwrap();
        let fresh = json(&destination_index).unwrap();
        for key in [
            "remoteMcpServersConfig",
            "artifactHostGrant",
            "pendingStartMessages",
            "apiKey",
            "futureConnector",
        ] {
            assert_eq!(preserved[key], original[key]);
            assert!(fresh.get(key).is_none());
            assert!(
                package(&capture(&data, home).unwrap()[0])
                    .index
                    .get(key)
                    .is_none()
            );
        }
        let mut local = fresh;
        local["remoteMcpServersConfig"] =
            json!([{"server":"destination", "authorization":"destination-token"}]);
        local["artifactHostGrant"] = json!("destination-grant");
        local["pendingStartMessages"] = json!(["local pending message"]);
        local["futureConnector"] =
            json!({"server":"destination", "auth":{"accessToken":"destination-token"}});
        put(&destination_index, serde_json::to_vec(&local).unwrap());
        put(&body.join("outputs/new.txt"), "new source output");
        reconcile(&data, home, &[]).unwrap();
        let preserved = json(&destination_index).unwrap();
        for key in [
            "remoteMcpServersConfig",
            "artifactHostGrant",
            "pendingStartMessages",
            "futureConnector",
        ] {
            assert_eq!(preserved[key], local[key]);
        }
        assert_eq!(
            std::fs::read(destination.join(ID).join("outputs/new.txt")).unwrap(),
            b"new source output"
        );
        let incoming = capture(&data, home).unwrap().remove(0);
        restore(&data, home, &incoming, FORK, &[]).unwrap();
        let fork = json(&origin.join(format!("{FORK}.json"))).unwrap();
        for key in LOCAL_INDEX_FIELDS
            .iter()
            .copied()
            .chain(["apiKey", "futureConnector"])
        {
            assert!(fork.get(key).is_none());
        }
    }

    #[test]
    fn distinct_full_session_ids_with_same_short_prefix_keep_separate_native_bodies() {
        const COLLISION: &str = "local_11111111-aaaa-4bbb-8ccc-dddddddddddd";
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Claude");
        let home = Path::new("/Users/source");
        let (scope, body) = fixture(&data, home, true, false);
        let original = tree_hash(&body).unwrap();
        let mut incoming = capture(&data, home).unwrap().remove(0);
        let mut payload = package(&incoming);
        payload.files.insert(
            "outputs/report.bin".into(),
            File {
                data: STANDARD.encode(b"other chat output"),
                executable: false,
            },
        );
        incoming.payload = serde_json::to_value(payload).unwrap();
        restore(&data, home, &incoming, COLLISION, &[]).unwrap();
        assert_eq!(tree_hash(&body).unwrap(), original);
        assert_eq!(
            std::fs::read(scope.join(COLLISION).join("outputs/report.bin")).unwrap(),
            b"other chat output"
        );
        assert!(scope.join(format!("{ID}.json")).exists());
        assert!(scope.join(format!("{COLLISION}.json")).exists());
        assert_eq!(capture(&data, home).unwrap().len(), 2);
    }

    #[test]
    fn staging_refuses_changed_native_files_and_stops_when_claude_opens() {
        use std::cell::Cell;
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Claude");
        let scope = scope(&data, "account");
        let target = scope.join(format!("{ID}.json"));
        put(&target, b"original");
        let writes = || {
            BTreeMap::from([(
                target.clone(),
                BTreeMap::from([(PathBuf::new(), (b"incoming".to_vec(), false))]),
            )])
        };
        let calls = Cell::new(0);
        assert!(
            install(&data, writes(), &|| {
                let call = calls.get() + 1;
                calls.set(call);
                if call == 2 {
                    put(&target, b"new user edit");
                }
                Ok(true)
            })
            .is_err()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new user edit");
        assert!(pending(&data).unwrap().is_empty());
        let calls = Cell::new(0);
        assert!(
            install(&data, writes(), &|| {
                let call = calls.get() + 1;
                calls.set(call);
                Ok(call == 1)
            })
            .is_err()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new user edit");
        assert_eq!(pending(&data).unwrap().len(), 1);
        recover(&data).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new user edit");
        assert!(pending(&data).unwrap().is_empty());
    }

    #[test]
    fn reconciliation_revalidates_all_source_replicas_before_replacing_them() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Claude");
        let home = Path::new("/Users/source");
        let (_, body) = fixture(&data, home, false, false);
        let replicas = replica_groups(&data, home).unwrap().remove(ID).unwrap();
        let stale = replicas[0].session.clone();
        put(
            &body.join("outputs/new-user-edit.txt"),
            "edit after initial capture",
        );
        assert!(
            restore_inner(&data, home, &stale, ID, &[], Some(&replicas), &|| Ok(true)).is_err()
        );
        assert_eq!(
            std::fs::read(body.join("outputs/new-user-edit.txt")).unwrap(),
            b"edit after initial capture"
        );
        assert!(pending(&data).unwrap().is_empty());
    }

    #[test]
    fn explicit_project_mapping_beats_home_relocation_and_fork_retains_cli_history() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("source");
        fixture(&data, Path::new("/Users/source"), false, false);
        let incoming = capture(&data, Path::new("/Users/source"))
            .unwrap()
            .remove(0);
        let dest = temp.path().join("dest");
        let scope = scope(&dest, "dest-account");
        restore(
            &dest,
            Path::new("/Users/new"),
            &incoming,
            FORK,
            &[(
                "/Users/source/Documents/project".into(),
                "/Volumes/Work/project".into(),
            )],
        )
        .unwrap();
        let index = json(&scope.join(format!("{FORK}.json"))).unwrap();
        assert_eq!(index["sessionId"], FORK);
        assert_eq!(index["cliSessionId"], CLI);
        assert_eq!(index["userSelectedFolders"][0], "/Volumes/Work/project");
        assert!(
            scope
                .join(FORK)
                .join(format!(".claude/projects/-sessions-fixture/{CLI}.jsonl"))
                .exists()
        );
        assert!(!scope.join(ID).exists());
    }

    #[test]
    fn conflicting_local_replicas_and_missing_transcripts_fail_without_data_loss() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("source");
        let home = Path::new("/Users/source");
        let (_, body) = fixture(&data, home, false, false);
        let added = scope(&data, "account-new");
        reconcile(&data, home, &[]).unwrap();
        let incoming = capture(&data, home).unwrap().remove(0);
        put(
            &added.join(ID).join("outputs/report.bin"),
            "different outputs",
        );
        put(
            &body.join("outputs/report.bin"),
            "independently different outputs",
        );
        assert!(
            capture(&data, home)
                .unwrap_err()
                .to_string()
                .contains("divergent")
        );
        assert!(
            restore(&data, home, &incoming, ID, &[])
                .unwrap_err()
                .to_string()
                .contains("diverged")
        );
        assert_eq!(
            std::fs::read(added.join(ID).join("outputs/report.bin")).unwrap(),
            b"different outputs"
        );
        std::fs::remove_file(body.join(format!(".claude/projects/-sessions-fixture/{CLI}.jsonl")))
            .unwrap();
        assert!(
            capture(&data, home)
                .unwrap_err()
                .to_string()
                .contains("transcript is missing")
        );
    }

    #[test]
    fn one_dirty_account_advances_all_replicas_and_independent_edits_become_native_forks() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Claude");
        let home = Path::new("/Users/source");
        let (_, original) = fixture(&data, home, false, false);
        let other = scope(&data, "account-other");
        reconcile(&data, home, &[]).unwrap();
        put(&original.join("outputs/report.bin"), "first new turn");
        std::fs::remove_file(original.join("uploads/input.txt")).unwrap();
        // No timestamp change: full native baselines still identify the edit.
        reconcile(&data, home, &[]).unwrap();
        assert_eq!(
            std::fs::read(other.join(ID).join("outputs/report.bin")).unwrap(),
            b"first new turn"
        );
        assert!(!other.join(ID).join("uploads/input.txt").exists());
        assert_eq!(capture(&data, home).unwrap().len(), 1);
        put(
            &original.join("outputs/report.bin"),
            "edit on first account",
        );
        put(
            &other.join(ID).join("outputs/report.bin"),
            "edit on second account",
        );
        reconcile(&data, home, &[]).unwrap();
        let captured = capture(&data, home).unwrap();
        assert_eq!(captured.len(), 2);
        let contents: BTreeSet<_> = captured
            .iter()
            .map(|session| {
                STANDARD
                    .decode(&package(session).files["outputs/report.bin"].data)
                    .unwrap()
            })
            .collect();
        assert_eq!(
            contents,
            BTreeSet::from([
                b"edit on first account".to_vec(),
                b"edit on second account".to_vec()
            ])
        );
        for session in &captured {
            assert!(other.join(format!("{}.json", session.id)).exists());
            assert!(
                other
                    .join(&session.id)
                    .join(format!(".claude/projects/-sessions-fixture/{CLI}.jsonl"))
                    .exists()
            );
        }
        reconcile(&data, home, &[]).unwrap();
        assert_eq!(
            capture(&data, home).unwrap(),
            captured,
            "fork recovery is idempotent"
        );
    }

    #[test]
    fn related_project_metadata_and_memory_restore_without_replacing_other_projects() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("source");
        let home = Path::new("/Users/source");
        let (source_scope, _) = fixture(&data, home, false, false);
        let index_path = source_scope.join(format!("{ID}.json"));
        let mut index = json(&index_path).unwrap();
        index["spaceId"] = json!("aaaa-bbbb");
        put(&index_path, serde_json::to_vec(&index).unwrap());
        put(&source_scope.join("spaces.json"), serde_json::to_vec(&json!({"spaces":[{"id":"aaaa-bbbb","name":"Synthetic project","folders":[{"path":"/Users/source/Documents/project"}]}]})).unwrap());
        put(
            &source_scope.join("spaces/aaaa-bbbb/memory/MEMORY.md"),
            "Native project memory",
        );
        let incoming = capture(&data, home).unwrap().remove(0);
        let dest = temp.path().join("destination");
        let destination_scope = scope(&dest, "destination");
        put(
            &destination_scope.join("spaces.json"),
            r#"{"spaces":[{"id":"other","name":"Keep me"}]}"#,
        );
        restore(&dest, Path::new("/Users/new"), &incoming, ID, &[]).unwrap();
        let registry = json(&destination_scope.join("spaces.json")).unwrap();
        assert_eq!(registry["spaces"].as_array().unwrap().len(), 2);
        assert_eq!(registry["spaces"][0]["name"], "Keep me");
        assert_eq!(
            registry["spaces"][1]["folders"][0]["path"],
            "/Users/new/Documents/project"
        );
        assert_eq!(
            std::fs::read(destination_scope.join("spaces/aaaa-bbbb/memory/MEMORY.md")).unwrap(),
            b"Native project memory"
        );
    }

    #[test]
    fn unsafe_payload_and_scope_links_are_rejected_before_any_native_write() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("source");
        fixture(&data, Path::new("/Users/source"), false, false);
        let incoming = capture(&data, Path::new("/Users/source"))
            .unwrap()
            .remove(0);
        let dest = temp.path().join("destination");
        let destination_scope = scope(&dest, "destination");
        let mut attack = incoming.clone();
        attack.payload["files"]["../../config.json"] =
            json!({"data":STANDARD.encode("overwrite auth")});
        assert!(restore(&dest, Path::new("/Users/new"), &attack, ID, &[]).is_err());
        assert!(!destination_scope.join(format!("{ID}.json")).exists());
        #[cfg(unix)]
        {
            let outside = temp.path().join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            std::os::unix::fs::symlink(&outside, destination_scope.join(ID)).unwrap();
            assert!(restore(&dest, Path::new("/Users/new"), &incoming, ID, &[]).is_err());
            assert!(std::fs::read_dir(outside).unwrap().next().is_none());
        }
    }

    #[test]
    fn recovery_rolls_back_interrupted_writes_but_keeps_committed_history() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Claude");
        let scope = scope(&data, "account");
        let target = scope.join(format!("{ID}.json"));
        put(&target, "new partial index");
        let stage = data.join(format!("{STAGE}fixture"));
        put(&stage.join("old-0"), "original index");
        put(
            &stage.join("journal.json"),
            serde_json::to_vec(&vec![Operation {
                target: target.strip_prefix(&data).unwrap().to_path_buf(),
                had_original: true,
                expected: tree_hash(&target).unwrap(),
                original: Some(tree_hash(&stage.join("old-0")).unwrap()),
            }])
            .unwrap(),
        );
        assert!(
            capture(&data, Path::new("/Users/synthetic"))
                .unwrap_err()
                .to_string()
                .contains("recovery")
        );
        recover(&data).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"original index");
        assert!(!stage.exists());
        put(&stage.join("old-0"), "discard this backup");
        put(&stage.join("committed"), "1");
        recover(&data).unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"original index");
        assert!(!stage.exists());
    }

    #[test]
    fn recovery_preserves_post_crash_user_changes_and_rejects_broad_targets() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Claude");
        let scope = scope(&data, "account");
        let target = scope.join(format!("{ID}.json"));
        put(&target, "installed index before crash");
        let stage = data.join(format!("{STAGE}fixture"));
        put(&stage.join("old-0"), "original index");
        let operation = Operation {
            target: target.strip_prefix(&data).unwrap().to_path_buf(),
            had_original: true,
            expected: tree_hash(&target).unwrap(),
            original: Some(tree_hash(&stage.join("old-0")).unwrap()),
        };
        put(
            &stage.join("journal.json"),
            serde_json::to_vec(&vec![operation]).unwrap(),
        );
        put(&target, "new user turn after crash");
        assert!(
            recover(&data)
                .unwrap_err()
                .to_string()
                .contains("changed after")
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"new user turn after crash"
        );
        assert_eq!(
            std::fs::read(stage.join("old-0")).unwrap(),
            b"original index"
        );
        let malicious = Operation {
            target: PathBuf::from(STORE),
            had_original: false,
            expected: "a".repeat(64),
            original: None,
        };
        put(
            &stage.join("journal.json"),
            serde_json::to_vec(&vec![malicious]).unwrap(),
        );
        assert!(recover(&data).is_err());
        assert!(target.exists());
    }

    #[test]
    fn oversized_native_file_is_refused_before_reading_or_allocating_its_contents() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Claude");
        let home = Path::new("/Users/source");
        let (_, body) = fixture(&data, home, false, false);
        let path = body.join("outputs/huge.bin");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(
            capture(&data, home)
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
        assert_eq!(std::fs::metadata(path).unwrap().len(), MAX_FILE_BYTES + 1);
    }

    #[test]
    fn native_working_directories_exist_even_when_no_files_were_generated() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("source");
        let home = Path::new("/Users/source");
        let (_, body) = fixture(&data, home, false, false);
        std::fs::remove_file(body.join("outputs/report.bin")).unwrap();
        std::fs::remove_file(body.join("uploads/input.txt")).unwrap();
        let incoming = capture(&data, home).unwrap().remove(0);
        let destination = temp.path().join("destination");
        let scope = scope(&destination, "target-account");
        restore(&destination, Path::new("/Users/new"), &incoming, ID, &[]).unwrap();
        assert!(scope.join(ID).join("outputs").is_dir());
        assert!(scope.join(ID).join("uploads").is_dir());
        assert!(scope.join(ID).join("host-cwd").is_dir());
    }

    #[test]
    fn changed_project_context_gets_native_snapshot_and_older_chat_cannot_roll_it_back() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("source");
        let home = Path::new("/Users/source");
        let (source_scope, _) = fixture(&data, home, false, false);
        let index_path = source_scope.join(format!("{ID}.json"));
        let mut index = json(&index_path).unwrap();
        index["spaceId"] = json!("aaaa-bbbb");
        put(&index_path, serde_json::to_vec(&index).unwrap());
        put(
            &source_scope.join("spaces.json"),
            r#"{"spaces":[{"id":"aaaa-bbbb","name":"Project"}]}"#,
        );
        put(
            &source_scope.join("spaces/aaaa-bbbb/memory/MEMORY.md"),
            "old memory",
        );
        let original = capture(&data, home).unwrap().remove(0);
        let dest = temp.path().join("destination");
        let destination_scope = scope(&dest, "destination");
        restore(&dest, Path::new("/Users/new"), &original, ID, &[]).unwrap();
        put(
            &source_scope.join("spaces/aaaa-bbbb/memory/MEMORY.md"),
            "new memory",
        );
        let advanced = capture(&data, home).unwrap().remove(0);
        restore(&dest, Path::new("/Users/new"), &advanced, ID, &[]).unwrap();
        let installed = json(&destination_scope.join(format!("{ID}.json"))).unwrap();
        let new_space = installed["spaceId"].as_str().unwrap();
        assert_ne!(new_space, "aaaa-bbbb");
        assert_eq!(
            std::fs::read(
                destination_scope
                    .join("spaces")
                    .join(new_space)
                    .join("memory/MEMORY.md")
            )
            .unwrap(),
            b"new memory"
        );
        restore(&dest, Path::new("/Users/new"), &original, FORK, &[]).unwrap();
        assert_eq!(
            std::fs::read(
                destination_scope
                    .join("spaces")
                    .join(new_space)
                    .join("memory/MEMORY.md")
            )
            .unwrap(),
            b"new memory"
        );
        assert_eq!(
            std::fs::read(destination_scope.join("spaces/aaaa-bbbb/memory/MEMORY.md")).unwrap(),
            b"old memory"
        );
        assert_eq!(capture(&dest, Path::new("/Users/new")).unwrap().len(), 2);
    }

    #[test]
    fn engine_roundtrip_two_macs_multiple_accounts_and_continuations_has_no_echo() {
        use crate::chat_sync::engine::{self, Adapter, Provider, State};
        struct Native {
            data: PathBuf,
            home: PathBuf,
        }
        impl Adapter for Native {
            fn capture(&mut self) -> Result<Vec<NativeSession>> {
                capture(&self.data, &self.home)
            }
            fn restore(&mut self, session: &NativeSession, target: &str) -> Result<()> {
                restore(&self.data, &self.home, session, target, &[])
            }
            fn ready(&self) -> Result<bool> {
                Ok(true)
            }
        }
        fn cycle(
            root: &Path,
            state_path: &Path,
            state: &mut State,
            adapter: &mut Native,
        ) -> engine::Counts {
            reconcile(&adapter.data, &adapter.home, &[]).unwrap();
            engine::sync(root, state_path, state, Provider::Claude, adapter).unwrap()
        }
        fn append(data: &Path, scope: &Path, text: &str) {
            let path = scope
                .join(ID)
                .join(format!(".claude/projects/-sessions-fixture/{CLI}.jsonl"));
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            writeln!(
                file,
                "{}",
                serde_json::json!({"type":"assistant","message":{"content":text}})
            )
            .unwrap();
            assert!(data.exists());
        }
        let temp = tempfile::tempdir().unwrap();
        let cloud = temp.path().join("iCloud");
        std::fs::create_dir(&cloud).unwrap();
        let mut a = Native {
            data: temp.path().join("MacA/Claude"),
            home: PathBuf::from("/Users/a"),
        };
        let mut b = Native {
            data: temp.path().join("MacB/Claude"),
            home: PathBuf::from("/Users/b"),
        };
        fixture(&a.data, &a.home, false, false);
        let a_other = scope(&a.data, "account-other");
        scope(&b.data, "account-one");
        let b_other = scope(&b.data, "account-two");
        let (mut sa, mut sb) = (State::default(), State::default());
        let pa = temp.path().join("state-a.json");
        let pb = temp.path().join("state-b.json");
        assert_eq!(cycle(&cloud, &pa, &mut sa, &mut a).exported, 1);
        assert_eq!(cycle(&cloud, &pb, &mut sb, &mut b).imported, 1);
        append(
            &a.data,
            &a_other,
            "continued on a different account on Mac A",
        );
        assert_eq!(cycle(&cloud, &pa, &mut sa, &mut a).exported, 1);
        assert_eq!(cycle(&cloud, &pb, &mut sb, &mut b).imported, 1);
        append(&b.data, &b_other, "continued on the second Mac and account");
        assert_eq!(cycle(&cloud, &pb, &mut sb, &mut b).exported, 1);
        assert_eq!(cycle(&cloud, &pa, &mut sa, &mut a).imported, 1);
        let late = scope(&a.data, "newly-added-account");
        let settled_a = cycle(&cloud, &pa, &mut sa, &mut a);
        let settled_b = cycle(&cloud, &pb, &mut sb, &mut b);
        assert_eq!(
            (
                settled_a.exported,
                settled_a.imported,
                settled_b.exported,
                settled_b.imported
            ),
            (0, 0, 0, 0)
        );
        assert!(late.join(format!("{ID}.json")).exists());
        assert_eq!(
            capture(&a.data, &a.home).unwrap(),
            capture(&b.data, &b.home).unwrap()
        );
    }
}
