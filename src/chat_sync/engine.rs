use super::NativeSession;
use crate::{AppError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub const VERSION: u32 = 1;
const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PACKAGES: usize = 100_000;
pub(super) const MAX_CAPTURE_BYTES: u64 = 512 * 1024 * 1024;

pub fn error(message: &str) -> AppError {
    AppError::Other(message.to_owned())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Codex,
    Claude,
}

impl Provider {
    pub fn key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    version: u32,
    provider: Provider,
    chat_id: String,
    parent: Option<String>,
    device_id: String,
    session: NativeSession,
}

/// Keep ancestry and a content fingerprint in memory, not every historical
/// transcript. The complete immutable object is read only when restoring it.
#[derive(Clone, Debug)]
struct Revision {
    chat_id: String,
    parent: Option<String>,
    path: PathBuf,
    session_fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Replica {
    provider: Provider,
    chat_id: String,
    local_id: String,
    revision: String,
    baseline: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingImport {
    provider: Provider,
    chat_id: String,
    local_id: String,
    revision: String,
    before: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub device_id: String,
    replicas: Vec<Replica>,
    pending: BTreeMap<Provider, PendingImport>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: VERSION,
            device_id: uuid::Uuid::new_v4().to_string(),
            replicas: vec![],
            pending: BTreeMap::new(),
        }
    }
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.try_exists()? {
            return Ok(Self::default());
        }
        let state: Self =
            serde_json::from_slice(&read_regular(path, 16 * 1024 * 1024)?).map_err(|_| {
                error("The local sync record is unreadable. Restore it from backup before syncing.")
            })?;
        if state.version != VERSION {
            return Err(error("Update Switchboard to read this sync record."));
        }
        Ok(state)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write(path, &serde_json::to_vec(self)?)
    }
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Counts {
    pub exported: usize,
    pub imported: usize,
    pub conflicts: usize,
    pub pending: usize,
}

pub trait Adapter {
    fn capture(&mut self) -> Result<Vec<NativeSession>>;
    fn restore(&mut self, session: &NativeSession, target_id: &str) -> Result<()>;
    /// Check again immediately before an import, in case an app opened during
    /// an iCloud scan. A busy provider must never be modified.
    fn ready(&self) -> Result<bool>;
}

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fingerprint(session: &NativeSession) -> Result<String> {
    Ok(session_measure(session)?.0)
}

fn session_measure(session: &NativeSession) -> Result<(String, u64)> {
    struct HashWriter {
        hash: Sha256,
        bytes: u64,
    }
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.hash.update(bytes);
            self.bytes = self.bytes.saturating_add(bytes.len() as u64);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter {
        hash: Sha256::new(),
        bytes: 0,
    };
    serde_json::to_writer(&mut writer, session)?;
    Ok((
        writer
            .hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        writer.bytes,
    ))
}

/// Adapters call this before retaining each captured session, so the source
/// collection has the same explicit bound as the engine's deduplicated map.
pub(super) fn check_capture_budget(total: &mut u64, session: &NativeSession) -> Result<()> {
    *total = total.saturating_add(session_measure(session)?.1);
    if *total > MAX_CAPTURE_BYTES {
        return Err(error(
            "The combined native history exceeds the 512 MiB capture limit. No history was truncated.",
        ));
    }
    Ok(())
}

fn indexed(package: &Package, path: PathBuf) -> Result<Revision> {
    Ok(Revision {
        chat_id: package.chat_id.clone(),
        parent: package.parent.clone(),
        path,
        session_fingerprint: fingerprint(&package.session)?,
    })
}

fn package_path(root: &Path, provider: Provider, chat_id: &str, revision: &str) -> Result<PathBuf> {
    safe_child(
        root,
        &format!(
            "{}/{}/{revision}.json",
            provider.key(),
            digest(chat_id.as_bytes())
        ),
    )
}

fn load_package(revision: &str, header: &Revision, provider: Provider) -> Result<Package> {
    let bytes = read_regular(&header.path, MAX_PACKAGE_BYTES)?;
    if digest(&bytes) != revision {
        return Err(error(
            "An iCloud chat package changed or is still downloading. Retry when its download finishes.",
        ));
    }
    let package: Package = serde_json::from_slice(&bytes)
        .map_err(|_| error("An iCloud chat package has an unsupported format."))?;
    if package.version != VERSION
        || package.provider != provider
        || package.chat_id != header.chat_id
        || package.parent != header.parent
        || fingerprint(&package.session)? != header.session_fingerprint
    {
        return Err(error(
            "An iCloud chat package failed validation before restoration.",
        ));
    }
    Ok(package)
}

fn captures(adapter: &mut impl Adapter) -> Result<BTreeMap<String, NativeSession>> {
    let mut result = BTreeMap::new();
    let mut total = 0u64;
    for session in adapter.capture()? {
        if session.id.is_empty() || session.id.len() > 512 {
            return Err(error("A native chat has an unsupported identity."));
        }
        check_capture_budget(&mut total, &session)?;
        match result.entry(session.id.clone()) {
            std::collections::btree_map::Entry::Occupied(previous) => {
                if previous.get() != &session {
                    return Err(error(
                        "Local copies of a chat differ between accounts. Both were kept; resolve that conflict before syncing.",
                    ));
                }
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(session);
            }
        }
    }
    Ok(result)
}

/// One immutable object per revision. iCloud never has to resolve concurrent
/// writes to a shared catalog, database, lock, or mutable manifest.
fn publish(root: &Path, package: &Package) -> Result<String> {
    let bytes = serde_json::to_vec(package)?;
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(error(
            "A chat exceeds the 512 MiB sync package limit; it was kept locally.",
        ));
    }
    let revision = digest(&bytes);
    let folder = safe_child(
        root,
        &format!(
            "{}/{}",
            package.provider.key(),
            digest(package.chat_id.as_bytes())
        ),
    )?;
    private_dir(&folder)?;
    let path = safe_child(&folder, &format!("{revision}.json"))?;
    if path.try_exists()? {
        if read_regular(&path, MAX_PACKAGE_BYTES)? != bytes {
            return Err(error("An iCloud sync package failed its integrity check."));
        }
    } else {
        let mut staged = tempfile::NamedTempFile::new_in(&folder)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        match staged.persist_noclobber(&path) {
            Ok(_) => (),
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if read_regular(&path, MAX_PACKAGE_BYTES)? != bytes {
                    return Err(error("An iCloud sync package failed its integrity check."));
                }
            }
            Err(_) => return Err(error("Could not publish a chat to the sync folder.")),
        }
    }
    sync_directory(&folder)?;
    Ok(revision)
}

