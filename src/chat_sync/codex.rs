//! Portable native Codex histories. Only explicitly allowed conversation tables
//! are exported. Auth, queues, goals, enrollments and process state stay local.
//! The caller must stop every writer before restore and retain a rollback copy.
use super::NativeSession;
use crate::{AppError, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{
    Connection, OpenFlags, params_from_iter,
    types::{Value as SqlValue, ValueRef},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    time::Duration,
};

const THREAD: &str = "__SWITCHBOARD_THREAD__";
const USER_HOME: &str = "__SWITCHBOARD_HOME__";
const CODEX_HOME: &str = "__SWITCHBOARD_CODEX_HOME__";
const ROLLOUT: &str = "__SWITCHBOARD_ROLLOUT__";
const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_CAPTURE_BYTES: usize = 512 * 1024 * 1024;
const MAX_ROWS: usize = 500_000;
const STATE_TABLES: &[&str] = &[
    "threads",
    "thread_dynamic_tools",
    "thread_artifacts",
    "thread_attachments",
    "thread_spawn_edges",
    "projects",
    "project_roots",
    "thread_sections",
];
const HISTORY_TABLES: &[&str] = &[
    "thread_turns",
    "thread_items",
    "thread_history_projection_state",
    "thread_realtime_items",
];

fn error(message: &str) -> AppError {
    AppError::Other(format!("Codex chat sync: {message}"))
}
fn db_error(_: rusqlite::Error) -> AppError {
    error("native database could not be read or updated; no database diagnostics are published")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(String),
}
impl Cell {
    fn text(&self) -> Option<&str> {
        if let Self::Text(s) = self {
            Some(s)
        } else {
            None
        }
    }
    fn sql(&self) -> Result<SqlValue> {
        Ok(match self {
            Self::Null => SqlValue::Null,
            Self::Integer(n) => SqlValue::Integer(*n),
            Self::Real(n) => SqlValue::Real(*n),
            Self::Text(s) => SqlValue::Text(s.clone()),
            Self::Blob(s) => SqlValue::Blob(
                STANDARD
                    .decode(s)
                    .map_err(|_| error("invalid native blob"))?,
            ),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Table {
    columns: Vec<String>,
    rows: Vec<Vec<Cell>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Attachment {
    path: String,
    bytes: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Payload {
    version: u32,
    state_database: String,
    tables: BTreeMap<String, Table>,
    history: BTreeMap<String, Table>,
    rollout: String,
    #[serde(default)]
    attachments: Vec<Attachment>,
}

fn identifier(s: &str) -> Result<String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(error("unsupported native column name"));
    }
    Ok(format!("\"{s}\""))
}
fn regular(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(error("native store contains an unsafe file"));
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(error("native store file is inaccessible")),
    }
}
fn database(home: &Path) -> Result<Option<PathBuf>> {
    if !home.exists() {
        return Ok(None);
    }
    let mut found = vec![];
    for entry in fs::read_dir(home).map_err(|_| error("native store is inaccessible"))? {
        let path = entry
            .map_err(|_| error("native store is inaccessible"))?
            .path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(n) = name
            .strip_prefix("state_")
            .and_then(|s| s.strip_suffix(".sqlite"))
            .and_then(|s| s.parse::<u32>().ok())
        {
            regular(&path)?;
            found.push((n, path));
        }
    }
    found.sort_by_key(|(n, _)| *n);
    Ok(found.pop().map(|(_, p)| p))
}
fn connection(path: &Path, writable: bool) -> Result<Connection> {
    regular(path)?;
    let conn = Connection::open_with_flags(
        path,
        if writable {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        },
    )
    .map_err(db_error)?;
    conn.busy_timeout(Duration::from_secs(2))
        .map_err(db_error)?;
    Ok(conn)
}
fn table_names(conn: &Connection, schema: &str) -> Result<BTreeSet<String>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT name FROM {}.sqlite_master WHERE type='table'",
            identifier(schema)?
        ))
        .map_err(db_error)?;
    stmt.query_map([], |r| r.get(0))
        .map_err(db_error)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db_error)
}
fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(&format!(
            "PRAGMA {}.table_info({})",
            identifier(schema)?,
            identifier(table)?
        ))
        .map_err(db_error)?;
    stmt.query_map([], |r| r.get(1))
        .map_err(db_error)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db_error)
}
fn read_table(
    conn: &Connection,
    schema: &str,
    name: &str,
    key: &str,
    value: &str,
    budget: &mut usize,
) -> Result<Table> {
    let cols = columns(conn, schema, name)?;
    if cols.is_empty() || !cols.iter().any(|c| c == key) {
        return Err(error("unsupported native conversation schema"));
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT * FROM {}.{} WHERE {} = ?1 ORDER BY rowid",
            identifier(schema)?,
            identifier(name)?,
            identifier(key)?
        ))
        .map_err(db_error)?;
    let mut rows = stmt.query([value]).map_err(db_error)?;
    let mut result = vec![];
    let mut bytes = 0;
    while let Some(row) = rows.next().map_err(db_error)? {
        let mut values = vec![];
        for i in 0..cols.len() {
            let item = row.get_ref(i).map_err(db_error)?;
            let size = match item {
                ValueRef::Text(v) | ValueRef::Blob(v) => v.len(),
                _ => 16,
            };
            *budget = budget.checked_sub(size).ok_or_else(|| error("native histories exceed the 512 MiB per-pass capture limit; no chats were omitted"))?;
            values.push(match item {
                ValueRef::Null => Cell::Null,
                ValueRef::Integer(v) => Cell::Integer(v),
                ValueRef::Real(v) => Cell::Real(v),
                ValueRef::Text(v) => {
                    bytes += v.len();
                    Cell::Text(
                        std::str::from_utf8(v)
                            .map_err(|_| error("native text is not UTF-8"))?
                            .into(),
                    )
                }
                ValueRef::Blob(v) => {
                    bytes += v.len();
                    Cell::Blob(STANDARD.encode(v))
                }
            });
        }
        if bytes > MAX_BYTES || result.len() >= MAX_ROWS {
            return Err(error(
                "native conversation exceeds the safety limit; nothing was truncated",
            ));
        }
        result.push(values);
    }
    Ok(Table {
        columns: cols,
        rows: result,
    })
}
fn field<'a>(table: &'a Table, row: &'a [Cell], name: &str) -> Option<&'a Cell> {
    table
        .columns
        .iter()
        .position(|c| c == name)
        .and_then(|i| row.get(i))
}
fn owned_path(home: &Path, path: &Path) -> Result<PathBuf> {
    let root = home
        .canonicalize()
        .map_err(|_| error("native store is inaccessible"))?;
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        home.join(path)
    };
    regular(&candidate)?;
    let physical = candidate
        .canonicalize()
        .map_err(|_| error("native rollout or attachment is missing"))?;
    if !physical.starts_with(&root) {
        return Err(error(
            "native rollout or attachment is outside its configured store",
        ));
    }
    Ok(physical)
}
fn read_file(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = opts
        .open(path)
        .map_err(|_| error("native file is inaccessible"))?;
    let before = file
        .metadata()
        .map_err(|_| error("native file is inaccessible"))?;
    if !before.is_file() || before.len() > MAX_BYTES as u64 {
        return Err(error(
            "native file exceeds the safety limit or is not a regular file",
        ));
    }
    let mut bytes = vec![];
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error("native file could not be read"))?;
    let after = fs::metadata(path).map_err(|_| error("native file changed during capture"))?;
    if bytes.len() > MAX_BYTES
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(error(
            "native file changed during capture; retry when the app is idle",
        ));
    }
    Ok(bytes)
}
fn user_home(home: &Path) -> PathBuf {
    let parts: Vec<_> = home.components().collect();
    // Custom CODEX_HOME directories still refer to the containing macOS/Linux user.
    if parts.len() >= 3 && matches!(parts[1], Component::Normal(s) if s == "Users" || s == "home") {
        return parts[..3].iter().collect();
    }
    home.parent().unwrap_or(home).to_path_buf()
}
fn map_path(value: &str, mappings: &[(String, String)]) -> String {
    mappings
        .iter()
        .filter(|(from, _)| {
            value == from || value.strip_prefix(from).is_some_and(|s| s.starts_with('/'))
        })
        .max_by_key(|(from, _)| from.len())
        .map(|(from, to)| format!("{to}{}", &value[from.len()..]))
        .unwrap_or_else(|| value.into())
}
fn is_path_key(key: &str) -> bool {
    matches!(
        key,
        "cwd"
            | "path"
            | "file_path"
            | "filePath"
            | "image_path"
            | "imagePath"
            | "workspace_path"
            | "workspacePath"
            | "rootPath"
            | "root_path"
            | "workingDirectory"
            | "worktreePath"
            | "working_directory"
    )
}
fn map_json(value: &mut Value, mappings: &[(String, String)]) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if is_path_key(key)
                    && let Value::String(s) = value
                {
                    *s = map_path(s, mappings);
                    continue;
                }
                if matches!(
                    key.as_str(),
                    "writable_roots" | "rootPaths" | "workspace_roots"
                ) && let Value::Array(values) = value
                {
                    for value in values {
                        if let Value::String(s) = value {
                            *s = map_path(s, mappings);
                        }
                    }
                    continue;
                }
                map_json(value, mappings);
            }
        }
        Value::Array(values) => {
            for value in values {
                map_json(value, mappings);
            }
        }
        _ => {}
    }
}
fn transform_rollout(
    text: &str,
    id_from: &str,
    id_to: &str,
    mappings: &[(String, String)],
) -> Result<(String, BTreeMap<i64, i64>)> {
    let mut out = String::new();
    let mut offsets = BTreeMap::new();
    let mut offset = 0i64;
    let mut meta_count = 0;
    for line in text.split_inclusive('\n') {
        offsets.insert(offset, out.len() as i64);
        offset += line.len() as i64;
        if line.trim().is_empty() {
            out.push_str(line);
            continue;
        }
        let mut value: Value = serde_json::from_str(line)
            .map_err(|_| error("native rollout is incomplete or invalid"))?;
        if value["type"] == "session_meta" {
            meta_count += 1;
            if meta_count == 1 && value["payload"]["id"].as_str() != Some(id_from) {
                return Err(error("native rollout identity does not match its catalog"));
            }
            if value["payload"]["id"].as_str() == Some(id_from) {
                value["payload"]["id"] = Value::String(id_to.into());
            }
        }
        map_json(&mut value, mappings);
        out.push_str(&serde_json::to_string(&value)?);
        out.push('\n');
    }
    if meta_count == 0 {
        return Err(error(
            "native rollout must contain a primary session identity",
        ));
    }
    offsets.insert(offset, out.len() as i64);
    Ok((out, offsets))
}
fn transform_table(
    table: &mut Table,
    name: &str,
    id_from: &str,
    id_to: &str,
    mappings: &[(String, String)],
    offsets: &BTreeMap<i64, i64>,
) -> Result<()> {
    for row in &mut table.rows {
        if row.len() != table.columns.len() {
            return Err(error("invalid portable native row"));
        }
        for (key, cell) in table.columns.iter().zip(row) {
            if key.contains("byte_offset")
                && let Cell::Integer(old) = cell
            {
                *old = *offsets.get(old).ok_or_else(|| {
                    error("native history byte offset is inconsistent with its rollout")
                })?;
            }
            let Cell::Text(text) = cell else { continue };
            if (key == "thread_id"
                || (name == "threads" && key == "id")
                || (name == "thread_spawn_edges" && key == "child_thread_id"))
                && text == id_from
            {
                *text = id_to.into();
            } else if key == "rollout_path" && name == "threads" {
                *text = ROLLOUT.into();
            } else if is_path_key(key) {
                *text = map_path(text, mappings);
            } else if matches!(
                key.as_str(),
                "item_json" | "payload" | "metadata" | "sandbox_policy"
            ) && let Ok(mut json) = serde_json::from_str::<Value>(text)
            {
                map_json(&mut json, mappings);
                *text = serde_json::to_string(&json)?;
            }
        }
    }
    Ok(())
}
fn attachment_paths(value: &Value, home: &Path, paths: &mut BTreeSet<PathBuf>) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                if is_path_key(key)
                    && let Some(s) = v.as_str()
                {
                    let path = PathBuf::from(s);
                    if [
                        "visualizations",
                        "generated_images",
                        "attachments",
                        "uploads",
                    ]
                    .iter()
                    .any(|dir| path.starts_with(home.join(dir)))
                    {
                        paths.insert(path);
                    }
                }
                attachment_paths(v, home, paths);
            }
        }
        Value::Array(a) => {
            for v in a {
                attachment_paths(v, home, paths);
            }
        }
        _ => {}
    }
}