fn hash_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn packages(root: &Path, provider: Provider) -> Result<(BTreeMap<String, Revision>, usize)> {
    let folder = safe_child(root, provider.key())?;
    if !folder.try_exists()? {
        return Ok((BTreeMap::new(), 0));
    }
    let mut result = BTreeMap::new();
    let mut pending = 0;
    for group in std::fs::read_dir(&folder)? {
        let group = group?;
        let name = group.file_name().to_string_lossy().into_owned();
        if !hash_name(&name) {
            continue;
        }
        if !group.file_type()?.is_dir() {
            return Err(error("The sync folder contains an unsafe entry."));
        }
        for entry in std::fs::read_dir(group.path())? {
            let entry = entry?;
            let file = entry.file_name().to_string_lossy().into_owned();
            // Ignore iCloud placeholders and temporary uploads. They are not
            // complete committed objects and will be picked up on a later run.
            let Some(revision) = file.strip_suffix(".json").filter(|s| hash_name(s)) else {
                if file.ends_with(".icloud") {
                    pending += 1;
                }
                continue;
            };
            if !entry.file_type()?.is_file() {
                return Err(error("The sync folder contains an unsafe entry."));
            }
            let bytes = read_regular(&entry.path(), MAX_PACKAGE_BYTES)?;
            if digest(&bytes) != revision {
                pending += 1;
                continue;
            }
            let package: Package = serde_json::from_slice(&bytes)
                .map_err(|_| error("An iCloud chat package has an unsupported format."))?;
            if package.version != VERSION {
                return Err(error(
                    "Update Switchboard on both Macs to read this sync format.",
                ));
            }
            if package.provider != provider
                || digest(package.chat_id.as_bytes()) != name
                || package.parent.as_ref().is_some_and(|p| !hash_name(p))
                || package.chat_id.is_empty()
                || package.chat_id.len() > 512
            {
                return Err(error("An iCloud chat package failed validation."));
            }
            result.insert(revision.to_string(), indexed(&package, entry.path())?);
            if result.len() > MAX_PACKAGES {
                return Err(error(
                    "The sync archive has too many revisions to process safely.",
                ));
            }
        }
    }
    Ok((result, pending))
}

fn ancestor(older: &str, newer: &str, objects: &BTreeMap<String, Revision>) -> bool {
    let mut cursor = Some(newer);
    let mut visited = BTreeSet::new();
    while let Some(revision) = cursor {
        if revision == older {
            return true;
        }
        if !visited.insert(revision) {
            return false;
        }
        cursor = objects.get(revision).and_then(|p| p.parent.as_deref());
    }
    false
}