fn check_unindexed_rollouts(home: &Path, indexed: &BTreeSet<String>) -> Result<()> {
    use std::io::Read;
    let mut folders = Vec::new();
    for name in ["sessions", "archived_sessions"] {
        let folder = home.join(name);
        if folder.exists() {
            folders.push((folder, 0usize));
        }
    }
    let mut entries = 0usize;
    while let Some((folder, depth)) = folders.pop() {
        if depth > 32 {
            return Err(error("native session folders exceed the safety limit"));
        }
        let meta = fs::symlink_metadata(&folder)
            .map_err(|_| error("native session folder is inaccessible"))?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(error("native session folder is unsafe"));
        }
        for entry in
            fs::read_dir(&folder).map_err(|_| error("native session folder is inaccessible"))?
        {
            entries += 1;
            if entries > MAX_ROWS {
                return Err(error("native session folders exceed the safety limit"));
            }
            let entry = entry.map_err(|_| error("native session folder is inaccessible"))?;
            let kind = entry
                .file_type()
                .map_err(|_| error("native session file is inaccessible"))?;
            if kind.is_symlink() {
                return Err(error("native session folder contains a symlink"));
            }
            if kind.is_dir() {
                folders.push((entry.path(), depth + 1));
                continue;
            }
            if entry.path().extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let mut prefix = Vec::new();
            let mut opts = fs::OpenOptions::new();
            opts.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
            }
            opts.open(entry.path())
                .map_err(|_| error("native session file is inaccessible"))?
                .take(256 * 1024)
                .read_to_end(&mut prefix)
                .map_err(|_| error("native session file is inaccessible"))?;
            let first = prefix
                .split(|byte| *byte == b'\n')
                .find(|line| !line.is_empty())
                .ok_or_else(|| error("native session file has no catalog identity"))?;
            let meta: Value = serde_json::from_slice(first)
                .map_err(|_| error("native session file has no catalog identity"))?;
            let id = meta["payload"]["id"]
                .as_str()
                .filter(|_| meta["type"] == "session_meta");
            if id.is_none_or(|id| !indexed.contains(id)) {
                if let Some(id) = id
                    && entry.path().parent()
                        == Some(home.join("sessions/switchboard-sync").as_path())
                {
                    use sha2::{Digest, Sha256};
                    let bytes = read_file(&entry.path())?;
                    let hash: String = Sha256::digest(&bytes)
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect();
                    if entry.file_name().to_str()
                        == Some(format!("rollout-{id}-{}.jsonl", &hash[..20]).as_str())
                    {
                        // A failed pre-commit import can leave its immutable
                        // rollout here. Keep it for retry; it is not a native
                        // catalog chat. Any subsequent append invalidates this
                        // content check and is reported as unindexed history.
                        continue;
                    }
                }
                return Err(error(
                    "unindexed native histories were found; let Codex discover these sessions before syncing so no local chat is silently omitted",
                ));
            }
        }
    }
    Ok(())
}