fn complete_chain(revision: &str, objects: &BTreeMap<String, Revision>) -> bool {
    let Some(origin) = objects.get(revision) else {
        return false;
    };
    let mut next = Some(revision);
    let mut seen = BTreeSet::new();
    while let Some(id) = next {
        let Some(package) = objects.get(id) else {
            return false;
        };
        if !seen.insert(id) || package.chat_id != origin.chat_id {
            return false;
        }
        next = package.parent.as_deref();
    }
    true
}

fn fork_id(source: &str) -> String {
    let prefix = if source.starts_with("local_") {
        "local_"
    } else {
        ""
    };
    format!("{prefix}{}", uuid::Uuid::new_v4())
}

/// Export changes, then advance unchanged replicas or preserve concurrent
/// branches as independent native chats. No last-writer-wins or timestamp
/// comparison is involved. Each write is followed by a durable local receipt.
pub fn sync(
    root: &Path,
    state_path: &Path,
    state: &mut State,
    provider: Provider,
    adapter: &mut impl Adapter,
) -> Result<Counts> {
    let mut counts = Counts::default();
    if !adapter.ready()? {
        counts.pending = 1;
        return Ok(counts);
    }
    let mut local = captures(adapter)?;
    let (mut objects, incomplete) = packages(root, provider)?;
    counts.pending += incomplete;

    // A interrupted import is retried at its reserved ID only if that native
    // chat is still unchanged. Otherwise preserve it and recover into a fork.
    if let Some(pending) = state.pending.get(&provider).cloned() {
        let Some(header) = objects.get(&pending.revision) else {
            counts.pending += 1;
            return Ok(counts);
        };
        let package = load_package(&pending.revision, header, provider)?;
        let current = local.get(&pending.local_id).map(fingerprint).transpose()?;
        let mut expected = package.session.clone();
        expected.id = pending.local_id.clone();
        if local.get(&pending.local_id) == Some(&expected) {
            record_import(
                state_path,
                state,
                provider,
                &pending.revision,
                &package,
                &pending.local_id,
                &expected,
            )?;
        } else {
            let target = if current == pending.before {
                pending.local_id.clone()
            } else {
                counts.conflicts += 1;
                fork_id(&pending.local_id)
            };
            import(
                root,
                state_path,
                state,
                provider,
                &pending.revision,
                &package,
                &target,
                adapter,
                &mut local,
            )?;
        }
        counts.imported += 1;
    }

    for (id, session) in &local {
        let baseline = fingerprint(session)?;
        let position = state
            .replicas
            .iter()
            .position(|r| r.provider == provider && r.local_id == *id);
        if position.is_some_and(|i| state.replicas[i].baseline == baseline) {
            continue;
        }
        let previous = position.map(|i| state.replicas[i].clone());
        let chat_id = previous
            .as_ref()
            .map_or_else(|| id.clone(), |r| r.chat_id.clone());
        // A manually migrated identical native session can join an existing
        // replica without creating a new divergent root revision.
        let matching = if previous.is_none() {
            objects
                .iter()
                .find(|(_, p)| p.chat_id == chat_id && p.session_fingerprint == baseline)
                .map(|(revision, _)| revision.clone())
        } else {
            None
        };
        let revision = if let Some(revision) = matching {
            revision
        } else {
            let package = Package {
                version: VERSION,
                provider,
                chat_id: chat_id.clone(),
                parent: previous.as_ref().map(|r| r.revision.clone()),
                device_id: state.device_id.clone(),
                session: session.clone(),
            };
            let revision = publish(root, &package)?;
            objects.insert(
                revision.clone(),
                indexed(&package, package_path(root, provider, &chat_id, &revision)?)?,
            );
            counts.exported += 1;
            revision
        };
        let replica = Replica {
            provider,
            chat_id,
            local_id: id.clone(),
            revision,
            baseline,
        };
        if let Some(index) = position {
            state.replicas[index] = replica;
        } else {
            state.replicas.push(replica);
        }
        state.save(state_path)?;
    }

    let parents: BTreeSet<_> = objects.values().filter_map(|p| p.parent.as_ref()).collect();
    let heads: Vec<_> = objects
        .iter()
        .filter(|(id, _)| !parents.contains(id))
        .collect();
    for (revision, header) in heads {
        if !complete_chain(revision, &objects) {
            counts.pending += 1;
            continue;
        }
        let replicas: Vec<_> = state
            .replicas
            .iter()
            .filter(|r| r.provider == provider && r.chat_id == header.chat_id)
            .cloned()
            .collect();
        if replicas
            .iter()
            .any(|r| ancestor(revision, &r.revision, &objects))
        {
            continue;
        }
        let fast_forward = replicas.iter().find(|r| {
            local.get(&r.local_id).is_some_and(|session| {
                fingerprint(session).is_ok_and(|current| current == r.baseline)
            }) && ancestor(&r.revision, revision, &objects)
        });
        let target = if let Some(replica) = fast_forward {
            replica.local_id.clone()
        } else if replicas.is_empty() && !local.contains_key(&header.chat_id) {
            header.chat_id.clone()
        } else {
            counts.conflicts += 1;
            fork_id(&header.chat_id)
        };
        if !adapter.ready()? {
            counts.pending += 1;
            break;
        }
        let package = load_package(revision, header, provider)?;
        import(
            root, state_path, state, provider, revision, &package, &target, adapter, &mut local,
        )?;
        counts.imported += 1;
    }
    Ok(counts)
}