pub fn capture(home: &Path) -> Result<Vec<NativeSession>> {
    let Some(path) = database(home)? else {
        check_unindexed_rollouts(home, &BTreeSet::new())?;
        return Ok(vec![]);
    };
    let conn = connection(&path, false)?;
    let history_path = home.join("thread_history_1.sqlite");
    let has_history = regular(&history_path)?;
    if has_history {
        conn.execute(
            "ATTACH DATABASE ?1 AS history",
            [format!("file:{}?mode=ro", history_path.to_string_lossy())],
        )
        .map_err(db_error)?;
    }
    conn.execute_batch("BEGIN").map_err(db_error)?;
    let tables = table_names(&conn, "main")?;
    let history_tables = if has_history {
        table_names(&conn, "history")?
    } else {
        BTreeSet::new()
    };
    for (schema, known, discovered) in [
        ("main", STATE_TABLES, &tables),
        ("history", HISTORY_TABLES, &history_tables),
    ] {
        for table in discovered {
            if known.contains(&table.as_str()) {
                continue;
            }
            if columns(&conn, schema, table)?.iter().any(|c| {
                matches!(
                    c.as_str(),
                    "thread_id" | "parent_thread_id" | "child_thread_id"
                )
            }) {
                return Err(error(
                    "new native conversation tables require an adapter update; no partial history was exported",
                ));
            }
        }
    }
    if !tables.contains("threads") {
        return Err(error("native thread catalog is unsupported"));
    }
    let mut stmt = conn
        .prepare("SELECT id FROM threads ORDER BY id")
        .map_err(db_error)?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(db_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db_error)?;
    check_unindexed_rollouts(home, &ids.iter().cloned().collect())?;
    if ids.len() > MAX_ROWS {
        return Err(error("native catalog exceeds the safety limit"));
    }
    let mut mappings = vec![
        (home.to_string_lossy().into_owned(), CODEX_HOME.into()),
        (
            user_home(home).to_string_lossy().into_owned(),
            USER_HOME.into(),
        ),
    ];
    if let Ok(physical) = home.canonicalize() {
        mappings.push((physical.to_string_lossy().into_owned(), CODEX_HOME.into()));
    }
    let mut result = vec![];
    let mut budget = MAX_CAPTURE_BYTES;
    let mut encoded_budget = MAX_CAPTURE_BYTES;
    for id in ids {
        uuid::Uuid::parse_str(&id).map_err(|_| error("unsupported native thread identity"))?;
        let thread = read_table(&conn, "main", "threads", "id", &id, &mut budget)?;
        let row = thread
            .rows
            .first()
            .ok_or_else(|| error("native thread disappeared"))?;
        let raw_path = field(&thread, row, "rollout_path")
            .and_then(Cell::text)
            .ok_or_else(|| error("native thread has no rollout path"))?;
        let rollout_path = owned_path(home, Path::new(raw_path))?;
        let bytes = read_file(&rollout_path)?;
        budget = budget.checked_sub(bytes.len()).ok_or_else(|| {
            error(
                "native histories exceed the 512 MiB per-pass capture limit; no chats were omitted",
            )
        })?;
        let original =
            std::str::from_utf8(&bytes).map_err(|_| error("native rollout is not UTF-8"))?;
        let (rollout, offsets) = transform_rollout(original, &id, THREAD, &mappings)?;
        let mut native: BTreeMap<String, Table> = BTreeMap::new();
        for (table, key, reference) in [
            ("projects", "id", "project_id"),
            ("project_roots", "project_id", "project_id"),
            ("thread_sections", "id", "thread_section_id"),
        ] {
            if let Some(value) = field(&thread, row, reference).and_then(Cell::text) {
                if !tables.contains(table) {
                    return Err(error("native project or section schema is incomplete"));
                }
                native.insert(
                    table.into(),
                    read_table(&conn, "main", table, key, value, &mut budget)?,
                );
            }
        }
        let paginated =
            field(&thread, row, "history_mode").and_then(Cell::text) == Some("paginated");
        native.insert("threads".into(), thread);
        for table in [
            "thread_dynamic_tools",
            "thread_artifacts",
            "thread_attachments",
            "thread_spawn_edges",
        ] {
            if tables.contains(table) {
                native.insert(
                    table.into(),
                    read_table(
                        &conn,
                        "main",
                        table,
                        if table == "thread_spawn_edges" {
                            "child_thread_id"
                        } else {
                            "thread_id"
                        },
                        &id,
                        &mut budget,
                    )?,
                );
            }
        }
        let mut history: BTreeMap<String, Table> = BTreeMap::new();
        for table in HISTORY_TABLES {
            if history_tables.contains(*table) {
                history.insert(
                    (*table).into(),
                    read_table(&conn, "history", table, "thread_id", &id, &mut budget)?,
                );
            } else if paginated
                && matches!(
                    *table,
                    "thread_turns" | "thread_items" | "thread_history_projection_state"
                )
            {
                return Err(error(
                    "paginated history is missing; sync will not substitute a partial transcript",
                ));
            }
        }
        let mut attached = BTreeSet::new();
        for line in original.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                attachment_paths(&v, home, &mut attached);
            }
        }
        for table in native.values().chain(history.values()) {
            for row in &table.rows {
                for cell in row {
                    if let Cell::Text(s) = cell
                        && let Ok(v) = serde_json::from_str::<Value>(s)
                    {
                        attachment_paths(&v, home, &mut attached);
                    }
                }
            }
        }
        let mut attachments = vec![];
        let mut total = bytes.len();
        for p in attached {
            // Only existing explicit files in allowlisted native asset stores.
            if !p.exists() || p.is_dir() {
                continue;
            }
            let data = read_file(&owned_path(home, &p)?)?;
            total += data.len();
            budget = budget.checked_sub(data.len()).ok_or_else(|| error("native histories exceed the 512 MiB per-pass capture limit; no chats were omitted"))?;
            if total > MAX_BYTES {
                return Err(error(
                    "native attachments exceed the safety limit; nothing was truncated",
                ));
            }
            attachments.push(Attachment {
                path: map_path(&p.to_string_lossy(), &mappings),
                bytes: STANDARD.encode(data),
            });
        }
        for (name, table) in native.iter_mut().chain(history.iter_mut()) {
            transform_table(table, name, &id, THREAD, &mappings, &offsets)?;
        }
        if read_file(&rollout_path)? != bytes {
            return Err(error(
                "native rollout changed during capture; retry when the app is idle",
            ));
        }
        let payload = Payload {
            version: 1,
            state_database: path.file_name().unwrap().to_string_lossy().into_owned(),
            tables: native,
            history,
            rollout,
            attachments,
        };
        struct Counter<'a>(&'a mut usize);
        impl std::io::Write for Counter<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                *self.0 = self
                    .0
                    .checked_sub(bytes.len())
                    .ok_or_else(|| std::io::Error::other("capture limit"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(Counter(&mut encoded_budget), &payload).map_err(|_| {
            error(
                "native histories exceed the 512 MiB per-pass capture limit; no chats were omitted",
            )
        })?;
        result.push(NativeSession {
            id,
            payload: serde_json::to_value(payload)?,
        });
    }
    Ok(result)
}

fn validate_table(conn: &Connection, schema: &str, name: &str, table: &Table) -> Result<()> {
    let actual = columns(conn, schema, name)?;
    if table.columns != actual {
        return Err(error(
            "source and destination native schemas differ; update both Codex installations before syncing",
        ));
    }
    if table.rows.iter().any(|row| row.len() != actual.len()) {
        return Err(error("invalid portable native rows"));
    }
    Ok(())
}
fn insert_rows(
    conn: &Connection,
    schema: &str,
    name: &str,
    table: &Table,
    ignore: bool,
) -> Result<()> {
    if table.rows.is_empty() {
        return Ok(());
    }
    let names = table
        .columns
        .iter()
        .map(|c| identifier(c))
        .collect::<Result<Vec<_>>>()?
        .join(",");
    let args = vec!["?"; table.columns.len()].join(",");
    let sql = format!(
        "INSERT {} INTO {}.{} ({names}) VALUES ({args})",
        if ignore { "OR IGNORE" } else { "" },
        identifier(schema)?,
        identifier(name)?
    );
    let mut stmt = conn.prepare(&sql).map_err(db_error)?;
    for row in &table.rows {
        let values = row.iter().map(Cell::sql).collect::<Result<Vec<_>>>()?;
        stmt.execute(params_from_iter(values)).map_err(db_error)?;
    }
    Ok(())
}
fn publish(home: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let relative = path
        .strip_prefix(home)
        .map_err(|_| error("destination file escapes its native store"))?;
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(error("unsafe destination file path"));
    }
    let parent = relative
        .parent()
        .ok_or_else(|| error("invalid destination path"))?;
    let mut directory = home.to_path_buf();
    // Check every component below the injected root, including existing parents:
    // checking only the leaf would follow nested symlinks outside CODEX_HOME.
    for part in parent.components() {
        directory.push(part);
        match fs::symlink_metadata(&directory) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err(error("unsafe destination directory")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&directory)
                    .map_err(|_| error("destination directory could not be created"))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                        .map_err(|_| error("destination permissions could not be set"))?;
                }
            }
            Err(_) => return Err(error("destination directory is inaccessible")),
        }
    }
    if regular(path)? {
        if read_file(path)? == bytes {
            return Ok(());
        }
        return Err(error(
            "destination asset already contains different data; both copies were preserved",
        ));
    }
    let mut temp = tempfile::NamedTempFile::new_in(&directory)
        .map_err(|_| error("could not stage native file"))?;
    temp.write_all(bytes)
        .map_err(|_| error("could not stage native file"))?;
    temp.as_file()
        .sync_all()
        .map_err(|_| error("could not flush native file"))?;
    temp.persist_noclobber(path)
        .map_err(|_| error("native file could not be published without replacement"))?;
    #[cfg(unix)]
    {
        // Every new directory entry must be durable before SQLite can point at
        // the immutable rollout, including a newly-created sessions subtree.
        let mut current = directory.as_path();
        loop {
            fs::File::open(current)
                .and_then(|f| f.sync_all())
                .map_err(|_| error("native directory could not be flushed"))?;
            if current == home {
                break;
            }
            current = current
                .parent()
                .ok_or_else(|| error("native directory escaped its store"))?;
        }
    }
    Ok(())
}