#[allow(clippy::too_many_arguments)]
fn import(
    _root: &Path,
    state_path: &Path,
    state: &mut State,
    provider: Provider,
    revision: &str,
    package: &Package,
    target: &str,
    adapter: &mut impl Adapter,
    local: &mut BTreeMap<String, NativeSession>,
) -> Result<()> {
    if !adapter.ready()? {
        return Err(error(
            "Sync is waiting for the app and its CLI sessions to close.",
        ));
    }
    // A process can open, edit, and close again while iCloud is being scanned.
    // Being closed now does not prove the initially captured target is current.
    // Recheck before the native write so these unexported edits are preserved.
    let latest = captures(adapter)?;
    if latest.get(target) != local.get(target) {
        return Err(error(
            "Local history changed during sync. Its edits were kept; retry after closing the app.",
        ));
    }
    drop(latest);
    if !adapter.ready()? {
        return Err(error(
            "Sync is waiting for the app and its CLI sessions to close.",
        ));
    }
    state.pending.insert(
        provider,
        PendingImport {
            provider,
            chat_id: package.chat_id.clone(),
            local_id: target.to_string(),
            revision: revision.to_string(),
            before: local.get(target).map(fingerprint).transpose()?,
        },
    );
    state.save(state_path)?;
    adapter.restore(&package.session, target)?;
    if !adapter.ready()? {
        return Err(error(
            "The app reopened during restoration. Its history was kept; close it to finish recovery.",
        ));
    }
    let refreshed = captures(adapter)?;
    if !adapter.ready()? {
        return Err(error(
            "The app reopened during restoration. Its history was kept; close it to finish recovery.",
        ));
    }
    let restored = refreshed.get(target).ok_or_else(|| {
        error("The restored chat could not be read back. Its sync package was retained.")
    })?;
    record_import(
        state_path, state, provider, revision, package, target, restored,
    )?;
    *local = refreshed;
    Ok(())
}

fn record_import(
    state_path: &Path,
    state: &mut State,
    provider: Provider,
    revision: &str,
    package: &Package,
    target: &str,
    restored: &NativeSession,
) -> Result<()> {
    let replica = Replica {
        provider,
        chat_id: package.chat_id.clone(),
        local_id: target.to_string(),
        revision: revision.to_string(),
        baseline: fingerprint(restored)?,
    };
    state
        .replicas
        .retain(|r| !(r.provider == provider && r.local_id == target));
    state.replicas.push(replica);
    state.pending.remove(&provider);
    state.save(state_path)?;
    Ok(())
}

pub fn private_dir(path: &Path) -> Result<()> {
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(error("A sync path must not be a symbolic link."));
    }
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.try_exists()? {
        missing.push(cursor.to_path_buf());
        cursor = cursor
            .parent()
            .ok_or_else(|| error("Invalid sync directory."))?;
    }
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    // A flushed journal is useful only if its newly created directory entries
    // also survive a crash. Persist each new directory and its parent.
    for directory in missing {
        sync_directory(&directory)?;
        if let Some(parent) = directory.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn safe_child(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.is_empty()
        || Path::new(relative)
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(error("A sync package contains an unsafe path."));
    }
    let mut current = root.to_path_buf();
    if current
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(error("A sync path must not be a symbolic link."));
    }
    for part in Path::new(relative).components() {
        current.push(part);
        if current
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(error("A sync path must not be a symbolic link."));
        }
    }
    Ok(current)
}