/// SQLite's super-journal makes a transaction across attached on-disk databases
/// crash-atomic only in rollback-journal mode. Never rely on a multi-WAL commit.
/// A failed mode change (for example another app opening the database) aborts
/// before mutations. A crash can leave DELETE mode selected, which is itself a
/// fully supported native SQLite mode and preserves the committed histories.
fn atomic_native_write(
    conn: &mut Connection,
    has_history: bool,
    write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<()>,
) -> Result<()> {
    let schemas: &[&str] = if has_history {
        &["main", "history"]
    } else {
        &["main"]
    };
    let mut previous = Vec::new();
    let result = (|| {
        for schema in schemas {
            let mode: String = conn
                .query_row(&format!("PRAGMA {schema}.journal_mode"), [], |r| r.get(0))
                .map_err(db_error)?;
            if !matches!(mode.as_str(), "wal" | "delete" | "truncate" | "persist") {
                return Err(error("destination SQLite journal mode is not durable"));
            }
            previous.push((*schema, mode));
            let selected: String = conn
                .query_row(&format!("PRAGMA {schema}.journal_mode=DELETE"), [], |r| {
                    r.get(0)
                })
                .map_err(db_error)?;
            if selected != "delete" {
                return Err(error(
                    "destination database is busy; quit Codex before importing",
                ));
            }
            conn.execute_batch(&format!("PRAGMA {schema}.synchronous=FULL"))
                .map_err(db_error)?;
        }
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        write(&tx)?;
        tx.commit().map_err(db_error)
    })();
    for (schema, mode) in previous.into_iter().rev() {
        // Restoring WAL is optional for correctness; the complete native data is
        // already durable. Busy restoration leaves safe DELETE mode in place.
        let _ = conn.query_row(&format!("PRAGMA {schema}.journal_mode={mode}"), [], |r| {
            r.get::<_, String>(0)
        });
    }
    result
}

fn target_fingerprint(
    conn: &Connection,
    home: &Path,
    id: &str,
    has_history: bool,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    struct Writer<'a>(&'a mut Sha256);
    impl std::io::Write for Writer<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut digest = Sha256::new();
    let mut budget = MAX_CAPTURE_BYTES;
    let thread = read_table(conn, "main", "threads", "id", id, &mut budget)?;
    serde_json::to_writer(Writer(&mut digest), &thread)?;
    if let Some(row) = thread.rows.first() {
        let path = field(&thread, row, "rollout_path")
            .and_then(Cell::text)
            .ok_or_else(|| error("destination native thread has no rollout path"))?;
        let bytes = read_file(&owned_path(home, Path::new(path))?)?;
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    let names = table_names(conn, "main")?;
    for name in [
        "thread_dynamic_tools",
        "thread_artifacts",
        "thread_attachments",
        "thread_spawn_edges",
    ] {
        if names.contains(name) {
            let key = if name == "thread_spawn_edges" {
                "child_thread_id"
            } else {
                "thread_id"
            };
            serde_json::to_writer(
                Writer(&mut digest),
                &read_table(conn, "main", name, key, id, &mut budget)?,
            )?;
        }
    }
    if has_history {
        let names = table_names(conn, "history")?;
        for name in HISTORY_TABLES {
            if names.contains(*name) {
                serde_json::to_writer(
                    Writer(&mut digest),
                    &read_table(conn, "history", name, "thread_id", id, &mut budget)?,
                )?;
            }
        }
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub fn restore(
    home: &Path,
    session: &NativeSession,
    target_id: &str,
    mappings: &[(String, String)],
) -> Result<()> {
    restore_if_idle(home, session, target_id, mappings, &|| Ok(true))
}

/// Production restore rechecks process readiness after staging and within the
/// SQLite write transaction. Destination changes during staging are preserved.
pub fn restore_if_idle(
    home: &Path,
    session: &NativeSession,
    target_id: &str,
    mappings: &[(String, String)],
    ready: &dyn Fn() -> Result<bool>,
) -> Result<()> {
    if !ready()? {
        return Err(error("waiting for Codex and its CLI sessions to close"));
    }
    uuid::Uuid::parse_str(target_id).map_err(|_| error("invalid destination thread identity"))?;
    let mut payload: Payload = serde_json::from_value(session.payload.clone())
        .map_err(|_| error("invalid portable native session"))?;
    if payload.version != 1
        || payload
            .tables
            .keys()
            .any(|n| !STATE_TABLES.contains(&n.as_str()))
        || payload
            .history
            .keys()
            .any(|n| !HISTORY_TABLES.contains(&n.as_str()))
    {
        return Err(error("unsupported portable native session"));
    }
    if payload.rollout.len() > MAX_BYTES {
        return Err(error("portable native rollout exceeds the safety limit"));
    }
    let source_thread = payload
        .tables
        .get("threads")
        .ok_or_else(|| error("portable native session has no thread"))?;
    if source_thread.rows.len() != 1 || source_thread.rows[0].len() != source_thread.columns.len() {
        return Err(error(
            "portable native session must contain one complete thread",
        ));
    }
    if field(source_thread, &source_thread.rows[0], "history_mode").and_then(Cell::text)
        == Some("paginated")
        && [
            "thread_turns",
            "thread_items",
            "thread_history_projection_state",
        ]
        .iter()
        .any(|name| !payload.history.contains_key(*name))
    {
        return Err(error("portable paginated history is incomplete"));
    }
    let path = database(home)?.ok_or_else(|| error("open Codex once on this Mac to initialize its native store, then quit it before importing"))?;
    if path.file_name().and_then(|n| n.to_str()) != Some(&payload.state_database) {
        return Err(error(
            "native database versions differ; update both Codex installations",
        ));
    }
    let mut conn = connection(&path, true)?;
    let history_path = home.join("thread_history_1.sqlite");
    if !payload.history.is_empty() {
        if !regular(&history_path)? {
            return Err(error(
                "destination paginated history is not initialized; open Codex once and retry after quitting",
            ));
        }
        conn.execute(
            "ATTACH DATABASE ?1 AS history",
            [history_path.to_string_lossy().as_ref()],
        )
        .map_err(db_error)?;
    }
    for (name, table) in &payload.tables {
        validate_table(&conn, "main", name, table)?;
    }
    for (name, table) in &payload.history {
        validate_table(&conn, "history", name, table)?;
    }
    conn.execute_batch("BEGIN").map_err(db_error)?;
    let before = target_fingerprint(&conn, home, target_id, !payload.history.is_empty());
    conn.execute_batch("ROLLBACK").map_err(db_error)?;
    let before = before?;
    let mut maps = mappings.to_vec();
    // The exported schema uses a portable user-home token. Accept the user's
    // original absolute /Users/name or /home/name prefix as a map source too.
    for (from, to) in mappings {
        let components: Vec<_> = Path::new(from).components().collect();
        if components.len() >= 3
            && matches!(components[1], Component::Normal(s) if s == "Users" || s == "home")
        {
            let prefix: PathBuf = components[..3].iter().collect();
            if let Some(suffix) = from.strip_prefix(prefix.to_string_lossy().as_ref()) {
                maps.push((format!("{USER_HOME}{suffix}"), to.clone()));
            }
        }
    }
    maps.push((CODEX_HOME.into(), home.to_string_lossy().into_owned()));
    if !maps.iter().any(|(key, _)| key == USER_HOME) {
        maps.push((
            USER_HOME.into(),
            user_home(home).to_string_lossy().into_owned(),
        ));
    }
    use sha2::{Digest, Sha256};
    let mut asset_writes = Vec::new();
    let mut attachment_bytes = 0usize;
    for attachment in &payload.attachments {
        let mut dest = PathBuf::from(map_path(&attachment.path, &maps));
        let relative = dest
            .strip_prefix(home)
            .map_err(|_| error("portable attachment escapes the native store"))?;
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
            || ![
                "visualizations",
                "generated_images",
                "attachments",
                "uploads",
            ]
            .iter()
            .any(|dir| relative.starts_with(dir))
        {
            return Err(error(
                "portable attachment is outside supported native asset folders",
            ));
        }
        let bytes = STANDARD
            .decode(&attachment.bytes)
            .map_err(|_| error("invalid portable attachment"))?;
        attachment_bytes = attachment_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| error("portable attachments exceed the safety limit"))?;
        if attachment_bytes > MAX_BYTES {
            return Err(error("portable attachments exceed the safety limit"));
        }
        if regular(&dest)? && read_file(&owned_path(home, &dest)?)? != bytes {
            let digest: String = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let name = dest
                .file_name()
                .ok_or_else(|| error("portable attachment has no filename"))?
                .to_owned();
            dest = home
                .join("attachments/switchboard-sync")
                .join(digest)
                .join(name);
            maps.push((attachment.path.clone(), dest.to_string_lossy().into_owned()));
        }
        asset_writes.push((dest, bytes));
    }
    let (rollout, offsets) = transform_rollout(&payload.rollout, THREAD, target_id, &maps)?;
    for (name, table) in payload.tables.iter_mut().chain(payload.history.iter_mut()) {
        transform_table(table, name, THREAD, target_id, &maps, &offsets)?;
    }
    // Immutable revision paths ensure a failed transaction never changes an existing chat.
    let digest = Sha256::digest(rollout.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let rollout_path = home
        .join("sessions/switchboard-sync")
        .join(format!("rollout-{target_id}-{}.jsonl", &digest[..20]));
    let thread = payload
        .tables
        .get_mut("threads")
        .ok_or_else(|| error("portable native session has no thread"))?;
    if thread.rows.len() != 1 {
        return Err(error("portable native session must contain one thread"));
    }
    let index = thread
        .columns
        .iter()
        .position(|c| c == "rollout_path")
        .ok_or_else(|| error("portable native thread has no rollout path"))?;
    thread.rows[0][index] = Cell::Text(rollout_path.to_string_lossy().into_owned());
    if field(thread, &thread.rows[0], "id").and_then(Cell::text) != Some(target_id) {
        return Err(error("portable native thread identity is invalid"));
    }
    // Artifact IDs are globally unique, while history item IDs are scoped by
    // thread. A conflict fork therefore needs independent artifact identities.
    if target_id != session.id {
        for name in ["thread_artifacts", "thread_attachments"] {
            if let Some(table) = payload.tables.get_mut(name)
                && let Some(index) = table.columns.iter().position(|c| c == "id")
            {
                for row in &mut table.rows {
                    if let Cell::Text(old) = &row[index] {
                        let hash = Sha256::digest(format!("{target_id}/{old}").as_bytes());
                        let mut bytes = [0u8; 16];
                        bytes.copy_from_slice(&hash[..16]);
                        row[index] = Cell::Text(uuid::Uuid::from_bytes(bytes).to_string());
                    }
                }
            }
        }
    }
    for (name, table) in payload.tables.iter().chain(payload.history.iter()) {
        let key = if name == "thread_spawn_edges" {
            "child_thread_id"
        } else {
            "thread_id"
        };
        if let Some(idx) = table.columns.iter().position(|c| c == key)
            && table
                .rows
                .iter()
                .any(|row| row[idx].text() != Some(target_id))
        {
            return Err(error(
                "portable native session contains an unrelated thread",
            ));
        }
    }
    for (dest, bytes) in asset_writes {
        publish(home, &dest, &bytes)?;
    }
    publish(home, &rollout_path, rollout.as_bytes())?;
    if !ready()? {
        return Err(error(
            "Codex opened during sync; native history was not modified",
        ));
    }
    atomic_native_write(&mut conn, !payload.history.is_empty(), |tx| {
        if !ready()? {
            return Err(error(
                "Codex opened during sync; native history was not modified",
            ));
        }
        if target_fingerprint(tx, home, target_id, !payload.history.is_empty())? != before {
            return Err(error(
                "destination history changed during sync; all local changes were preserved, retry after Codex is closed",
            ));
        }
        // Existing destination project definitions are authoritative; importing one
        // conversation must never rename unrelated local projects or sections.
        for name in ["projects", "project_roots", "thread_sections"] {
            if let Some(table) = payload.tables.get(name) {
                insert_rows(tx, "main", name, table, true)?;
            }
        }
        for name in [
            "thread_dynamic_tools",
            "thread_artifacts",
            "thread_attachments",
            "thread_spawn_edges",
        ] {
            if payload.tables.contains_key(name) {
                tx.execute(
                    &format!(
                        "DELETE FROM {} WHERE {}=?1",
                        identifier(name)?,
                        if name == "thread_spawn_edges" {
                            "child_thread_id"
                        } else {
                            "thread_id"
                        }
                    ),
                    [target_id],
                )
                .map_err(db_error)?;
            }
        }
        // UPDATE rather than REPLACE avoids cascading removal of related local data.
        let thread = &payload.tables["threads"];
        let names = thread
            .columns
            .iter()
            .map(|c| identifier(c))
            .collect::<Result<Vec<_>>>()?;
        let assignments = thread
            .columns
            .iter()
            .filter(|c| c.as_str() != "id")
            .map(|c| identifier(c).map(|q| format!("{q}=excluded.{q}")))
            .collect::<Result<Vec<_>>>()?
            .join(",");
        let sql = format!(
            "INSERT INTO threads ({}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {assignments}",
            names.join(","),
            vec!["?"; names.len()].join(",")
        );
        tx.execute(
            &sql,
            params_from_iter(
                thread.rows[0]
                    .iter()
                    .map(Cell::sql)
                    .collect::<Result<Vec<_>>>()?,
            ),
        )
        .map_err(db_error)?;
        for name in [
            "thread_dynamic_tools",
            "thread_artifacts",
            "thread_attachments",
            "thread_spawn_edges",
        ] {
            if let Some(table) = payload.tables.get(name) {
                insert_rows(tx, "main", name, table, false)?;
            }
        }
        for (name, table) in &payload.history {
            tx.execute(
                &format!(
                    "DELETE FROM history.{} WHERE thread_id=?1",
                    identifier(name)?
                ),
                [target_id],
            )
            .map_err(db_error)?;
            insert_rows(tx, "history", name, table, false)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const ID: &str = "11111111-1111-4111-8111-111111111111";
    const FORK: &str = "22222222-2222-4222-8222-222222222222";
    fn schema(home: &Path) {
        fs::create_dir_all(home).unwrap();
        Connection::open(home.join("state_5.sqlite")).unwrap().execute_batch("
            CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, cwd TEXT NOT NULL, title TEXT, history_mode TEXT, archived INTEGER, project_id TEXT, thread_section_id TEXT, updated_at INTEGER, sandbox_policy TEXT);
            CREATE TABLE projects(id TEXT PRIMARY KEY, name TEXT, metadata TEXT);
            CREATE TABLE project_roots(project_id TEXT, position INTEGER, path TEXT, PRIMARY KEY(project_id,position));
            CREATE TABLE thread_sections(id TEXT PRIMARY KEY, name TEXT);
            CREATE TABLE thread_dynamic_tools(thread_id TEXT, position INTEGER, name TEXT, input_schema TEXT, PRIMARY KEY(thread_id,position));
            CREATE TABLE thread_artifacts(id TEXT PRIMARY KEY, thread_id TEXT, payload TEXT);
            CREATE TABLE thread_spawn_edges(child_thread_id TEXT PRIMARY KEY, parent_thread_id TEXT, status TEXT);
            CREATE TABLE remote_control_enrollments(account_id TEXT, credential TEXT);
            INSERT INTO remote_control_enrollments VALUES('fixture-account', 'fixture-secret');
        ").unwrap();
        Connection::open(home.join("thread_history_1.sqlite")).unwrap().execute_batch("
            CREATE TABLE thread_turns(thread_id TEXT, turn_id TEXT, rollout_ordinal INTEGER, rollout_byte_offset INTEGER, rollout_end_byte_offset INTEGER, PRIMARY KEY(thread_id,turn_id));
            CREATE TABLE thread_items(thread_id TEXT, turn_id TEXT, item_id TEXT, rollout_ordinal INTEGER, created_at_ms INTEGER, item_json TEXT, item_type TEXT, PRIMARY KEY(thread_id,turn_id,item_id));
            CREATE TABLE thread_history_projection_state(thread_id TEXT PRIMARY KEY, next_rollout_byte_offset INTEGER, next_rollout_ordinal INTEGER);
            CREATE TABLE thread_realtime_items(thread_id TEXT, item_id TEXT, item_json TEXT, PRIMARY KEY(thread_id,item_id));
        ").unwrap();
    }
    fn seed(home: &Path) -> String {
        schema(home);
        let workspace = home.parent().unwrap().join("project");
        let image = home.join("generated_images/example.png");
        fs::create_dir_all(image.parent().unwrap()).unwrap();
        fs::write(&image, b"synthetic-image").unwrap();
        let lines = [
            json!({"timestamp":"2026-09-28T10:00:00Z","type":"session_meta","payload":{"id":ID,"cwd":workspace,"source":"cli"}}),
            json!({"type":"response_item","payload":{"type":"reasoning","encrypted_content":"synthetic-reasoning"}}),
            json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"tool-1","arguments":"{\"cmd\":\"pwd\"}"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("Historical literal: {}",workspace.display())},{"type":"localImage","path":image}]}}),
        ];
        let text = lines.iter().map(|v| format!("{v}\n")).collect::<String>();
        let rollout = home.join("sessions/native.jsonl");
        fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        fs::write(&rollout, &text).unwrap();
        fs::write(home.join("auth.json"), "fixture-auth-must-not-transfer").unwrap();
        let db = Connection::open(home.join("state_5.sqlite")).unwrap();
        db.execute("INSERT INTO threads VALUES(?1,?2,?3,'Full native conversation','paginated',1,'project-1','section-1',100,'{\"type\":\"read-only\"}')", rusqlite::params![ID,rollout.to_str().unwrap(),workspace.to_str().unwrap()]).unwrap();
        db.execute(
            "INSERT INTO projects VALUES('project-1','My project','{}')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO project_roots VALUES('project-1',0,?1)",
            [workspace.to_str().unwrap()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO thread_sections VALUES('section-1','Research')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO thread_dynamic_tools VALUES(?1,0,'fixture-tool','{}')",
            [ID],
        )
        .unwrap();
        db.execute(
            "INSERT INTO thread_artifacts VALUES('artifact-1',?1,?2)",
            [ID, json!({"path":image}).to_string().as_str()],
        )
        .unwrap();
        let db = Connection::open(home.join("thread_history_1.sqlite")).unwrap();
        db.execute(
            "INSERT INTO thread_turns VALUES(?1,'turn-1',0,0,?2)",
            rusqlite::params![ID, text.len() as i64],
        )
        .unwrap();
        for (i, kind) in [
            "userMessage",
            "reasoning",
            "commandExecution",
            "agentMessage",
        ]
        .iter()
        .enumerate()
        {
            let item = json!({"type":kind,"id":format!("item-{i}"),"text":format!("Native {kind}"),"cwd":workspace});
            db.execute(
                "INSERT INTO thread_items VALUES(?1,'turn-1',?2,?3,100,?4,?5)",
                rusqlite::params![ID, format!("item-{i}"), i as i64, item.to_string(), kind],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO thread_history_projection_state VALUES(?1,?2,4)",
            rusqlite::params![ID, text.len() as i64],
        )
        .unwrap();
        db.execute("INSERT INTO thread_realtime_items VALUES(?1,'realtime-1','{\"type\":\"realtime\",\"text\":\"audio transcript\"}')", [ID]).unwrap();
        text
    }
    fn homes() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("old-user/.codex");
        let dest = temp.path().join("new-user/.codex");
        (temp, source, dest)
    }
    #[test]
    fn native_roundtrip_keeps_tools_reasoning_archive_projects_attachments_and_offsets() {
        let (_temp, source, dest) = homes();
        let original = seed(&source);
        schema(&dest);
        let sessions = capture(&source).unwrap();
        assert_eq!(sessions.len(), 1);
        let portable = serde_json::to_string(&sessions).unwrap();
        assert!(!portable.contains("fixture-auth-must-not-transfer"));
        assert!(!portable.contains("fixture-secret"));
        assert!(portable.contains("commandExecution"));
        assert!(portable.contains("synthetic-reasoning"));
        restore(&dest, &sessions[0], ID, &[]).unwrap();
        assert_eq!(capture(&dest).unwrap()[0], sessions[0]);
        let db = Connection::open(dest.join("state_5.sqlite")).unwrap();
        let (path, cwd, archived): (String, String, i64) = db
            .query_row(
                "SELECT rollout_path,cwd,archived FROM threads WHERE id=?1",
                [ID],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(cwd.starts_with(dest.parent().unwrap().to_str().unwrap()));
        assert_eq!(archived, 1);
        let restored = fs::read_to_string(path).unwrap();
        assert!(restored.contains(&format!(
            "Historical literal: {}",
            source.parent().unwrap().join("project").display()
        )));
        assert!(restored.contains("function_call"));
        assert_ne!(restored, original);
        assert_eq!(
            fs::read(dest.join("generated_images/example.png")).unwrap(),
            b"synthetic-image"
        );
        let history = Connection::open(dest.join("thread_history_1.sqlite")).unwrap();
        let end: i64 = history.query_row("SELECT next_rollout_byte_offset FROM thread_history_projection_state WHERE thread_id=?1", [ID], |r| r.get(0)).unwrap();
        assert_eq!(end, restored.len() as i64);
        assert_eq!(
            history
                .query_row(
                    "SELECT count(*) FROM thread_items WHERE thread_id=?1",
                    [ID],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            4
        );
        assert!(!dest.join("auth.json").exists());
    }
    #[test]
    fn fork_preserves_both_sessions_and_rekeys_global_artifact_identity() {
        let (_temp, source, dest) = homes();
        seed(&source);
        schema(&dest);
        let session = capture(&source).unwrap().remove(0);
        restore(&dest, &session, ID, &[]).unwrap();
        restore(&dest, &session, FORK, &[]).unwrap();
        let db = Connection::open(dest.join("state_5.sqlite")).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM threads", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM thread_artifacts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(capture(&dest).unwrap().len(), 2);
    }
    #[test]
    fn schema_mismatch_fails_before_native_write_and_missing_history_is_not_silently_omitted() {
        let (_temp, source, dest) = homes();
        seed(&source);
        schema(&dest);
        let session = capture(&source).unwrap().remove(0);
        Connection::open(dest.join("state_5.sqlite"))
            .unwrap()
            .execute("ALTER TABLE threads ADD COLUMN incompatible TEXT", [])
            .unwrap();
        assert!(restore(&dest, &session, ID, &[]).is_err());
        assert!(!dest.join("sessions").exists());
        fs::remove_file(source.join("thread_history_1.sqlite")).unwrap();
        assert!(capture(&source).is_err());
    }
    #[test]
    fn malicious_payload_cannot_import_unrelated_thread_or_overwrite_auth() {
        let (_temp, source, dest) = homes();
        seed(&source);
        schema(&dest);
        let session = capture(&source).unwrap().remove(0);
        let mut malicious = session.clone();
        malicious.payload["attachments"][0]["path"] =
            json!("__SWITCHBOARD_CODEX_HOME__/attachments/../../auth.json");
        assert!(restore(&dest, &malicious, ID, &[]).is_err());
        assert!(!dest.join("auth.json").exists());
        let mut malicious = session;
        malicious.payload["history"]["thread_items"]["rows"][0][0] =
            serde_json::to_value(Cell::Text(FORK.into())).unwrap();
        assert!(restore(&dest, &malicious, ID, &[]).is_err());
    }
    #[test]
    fn unknown_thread_table_and_incomplete_rollout_fail_closed() {
        let (_temp, source, _dest) = homes();
        seed(&source);
        Connection::open(source.join("state_5.sqlite"))
            .unwrap()
            .execute(
                "CREATE TABLE future_native_history(thread_id TEXT, data TEXT)",
                [],
            )
            .unwrap();
        assert!(capture(&source).is_err());
        Connection::open(source.join("state_5.sqlite"))
            .unwrap()
            .execute("DROP TABLE future_native_history", [])
            .unwrap();
        fs::write(source.join("sessions/native.jsonl"), "{\"type\":").unwrap();
        assert!(capture(&source).is_err());
    }
    #[test]
    fn custom_workspace_mapping_is_applied_to_metadata_not_historical_text() {
        let (_temp, source, dest) = homes();
        seed(&source);
        schema(&dest);
        let session = capture(&source).unwrap().remove(0);
        restore(
            &dest,
            &session,
            ID,
            &[(
                "__SWITCHBOARD_HOME__/project".into(),
                "/Volumes/Work/new-project".into(),
            )],
        )
        .unwrap();
        let db = Connection::open(dest.join("state_5.sqlite")).unwrap();
        assert_eq!(
            db.query_row("SELECT cwd FROM threads", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "/Volumes/Work/new-project"
        );
    }
    #[cfg(unix)]
    #[test]
    fn rollout_symlinks_and_destination_asset_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;
        let (_temp, source, dest) = homes();
        seed(&source);
        schema(&dest);
        let session = capture(&source).unwrap().remove(0);
        let external = source.parent().unwrap().join("outside");
        fs::create_dir(&external).unwrap();
        symlink(&external, dest.join("generated_images")).unwrap();
        assert!(restore(&dest, &session, ID, &[]).is_err());
        let rollout = source.join("sessions/native.jsonl");
        fs::rename(&rollout, source.join("saved.jsonl")).unwrap();
        symlink(source.join("saved.jsonl"), rollout).unwrap();
        assert!(capture(&source).is_err());
    }
    #[test]
    fn attached_wal_databases_use_rollback_journals_and_roll_back_together() {
        let (_temp, source, _dest) = homes();
        seed(&source);
        let mut db = Connection::open(source.join("state_5.sqlite")).unwrap();
        db.execute(
            "ATTACH DATABASE ?1 AS history",
            [source.join("thread_history_1.sqlite").to_str().unwrap()],
        )
        .unwrap();
        db.execute_batch("PRAGMA main.journal_mode=WAL; PRAGMA history.journal_mode=WAL;")
            .unwrap();
        let result = atomic_native_write(&mut db, true, |tx| {
            for schema in ["main", "history"] {
                assert_eq!(
                    tx.query_row(&format!("PRAGMA {schema}.journal_mode"), [], |r| r
                        .get::<_, String>(0))
                        .unwrap(),
                    "delete"
                );
            }
            tx.execute("UPDATE threads SET title='must roll back'", [])
                .unwrap();
            tx.execute("DELETE FROM history.thread_items", []).unwrap();
            Err(error(
                "synthetic injected failure after both databases changed",
            ))
        });
        assert!(result.is_err());
        assert_eq!(
            db.query_row("SELECT title FROM threads", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "Full native conversation"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM history.thread_items", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            4
        );
        for schema in ["main", "history"] {
            assert_eq!(
                db.query_row(&format!("PRAGMA {schema}.journal_mode"), [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                "wal"
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn nested_symlink_below_existing_asset_parent_cannot_escape_store() {
        use std::os::unix::fs::symlink;
        let (_temp, source, dest) = homes();
        seed(&source);
        schema(&dest);
        let mut session = capture(&source).unwrap().remove(0);
        let outside = source.parent().unwrap().join("outside/nested");
        fs::create_dir_all(&outside).unwrap();
        symlink(outside.parent().unwrap(), dest.join("generated_images")).unwrap();
        session.payload["attachments"][0]["path"] =
            json!("__SWITCHBOARD_CODEX_HOME__/generated_images/nested/example.png");
        assert!(restore(&dest, &session, ID, &[]).is_err());
        assert!(!outside.join("example.png").exists());
    }
    #[test]
    fn unindexed_histories_are_reported_instead_of_silently_omitted() {
        let (_temp, source, _dest) = homes();
        seed(&source);
        let orphan = fs::read_to_string(source.join("sessions/native.jsonl"))
            .unwrap()
            .replace(ID, FORK);
        fs::write(source.join("sessions/orphan.jsonl"), orphan).unwrap();
        assert!(capture(&source).is_err());
        fs::remove_file(source.join("state_5.sqlite")).unwrap();
        assert!(capture(&source).is_err());
    }
    #[test]
    fn two_mac_engine_with_native_adapter_fast_forwards_without_echo() {
        use super::super::engine::{Adapter, Provider, State, sync};
        struct Native(PathBuf);
        impl Adapter for Native {
            fn capture(&mut self) -> Result<Vec<NativeSession>> {
                capture(&self.0)
            }
            fn restore(&mut self, session: &NativeSession, target_id: &str) -> Result<()> {
                restore(&self.0, session, target_id, &[])
            }
            fn ready(&self) -> Result<bool> {
                Ok(true)
            }
        }
        let (temp, source, dest) = homes();
        seed(&source);
        schema(&dest);
        let cloud = temp.path().join("cloud");
        let first_receipt = temp.path().join("first/state.json");
        let second_receipt = temp.path().join("second/state.json");
        let mut first = State::default();
        let mut second = State::default();
        let mut a = Native(source.clone());
        let mut b = Native(dest.clone());
        assert_eq!(
            sync(&cloud, &first_receipt, &mut first, Provider::Codex, &mut a)
                .unwrap()
                .exported,
            1
        );
        assert_eq!(
            sync(
                &cloud,
                &second_receipt,
                &mut second,
                Provider::Codex,
                &mut b
            )
            .unwrap()
            .imported,
            1
        );
        let db = Connection::open(dest.join("state_5.sqlite")).unwrap();
        db.execute(
            "UPDATE threads SET title='continued on new Mac',updated_at=200 WHERE id=?1",
            [ID],
        )
        .unwrap();
        drop(db);
        let update = sync(
            &cloud,
            &second_receipt,
            &mut second,
            Provider::Codex,
            &mut b,
        )
        .unwrap();
        assert_eq!(
            (update.exported, update.imported, update.conflicts),
            (1, 0, 0)
        );
        let update = sync(&cloud, &first_receipt, &mut first, Provider::Codex, &mut a).unwrap();
        assert_eq!(
            (update.exported, update.imported, update.conflicts),
            (0, 1, 0)
        );
        assert_eq!(
            capture(&source).unwrap()[0].payload,
            capture(&dest).unwrap()[0].payload
        );
        for (receipt, state, native) in [
            (&first_receipt, &mut first, &mut a),
            (&second_receipt, &mut second, &mut b),
        ] {
            let state_on_disk = State::load(receipt).unwrap();
            *state = state_on_disk;
            let counts = sync(&cloud, receipt, state, Provider::Codex, native).unwrap();
            assert_eq!(
                (counts.exported, counts.imported, counts.conflicts),
                (0, 0, 0)
            );
        }
    }
    #[test]
    fn changed_native_attachment_is_remapped_without_overwriting_local_revision() {
        let (_temp, source, dest) = homes();
        seed(&source);
        schema(&dest);
        let initial = capture(&source).unwrap().remove(0);
        restore(&dest, &initial, ID, &[]).unwrap();
        fs::write(
            source.join("generated_images/example.png"),
            b"updated-synthetic-image",
        )
        .unwrap();
        let update = capture(&source).unwrap().remove(0);
        restore(&dest, &update, ID, &[]).unwrap();
        assert_eq!(
            fs::read(dest.join("generated_images/example.png")).unwrap(),
            b"synthetic-image"
        );
        let snapshot = capture(&dest).unwrap().remove(0);
        let body = snapshot.payload["rollout"].as_str().unwrap();
        assert!(body.contains("__SWITCHBOARD_CODEX_HOME__/attachments/switchboard-sync/"));
        let native: Payload = serde_json::from_value(snapshot.payload).unwrap();
        assert_eq!(native.attachments.len(), 1);
        assert_eq!(
            STANDARD.decode(&native.attachments[0].bytes).unwrap(),
            b"updated-synthetic-image"
        );
    }
    /// Uses only synthetic isolated homes. Requests never start a model turn;
    /// even connection warmup is directed to a loopback discard port.
    #[test]
    fn installed_codex_reads_restored_native_tool_history_and_archive() {
        use std::io::{BufRead, Write};
        use std::process::{Child, ChildStdin, Command, Stdio};
        use std::sync::mpsc::{Receiver, channel};
        let Some(binary) = [
            "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex",
            "/Applications/Codex.app/Contents/Resources/codex",
            "/opt/homebrew/bin/codex",
            "/usr/local/bin/codex",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file()) else {
            eprintln!("SKIP: installed Codex CLI unavailable for isolated native sync readback");
            return;
        };
        struct Rpc {
            child: Child,
            input: ChildStdin,
            output: Receiver<Value>,
            next: u64,
        }
        impl Rpc {
            fn start(binary: &Path, home: &Path) -> Self {
                fs::create_dir_all(home).unwrap();
                let mut child = Command::new(binary)
                    .args([
                        "app-server",
                        "--stdio",
                        "-c",
                        "model_provider=\"sync_fixture\"",
                        "-c",
                        "model_providers.sync_fixture.name=\"Synthetic sync test\"",
                        "-c",
                        "model_providers.sync_fixture.base_url=\"http://127.0.0.1:9/v1\"",
                        "-c",
                        "model_providers.sync_fixture.wire_api=\"responses\"",
                        "-c",
                        "model_providers.sync_fixture.requires_openai_auth=false",
                        "-c",
                        "cli_auth_credentials_store=\"file\"",
                        "-c",
                        "analytics.enabled=false",
                        "-c",
                        "check_for_update_on_startup=false",
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
                    .spawn()
                    .unwrap();
                let input = child.stdin.take().unwrap();
                let stdout = child.stdout.take().unwrap();
                let (send, output) = channel();
                std::thread::spawn(move || {
                    for line in std::io::BufReader::new(stdout)
                        .lines()
                        .map_while(std::result::Result::ok)
                    {
                        if let Ok(value) = serde_json::from_str(&line)
                            && send.send(value).is_err()
                        {
                            break;
                        }
                    }
                });
                let mut rpc = Self {
                    child,
                    input,
                    output,
                    next: 1,
                };
                rpc.request("initialize", json!({"clientInfo":{"name":"switchboard_sync_fixture","version":"1"},"capabilities":{"experimentalApi":true}}));
                writeln!(rpc.input, "{}", json!({"method":"initialized","params":{}})).unwrap();
                rpc.input.flush().unwrap();
                rpc
            }
            fn request(&mut self, method: &str, params: Value) -> Value {
                let id = self.next;
                self.next += 1;
                writeln!(
                    self.input,
                    "{}",
                    json!({"id":id,"method":method,"params":params})
                )
                .unwrap();
                self.input.flush().unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(20);
                loop {
                    let value = self
                        .output
                        .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                        .expect("synthetic Codex request timed out");
                    if value["id"] == id {
                        assert!(
                            value.get("error").is_none(),
                            "synthetic Codex method {method} was rejected"
                        );
                        return value["result"].clone();
                    }
                }
            }
        }
        impl Drop for Rpc {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        let (_temp, source, dest) = homes();
        fs::create_dir_all(&source).unwrap();
        let stamp = "2026-09-28T10:00:00Z";
        let turn = "33333333-3333-4333-8333-333333333333";
        let row =
            |kind: &str, payload: Value| json!({"timestamp":stamp,"type":kind,"payload":payload});
        let records = [
            row(
                "session_meta",
                json!({"id":ID,"timestamp":stamp,"cwd":source.parent().unwrap(),"originator":"switchboard-test","cli_version":"0.151.0","source":"cli"}),
            ),
            row(
                "event_msg",
                json!({"type":"task_started","turn_id":turn,"collaboration_mode_kind":"default"}),
            ),
            row(
                "event_msg",
                json!({"type":"user_message","message":"synthetic native user","images":[]}),
            ),
            row(
                "response_item",
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"synthetic native user"}]}),
            ),
            row(
                "event_msg",
                json!({"type":"web_search_begin","call_id":"synthetic-tool"}),
            ),
            row(
                "event_msg",
                json!({"type":"web_search_end","call_id":"synthetic-tool","query":"synthetic native tool content","action":{"type":"search","query":"synthetic native tool content","queries":null}}),
            ),
            row(
                "response_item",
                json!({"type":"function_call","name":"fixture_tool","call_id":"synthetic-call","arguments":"{\"synthetic\":true}"}),
            ),
            row(
                "response_item",
                json!({"type":"function_call_output","call_id":"synthetic-call","output":"synthetic full native tool result"}),
            ),
            row(
                "event_msg",
                json!({"type":"agent_message","message":"synthetic native assistant"}),
            ),
            row(
                "response_item",
                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"synthetic native assistant"}]}),
            ),
            row(
                "event_msg",
                json!({"type":"task_complete","turn_id":turn,"last_agent_message":"synthetic native assistant"}),
            ),
        ];
        let input = source.join("synthetic-input.jsonl");
        fs::write(
            &input,
            records.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        let mut rpc = Rpc::start(&binary, &source);
        let response = rpc.request("thread/fork",json!({"threadId":ID,"path":input,"cwd":source.parent().unwrap(),"excludeTurns":true,"deferGoalContinuation":true}));
        let id = response["thread"]["id"].as_str().unwrap().to_string();
        rpc.request(
            "thread/name/set",
            json!({"threadId":id,"name":"Synthetic complete native sync"}),
        );
        let expected = rpc.request("thread/read", json!({"threadId":id,"includeTurns":true}));
        let turns = expected["thread"]["turns"].clone();
        let visible = serde_json::to_string(&turns).unwrap();
        for marker in [
            "synthetic native user",
            "synthetic native assistant",
            "synthetic native tool content",
        ] {
            assert!(visible.contains(marker));
        }
        rpc.request("thread/archive", json!({"threadId":id}));
        drop(rpc);
        let mut rpc = Rpc::start(&binary, &dest);
        rpc.request("thread/list", json!({"limit":1}));
        drop(rpc);
        let sessions = capture(&source).unwrap();
        let session = sessions.iter().find(|s| s.id == id).unwrap();
        assert!(
            session.payload["rollout"]
                .as_str()
                .unwrap()
                .contains("synthetic full native tool result")
        );
        restore(&dest, session, &id, &[]).unwrap();
        let db = Connection::open(database(&dest).unwrap().unwrap()).unwrap();
        assert_eq!(
            db.query_row("SELECT archived FROM threads WHERE id=?1", [&id], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(db);
        let mut rpc = Rpc::start(&binary, &dest);
        let actual = rpc.request("thread/read", json!({"threadId":id,"includeTurns":true}));
        assert_eq!(
            actual["thread"]["name"],
            json!("Synthetic complete native sync")
        );
        assert_eq!(actual["thread"]["turns"], turns);
        assert!(!source.join("auth.json").exists());
        assert!(!dest.join("auth.json").exists());
    }
    #[test]
    fn destination_change_during_staging_is_never_overwritten() {
        use std::cell::Cell as Counter;
        for append_only in [false, true] {
            let (_temp, source, dest) = homes();
            seed(&source);
            schema(&dest);
            let session = capture(&source).unwrap().remove(0);
            restore(&dest, &session, ID, &[]).unwrap();
            let calls = Counter::new(0);
            let ready = || {
                let next = calls.get() + 1;
                calls.set(next);
                if next == 2 {
                    let db = Connection::open(dest.join("state_5.sqlite")).unwrap();
                    if append_only {
                        let path: String = db
                            .query_row("SELECT rollout_path FROM threads WHERE id=?1", [ID], |r| {
                                r.get(0)
                            })
                            .unwrap();
                        use std::io::Write;
                        writeln!(fs::OpenOptions::new().append(true).open(path).unwrap(), "{}", json!({"type":"event_msg","payload":{"type":"agent_message","message":"new local content after staging"}})).unwrap();
                    } else {
                        db.execute(
                            "UPDATE threads SET title='new local change after staging' WHERE id=?1",
                            [ID],
                        )
                        .unwrap();
                    }
                }
                Ok(true)
            };
            assert!(restore_if_idle(&dest, &session, ID, &[], &ready).is_err());
            let snapshot = capture(&dest).unwrap().remove(0);
            let serialized = serde_json::to_string(&snapshot).unwrap();
            assert!(serialized.contains(if append_only {
                "new local content after staging"
            } else {
                "new local change after staging"
            }));
        }
    }
    #[test]
    fn reopening_app_during_new_import_keeps_retryable_staging_out_of_catalog() {
        use std::cell::Cell as Counter;
        let (_temp, source, dest) = homes();
        seed(&source);
        schema(&dest);
        let session = capture(&source).unwrap().remove(0);
        let calls = Counter::new(0);
        let ready = || {
            calls.set(calls.get() + 1);
            Ok(calls.get() == 1)
        };
        assert!(restore_if_idle(&dest, &session, ID, &[], &ready).is_err());
        assert!(capture(&dest).unwrap().is_empty());
        restore_if_idle(&dest, &session, ID, &[], &|| Ok(true)).unwrap();
        assert_eq!(capture(&dest).unwrap().len(), 1);
    }
}