pub fn read_regular(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let metadata = path.symlink_metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(error(
            "A sync file is unsafe or exceeds the supported size.",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(error("A sync file is not a regular file."));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(error("A sync file exceeds the supported size."));
    }
    Ok(bytes)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| error("Invalid sync file path."))?;
    private_dir(parent)?;
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(error("A sync file must not be a symbolic link."));
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|_| error("Could not save the local sync record."))?;
    sync_directory(parent)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn package(provider: Provider, text: &str) -> Package {
        Package {
            version: VERSION,
            provider,
            chat_id: "synthetic-chat".into(),
            parent: None,
            device_id: "synthetic-mac".into(),
            session: NativeSession {
                id: "synthetic-chat".into(),
                payload: json!({"text":text}),
            },
        }
    }

    #[test]
    fn immutable_publication_is_idempotent_and_separate_per_provider() {
        let dir = tempfile::tempdir().unwrap();
        let first = package(Provider::Codex, "synthetic secret-free text");
        let revision = publish(dir.path(), &first).unwrap();
        assert_eq!(revision, publish(dir.path(), &first).unwrap());
        publish(dir.path(), &package(Provider::Claude, "different text")).unwrap();
        assert_eq!(packages(dir.path(), Provider::Codex).unwrap().0.len(), 1);
        assert_eq!(packages(dir.path(), Provider::Claude).unwrap().0.len(), 1);
    }

    #[test]
    fn truncated_cloud_objects_and_placeholders_are_never_imported() {
        let dir = tempfile::tempdir().unwrap();
        let revision = publish(dir.path(), &package(Provider::Codex, "complete")).unwrap();
        let group = dir.path().join("codex").join(digest(b"synthetic-chat"));
        std::fs::write(group.join(format!("{revision}.json")), b"incomplete").unwrap();
        std::fs::write(group.join(".incoming.json.icloud"), b"").unwrap();
        let (objects, pending) = packages(dir.path(), Provider::Codex).unwrap();
        assert!(objects.is_empty());
        assert_eq!(pending, 2);
    }

    #[test]
    fn revision_index_drops_payload_and_revalidates_before_lazy_import() {
        let dir = tempfile::tempdir().unwrap();
        let original = package(Provider::Codex, &"synthetic transcript ".repeat(10_000));
        let revision = publish(dir.path(), &original).unwrap();
        let (objects, pending) = packages(dir.path(), Provider::Codex).unwrap();
        assert_eq!(pending, 0);
        let header = objects.get(&revision).unwrap();
        assert!(format!("{header:?}").len() < 1024);
        assert_eq!(
            load_package(&revision, header, Provider::Codex)
                .unwrap()
                .session,
            original.session
        );
        assert!(load_package(&revision, header, Provider::Claude).is_err());
        std::fs::write(&header.path, b"download changed after index scan").unwrap();
        assert!(load_package(&revision, header, Provider::Codex).is_err());
    }

    #[test]
    fn streaming_fingerprint_and_capture_budget_do_not_allocate_full_copies() {
        let session = package(Provider::Codex, "bounded native fixture").session;
        let bytes = serde_json::to_vec(&session).unwrap();
        assert_eq!(
            session_measure(&session).unwrap(),
            (digest(&bytes), bytes.len() as u64)
        );
        let mut budget = MAX_CAPTURE_BYTES - bytes.len() as u64;
        check_capture_budget(&mut budget, &session).unwrap();
        assert!(check_capture_budget(&mut budget, &session).is_err());
    }

    #[test]
    fn incomplete_or_cross_chat_ancestry_is_not_a_fast_forward() {
        let mut child = package(Provider::Codex, "child");
        child.parent = Some("missing".into());
        let mut objects =
            BTreeMap::from([("child".into(), indexed(&child, PathBuf::new()).unwrap())]);
        assert!(!complete_chain("child", &objects));
        let mut wrong = package(Provider::Codex, "unrelated");
        wrong.chat_id = "another-chat".into();
        objects.insert("missing".into(), indexed(&wrong, PathBuf::new()).unwrap());
        assert!(!complete_chain("child", &objects));
    }

    #[test]
    fn unsafe_relative_paths_and_symlinks_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        for path in ["../outside", "/absolute", "child/../../escape", ""] {
            assert!(safe_child(dir.path(), path).is_err());
        }
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), dir.path().join("codex")).unwrap();
            assert!(publish(dir.path(), &package(Provider::Codex, "text")).is_err());
            std::fs::write(outside.path().join("secret"), b"never read").unwrap();
            std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link"))
                .unwrap();
            assert!(read_regular(&dir.path().join("link"), 100).is_err());
        }
    }

    #[test]
    fn corrupt_local_receipts_are_not_treated_as_a_first_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, b"broken").unwrap();
        assert!(State::load(&path).is_err());
    }
}
