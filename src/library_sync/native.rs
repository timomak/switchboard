//! Native, per-item CAS installation. Roots and process readiness are injected;
//! account discovery and provider operation leases remain with the caller.
use super::{
    mcp,
    model::{
        Adapter, ApplyResult, Content, InstallStatus, InventoryCandidate, Kind, LibraryItem, Target,
    },
    skills::{self, error},
    storage::content_digest,
};
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const MAX_CONFIG: usize = 16 * 1024 * 1024;
const MAX_INVENTORY: usize = 256 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct NativeRoots {
    pub user_home: PathBuf,
    pub codex_homes: Vec<PathBuf>,
    pub codex_skill_roots: Vec<PathBuf>,
    pub claude_homes: Vec<PathBuf>,
    pub claude_config_files: Vec<PathBuf>,
}
#[derive(Clone, Debug)]
struct Destination {
    target: Target,
    kind: Kind,
    path: PathBuf,
}
pub struct NativeAdapter<'a> {
    roots: NativeRoots,
    local: PathBuf,
    bindings: BTreeMap<String, String>,
    ready: &'a dyn Fn(Target) -> Result<bool>,
    destinations: BTreeMap<String, Destination>,
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn opaque(parts: &[&str]) -> String {
    hash(parts.join("\0").as_bytes())
}
/// Local binding identity follows the item across renames, and stays separate
/// for every native account/configuration destination and slot.
pub(super) fn binding_key(item_id: &str, locator: &str, slot: &str) -> String {
    format!("item:{}:{slot}", opaque(&[item_id, locator]))
}
fn outcome(status: InstallStatus, detail: &str, applied: bool) -> ApplyResult {
    ApplyResult {
        status,
        detail: detail.into(),
        applied,
    }
}
fn missing(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err(error("could not inspect a native destination")),
    }
}

pub(crate) fn write_private(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    skills::no_symlink(path)?;
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(if executable { 0o700 } else { 0o600 })
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let mut file = options
        .open(path)
        .map_err(|_| error("could not create a private staged file"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| error("could not durably stage a native file"))
}
fn sync_dir(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| error("could not flush a native folder"))
}
pub(crate) fn sync_tree(path: &Path) -> Result<()> {
    skills::no_symlink(path)?;
    for entry in fs::read_dir(path).map_err(|_| error("could not flush staged folders"))? {
        let entry = entry.map_err(|_| error("could not flush staged folders"))?;
        let meta = fs::symlink_metadata(entry.path())
            .map_err(|_| error("could not inspect staged content"))?;
        if meta.is_dir() {
            sync_tree(&entry.path())?;
        } else if !meta.is_file() {
            return Err(error("unsafe staged content"));
        }
    }
    sync_dir(path)
}
fn private_dir(path: &Path) -> Result<()> {
    skills::no_symlink(path)?;
    fs::create_dir_all(path).map_err(|_| error("could not create a private sync folder"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| error("could not protect a private sync folder"))?;
    }
    Ok(())
}
fn sync_lineage(path: &Path) -> Result<()> {
    let mut current = Some(path);
    while let Some(p) = current {
        sync_dir(p)?;
        current = p.parent();
    }
    Ok(())
}
fn optional_file(path: &Path) -> Result<Option<Vec<u8>>> {
    if missing(path)? {
        Ok(None)
    } else {
        skills::read(path, MAX_CONFIG).map(Some)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    version: u32,
    entry_hash: String,
    content: Option<Content>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingStamp {
    locator: String,
    name: String,
    stamp: Stamp,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    target: PathBuf,
    kind: Kind,
    before: Option<String>,
    after: Option<String>,
    stamp: Option<PendingStamp>,
}
fn raw_hash(path: &Path, kind: Kind) -> Result<Option<String>> {
    if missing(path)? {
        return Ok(None);
    }
    skills::no_symlink(path)?;
    match kind {
        Kind::Skill => Ok(Some(content_digest(&skills::capture(path)?)?)),
        Kind::Mcp => Ok(Some(hash(&skills::read(path, MAX_CONFIG)?))),
    }
}
fn remove_path(path: &Path) -> Result<()> {
    skills::no_symlink(path)?;
    if missing(path)? {
        return Ok(());
    }
    let meta = fs::symlink_metadata(path).map_err(|_| error("could not inspect staged cleanup"))?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .map_err(|_| error("could not clean a completed native operation"))
}
fn rename_new(source: &Path, target: &Path) -> Result<()> {
    skills::no_symlink(source)?;
    skills::no_symlink(target)?;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            source,
            rustix::fs::CWD,
            target,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| {
            error("native content appeared during installation; both copies were retained")
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(error(
            "atomic native installation is not supported on this platform",
        ))
    }
}

impl<'a> NativeAdapter<'a> {
    pub fn new(
        mut roots: NativeRoots,
        local: PathBuf,
        bindings: BTreeMap<String, String>,
        ready: &'a dyn Fn(Target) -> Result<bool>,
    ) -> Self {
        fn unique(paths: &mut Vec<PathBuf>) {
            let mut seen = BTreeSet::new();
            paths.retain(|p| seen.insert(fs::canonicalize(p).unwrap_or_else(|_| p.clone())));
        }
        unique(&mut roots.codex_homes);
        unique(&mut roots.codex_skill_roots);
        unique(&mut roots.claude_homes);
        unique(&mut roots.claude_config_files);
        Self {
            roots,
            local,
            bindings,
            ready,
            destinations: BTreeMap::new(),
        }
    }
    fn register(&mut self, target: Target, kind: Kind, path: PathBuf) -> String {
        let id = opaque(&[
            target.key(),
            match kind {
                Kind::Skill => "skill",
                Kind::Mcp => "mcp",
            },
            path.to_string_lossy().as_ref(),
        ]);
        self.destinations
            .insert(id.clone(), Destination { target, kind, path });
        id
    }
    fn skill_roots(&self, target: Target) -> Vec<PathBuf> {
        match target {
            Target::Codex => {
                let mut roots = self.roots.codex_skill_roots.clone();
                roots.extend(self.roots.codex_homes.iter().map(|p| p.join("skills")));
                let mut seen = BTreeSet::new();
                roots.retain(|p| seen.insert(fs::canonicalize(p).unwrap_or_else(|_| p.clone())));
                roots
            }
            Target::ClaudeCode => self
                .roots
                .claude_homes
                .iter()
                .map(|p| p.join("skills"))
                .collect(),
            Target::Cowork => Vec::new(),
        }
    }
    fn configs(&self, target: Target) -> Vec<PathBuf> {
        if target == Target::ClaudeCode && !self.roots.claude_config_files.is_empty() {
            return self.roots.claude_config_files.clone();
        }
        match target {
            Target::Codex => self
                .roots
                .codex_homes
                .iter()
                .map(|p| p.join("config.toml"))
                .collect(),
            Target::ClaudeCode => self
                .roots
                .claude_homes
                .iter()
                .map(|p| {
                    if p == &self.roots.user_home.join(".claude") {
                        self.roots.user_home.join(".claude.json")
                    } else {
                        p.join(".claude.json")
                    }
                })
                .collect(),
            Target::Cowork => Vec::new(),
        }
    }
    fn resolve(
        &mut self,
        target: Target,
        item: &LibraryItem,
        locator: &str,
    ) -> Result<Destination> {
        if !skills::safe_name(&item.name) {
            return Err(error("an item name cannot be installed safely"));
        }
        if !self.destinations.contains_key(locator) {
            let _ = self.destinations(target, item)?;
        }
        let dest = self
            .destinations
            .get(locator)
            .filter(|d| d.target == target && d.kind == item.content.kind())
            .ok_or_else(|| error("the selected native destination is no longer configured"))?
            .clone();
        skills::no_symlink(&dest.path)?;
        Ok(dest)
    }
    fn stamp_path(&self, locator: &str, name: &str) -> PathBuf {
        self.local
            .join("ownership")
            .join(format!("{}.json", opaque(&[locator, name])))
    }
    fn read_stamp(&self, locator: &str, name: &str) -> Result<Option<Stamp>> {
        let path = self.stamp_path(locator, name);
        let Some(bytes) = optional_file(&path)? else {
            return Ok(None);
        };
        let stamp: Stamp =
            serde_json::from_slice(&bytes).map_err(|_| error("invalid local ownership receipt"))?;
        if stamp.version != 1 {
            return Err(error("unsupported local ownership receipt"));
        }
        Ok(Some(stamp))
    }
    fn pending_stamp(locator: &str, item: &LibraryItem, entry_hash: &str) -> PendingStamp {
        PendingStamp {
            locator: locator.into(),
            name: item.name.clone(),
            stamp: Stamp {
                version: 1,
                entry_hash: entry_hash.into(),
                content: if item.content.kind() == Kind::Mcp {
                    Some(item.content.clone())
                } else {
                    None
                },
            },
        }
    }
    fn record_stamp(&self, stamp: &PendingStamp) -> Result<()> {
        if !skills::safe_name(&stamp.name)
            || stamp.locator.len() != 64
            || !stamp.locator.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(error("invalid native ownership identity"));
        }
        let path = self.stamp_path(&stamp.locator, &stamp.name);
        private_dir(path.parent().unwrap())?;
        let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        write_private(&temp, &serde_json::to_vec(&stamp.stamp)?, false)?;
        skills::no_symlink(&path)?;
        fs::rename(&temp, &path).map_err(|_| error("could not retain native ownership"))?;
        sync_lineage(path.parent().unwrap())
    }
    fn stamp(&self, locator: &str, item: &LibraryItem, entry_hash: &str) -> Result<()> {
        if self.preserve_local_stamp(locator, item, entry_hash)? {
            return Ok(());
        }
        self.record_stamp(&Self::pending_stamp(locator, item, entry_hash))
    }
    fn preserve_local_stamp(
        &self,
        locator: &str,
        item: &LibraryItem,
        entry_hash: &str,
    ) -> Result<bool> {
        Ok(self.read_stamp(locator, &item.name)?.is_some_and(|old| {
            old.content.as_ref() == Some(&item.content) && old.entry_hash != entry_hash
        }))
    }
    fn journal_path(&self, dest: &Destination) -> PathBuf {
        self.local
            .join("journals")
            .join(opaque(&[dest.path.to_string_lossy().as_ref()]))
    }
    fn recover(&self, dest: &Destination) -> Result<()> {
        let dir = self.journal_path(dest);
        if missing(&dir)? {
            return Ok(());
        }
        skills::no_symlink(&dir)?;
        if missing(&dir.join("journal.json"))? {
            if missing(&dir.join("old"))? {
                remove_path(&dir)?;
                sync_dir(dir.parent().unwrap())?;
                return Ok(());
            }
            return Err(error(
                "an incomplete recovery journal needs review; originals were retained",
            ));
        }
        let bytes = skills::read(&dir.join("journal.json"), 1024 * 1024)?;
        let journal: Journal =
            serde_json::from_slice(&bytes).map_err(|_| error("invalid local recovery journal"))?;
        if journal.version != 1 || journal.target != dest.path || journal.kind != dest.kind {
            return Err(error("invalid local recovery destination"));
        }
        let current = raw_hash(&dest.path, dest.kind)?;
        let backup = raw_hash(&dir.join("old"), dest.kind)?;
        if backup.is_some() && backup != journal.before {
            return Err(error("a recovery backup changed; all copies were retained"));
        }
        if current == journal.after {
            if let Some(stamp) = &journal.stamp {
                self.record_stamp(stamp)?;
            }
            remove_path(&dir)?;
            sync_dir(dir.parent().unwrap())?;
            return Ok(());
        }
        // With no original backup there is nothing to roll back. Keep the
        // current native item untouched; the engine will inspect any local edit.
        if backup.is_none() {
            remove_path(&dir)?;
            sync_dir(dir.parent().unwrap())?;
            return Ok(());
        }
        if current.is_none() && backup == journal.before && backup.is_some() {
            if !(self.ready)(dest.target)? {
                return Err(error("waiting for the app to close before native recovery"));
            }
            rename_new(&dir.join("old"), &dest.path)?;
            sync_dir(dest.path.parent().unwrap())?;
            remove_path(&dir)?;
            sync_dir(dir.parent().unwrap())?;
            return Ok(());
        }
        Err(error(
            "native content changed after an interruption; current content and recovery copies were retained",
        ))
    }
    fn replace(
        &self,
        dest: &Destination,
        before: Option<String>,
        files: Option<&[super::model::SkillFile]>,
        bytes: Option<&[u8]>,
        stamp: Option<PendingStamp>,
    ) -> Result<bool> {
        self.recover(dest)?;
        let dir = self.journal_path(dest);
        private_dir(dir.parent().unwrap())?;
        private_dir(&dir)?;
        let prepared = (|| -> Result<()> {
            if let Some(files) = files {
                skills::stage(&dir.join("new"), files)?;
            } else if let Some(bytes) = bytes {
                write_private(&dir.join("new"), bytes, false)?;
            }
            let after = raw_hash(&dir.join("new"), dest.kind)?;
            write_private(
                &dir.join("journal.json"),
                &serde_json::to_vec(&Journal {
                    version: 1,
                    target: dest.path.clone(),
                    kind: dest.kind,
                    before: before.clone(),
                    after,
                    stamp: stamp.clone(),
                })?,
                false,
            )?;
            sync_tree(&dir)?;
            sync_lineage(dir.parent().unwrap())?;
            Ok(())
        })();
        if let Err(err) = prepared {
            let _ = remove_path(&dir);
            return Err(err);
        }
        if !(self.ready)(dest.target)? {
            self.recover(dest)?;
            return Ok(false);
        }
        if raw_hash(&dest.path, dest.kind)? != before {
            return Err(error(
                "native content changed during staging; no local changes were overwritten",
            ));
        }
        skills::no_symlink(&dest.path)?;
        fs::create_dir_all(dest.path.parent().unwrap())
            .map_err(|_| error("could not create a native destination"))?;
        sync_lineage(dest.path.parent().unwrap())?;
        if !(self.ready)(dest.target)? {
            self.recover(dest)?;
            return Ok(false);
        }
        if before.is_some() {
            rename_new(&dest.path, &dir.join("old"))?;
            sync_dir(dest.path.parent().unwrap())?;
            sync_dir(&dir)?;
            if raw_hash(&dir.join("old"), dest.kind)? != before {
                if missing(&dest.path)? {
                    rename_new(&dir.join("old"), &dest.path)?;
                    sync_dir(dest.path.parent().unwrap())?;
                }
                return Err(error(
                    "native content changed immediately before replacement; changes were preserved",
                ));
            }
        }
        if !missing(&dir.join("new"))? {
            rename_new(&dir.join("new"), &dest.path)?;
        }
        sync_dir(dest.path.parent().unwrap())?;
        sync_dir(&dir)?;
        write_private(&dir.join("committed"), b"1", false)?;
        sync_dir(&dir)?;
        if let Some(stamp) = &stamp {
            self.record_stamp(stamp)?;
        }
        remove_path(&dir)?;
        sync_dir(dir.parent().unwrap())?;
        Ok(true)
    }
    pub fn inventory(&mut self) -> Result<Vec<InventoryCandidate>> {
        let mut result = Vec::new();
        let mut bytes = 0usize;
        for target in [Target::Codex, Target::ClaudeCode] {
            for root in self.skill_roots(target) {
                if missing(&root)? {
                    continue;
                }
                let locator = self.register(target, Kind::Skill, root.clone());
                if skills::no_symlink(&root).is_err() {
                    result.push(candidate(
                        &locator,
                        "Linked skill location",
                        target,
                        Kind::Skill,
                        "unsupported",
                        None,
                        vec![],
                        "Linked roots are preserved and are not imported.",
                    ));
                    continue;
                }
                let mut entries = fs::read_dir(&root)
                    .map_err(|_| error("could not inventory a skill root"))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|_| error("could not inventory skills"))?;
                entries.sort_by_key(|e| e.file_name());
                for entry in entries {
                    if result.len() >= 5000 {
                        return Err(error("the inventory exceeds the supported item count"));
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    if target == Target::ClaudeCode && name == "synced" {
                        let managed = self.register(target, Kind::Skill, entry.path());
                        result.push(candidate(&managed,"Claude account skills",target,Kind::Skill,"account_managed",None,vec![],"Claude downloads these skills from your account. Manage them in Claude Customize; the cache is excluded."));
                        continue;
                    }
                    let kind = entry
                        .file_type()
                        .map_err(|_| error("could not inspect a skill"))?;
                    if !kind.is_dir() && !kind.is_symlink() {
                        continue;
                    }
                    let path = entry.path();
                    let item_locator = self.register(target, Kind::Skill, path.clone());
                    if !skills::safe_name(&name) {
                        result.push(candidate(&item_locator,"Unsupported skill name",target,Kind::Skill,"unsupported",None,vec![],"Use a short skill folder name with letters, numbers, hyphens or underscores."));
                        continue;
                    }
                    if path.join(".claude-plugin").exists() || path.join(".codex-plugin").exists() {
                        result.push(candidate(&item_locator,&name,target,Kind::Skill,"plugin_managed",None,vec![],"This folder is a plugin; install or update it through its original plugin manager."));
                        continue;
                    }
                    match skills::capture(&path) {Ok(content)=>{let requirements=skills::requirements(&content,target)?;let size=serde_json::to_vec(&content)?.len();bytes=bytes.saturating_add(size);if bytes>MAX_INVENTORY{return Err(error("the skill inventory exceeds 256 MiB; reduce the selected roots"));}result.push(candidate(&item_locator,&name,target,Kind::Skill,"custom",Some(content),requirements,"Complete personal skill folder; supporting files are included."));},Err(_)=>result.push(candidate(&item_locator,&name,target,Kind::Skill,"unsupported",None,vec![],"This skill contains unsupported paths, files, missing SKILL.md, or exceeds the size limit."))}
                }
            }
            for path in self.configs(target) {
                if missing(&path)? {
                    continue;
                }
                let locator = self.register(target, Kind::Mcp, path.clone());
                match native_entries(&path,target) {Ok(entries)=>for(name,value)in entries{let id=opaque(&[&locator,&name]);if !skills::safe_name(&name){result.push(candidate(&id,"Unsupported MCP name",target,Kind::Mcp,"unsupported",None,vec![],"Use a server name containing letters, numbers, hyphens or underscores."));continue;}
                match mcp::normalize(&value,target){Ok(definition)=>{let requirements=mcp::requirements(&definition)?;let mut item=candidate(&locator,&name,target,Kind::Mcp,"custom",Some(Content::Mcp{definition}),requirements,"Personal MCP setup; credential values stay on this Mac.");item.id=id;result.push(item);},Err(_)=>result.push(candidate(&locator,&name,target,Kind::Mcp,"unsupported",None,vec![],"This MCP setup uses an unsupported transport or client-specific options; it remains under its current configuration."))}
            },Err(_)=>result.push(candidate(&locator,"MCP configuration",target,Kind::Mcp,"unsupported",None,vec![],"The native MCP configuration could not be safely parsed; its contents were not exported."))}
            }
        }
        result.sort_by(|a, b| (&a.source, &a.name, &a.id).cmp(&(&b.source, &b.name, &b.id)));
        Ok(result)
    }
    pub fn binding_destinations(
        &mut self,
        item: &LibraryItem,
        target: Target,
    ) -> Result<Vec<(String, Vec<String>)>> {
        let Content::Mcp { definition } = &item.content else {
            return Ok(Vec::new());
        };
        let slots = mcp::binding_slots(definition)?;
        Ok(self
            .destinations(target, item)?
            .into_iter()
            .map(|id| (id, slots.clone()))
            .collect())
    }
}

#[allow(clippy::too_many_arguments)] // Inventory metadata has no native payload or path in diagnostics.
fn candidate(
    locator: &str,
    name: &str,
    source: Target,
    kind: Kind,
    classification: &str,
    content: Option<Content>,
    requirements: Vec<String>,
    detail: &str,
) -> InventoryCandidate {
    InventoryCandidate {
        id: opaque(&[locator, name]),
        name: name.into(),
        source,
        kind,
        classification: classification.into(),
        detail: detail.into(),
        locator: locator.into(),
        content,
        requirements,
    }
}
fn native_entries(path: &Path, target: Target) -> Result<BTreeMap<String, Value>> {
    let Some(bytes) = optional_file(path)? else {
        return Ok(BTreeMap::new());
    };
    if target == Target::Codex {
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| error("native MCP configuration is not UTF-8"))?;
        let value: toml::Value = toml::from_str(text)
            .map_err(|_| error("native MCP configuration could not be parsed"))?;
        let Some(servers) = value.get("mcp_servers") else {
            return Ok(BTreeMap::new());
        };
        servers
            .as_table()
            .ok_or_else(|| error("invalid native MCP server table"))?
            .iter()
            .map(|(k, v)| Ok((k.clone(), serde_json::to_value(v)?)))
            .collect()
    } else {
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| error("native MCP configuration could not be parsed"))?;
        if !value.is_object() {
            return Err(error("invalid native MCP configuration"));
        }
        let Some(servers) = value.get("mcpServers") else {
            return Ok(BTreeMap::new());
        };
        Ok(servers
            .as_object()
            .ok_or_else(|| error("invalid native MCP server map"))?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }
}
fn edit_native(
    bytes: Option<&[u8]>,
    target: Target,
    name: &str,
    entry: Option<&Value>,
) -> Result<Vec<u8>> {
    if target == Target::Codex {
        let text = std::str::from_utf8(bytes.unwrap_or(b""))
            .map_err(|_| error("native MCP configuration is not UTF-8"))?;
        let mut doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|_| error("native MCP configuration could not be parsed"))?;
        if let Some(entry) = entry {
            if doc.get("mcp_servers").is_none() {
                doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let table = doc["mcp_servers"]
                .as_table_like_mut()
                .ok_or_else(|| error("unsupported native MCP server table"))?;
            let wrapped = json!({"server":entry});
            let encoded = toml::to_string(&wrapped)
                .map_err(|_| error("could not encode native MCP setup"))?;
            let mut one = encoded
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| error("could not encode native MCP setup"))?;
            table.insert(
                name,
                one.as_table_mut()
                    .remove("server")
                    .ok_or_else(|| error("could not encode native MCP setup"))?,
            );
        } else if let Some(table) = doc
            .get_mut("mcp_servers")
            .and_then(toml_edit::Item::as_table_like_mut)
        {
            table.remove(name);
        }
        Ok(doc.to_string().into_bytes())
    } else {
        let mut doc: Value = serde_json::from_slice(bytes.unwrap_or(b"{}"))
            .map_err(|_| error("native MCP configuration could not be parsed"))?;
        let obj = doc
            .as_object_mut()
            .ok_or_else(|| error("invalid native MCP configuration"))?;
        if let Some(entry) = entry {
            let servers = obj
                .entry("mcpServers")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| error("invalid native MCP server map"))?;
            servers.insert(name.into(), entry.clone());
        } else if let Some(servers) = obj.get_mut("mcpServers").and_then(Value::as_object_mut) {
            servers.remove(name);
        }
        serde_json::to_vec_pretty(&doc).map_err(Into::into)
    }
}

impl Adapter for NativeAdapter<'_> {
    fn ready(&self, target: Target) -> Result<bool> {
        (self.ready)(target)
    }
    fn recover_pending(&mut self, target: Target, item: &LibraryItem, locator: &str) -> Result<()> {
        let dest = self.resolve(target, item, locator)?;
        self.recover(&dest)
    }
    fn destinations(&mut self, target: Target, item: &LibraryItem) -> Result<Vec<String>> {
        if !skills::safe_name(&item.name)
            || (target == Target::ClaudeCode
                && item.content.kind() == Kind::Skill
                && matches!(item.name.as_str(), "synced" | "anthropic-skills"))
        {
            return Err(error(
                "an item name is reserved or cannot be installed safely",
            ));
        }
        let paths = match item.content.kind() {
            Kind::Mcp => self.configs(target),
            Kind::Skill => {
                let roots = self.skill_roots(target);
                if target == Target::Codex {
                    let existing = roots
                        .iter()
                        .map(|p| p.join(&item.name))
                        .filter(|p| p.exists())
                        .collect::<Vec<_>>();
                    if existing.is_empty() {
                        roots
                            .into_iter()
                            .take(1)
                            .map(|p| p.join(&item.name))
                            .collect()
                    } else {
                        existing
                    }
                } else {
                    roots.into_iter().map(|p| p.join(&item.name)).collect()
                }
            }
        };
        Ok(paths
            .into_iter()
            .map(|path| self.register(target, item.content.kind(), path))
            .collect())
    }
    fn inspect(
        &mut self,
        target: Target,
        item: &LibraryItem,
        locator: &str,
    ) -> Result<Option<Content>> {
        let dest = self.resolve(target, item, locator)?;
        if item.content.kind() == Kind::Skill {
            return if missing(&dest.path)? {
                Ok(None)
            } else {
                skills::capture(&dest.path).map(Some)
            };
        }
        let entries = native_entries(&dest.path, target)?;
        let Some(entry) = entries.get(&item.name) else {
            return Ok(None);
        };
        let entry_hash = hash(&serde_json::to_vec(entry)?);
        if let Some(stamp) = self.read_stamp(locator, &item.name)?
            && let Some(Content::Mcp { definition }) = stamp.content
        {
            if stamp.entry_hash == entry_hash {
                return Ok(Some(Content::Mcp { definition }));
            }
            return Ok(Some(Content::Mcp {
                definition: mcp::observe(entry, target, &definition)?,
            }));
        }
        Ok(Some(Content::Mcp {
            definition: mcp::normalize(entry, target)?,
        }))
    }
    fn apply(
        &mut self,
        target: Target,
        item: &LibraryItem,
        locator: &str,
        expected: Option<&str>,
    ) -> Result<ApplyResult> {
        if !(self.ready)(target)? {
            return Ok(outcome(
                InstallStatus::WaitingForApp,
                "Quit the relevant app and CLI sessions to install this item.",
                false,
            ));
        }
        let dest = self.resolve(target, item, locator)?;
        self.recover(&dest)?;
        let current = self.inspect(target, item, locator)?;
        let actual = current.as_ref().map(content_digest).transpose()?;
        if actual.as_deref() != expected {
            return Ok(outcome(
                InstallStatus::Conflict,
                "The native item changed; its local version was preserved.",
                false,
            ));
        }
        let before = raw_hash(&dest.path, dest.kind)?;
        if dest.kind == Kind::Skill {
            let current_hash = actual.clone();
            if !item.active() {
                if current_hash.is_none() {
                    return Ok(outcome(
                        InstallStatus::Ready,
                        "The managed skill is absent.",
                        true,
                    ));
                }
                let stamp = self.read_stamp(locator, &item.name)?;
                if !stamp.is_some_and(|s| Some(s.entry_hash) == current_hash) {
                    return Ok(outcome(
                        InstallStatus::Conflict,
                        "The skill is unowned or changed locally; removal was skipped.",
                        false,
                    ));
                }
                if !self.replace(&dest, before, None, None, None)? {
                    return Ok(outcome(
                        InstallStatus::WaitingForApp,
                        "The app opened during installation; retry after it closes.",
                        false,
                    ));
                }
                return Ok(outcome(
                    InstallStatus::Ready,
                    "The unchanged managed skill was removed.",
                    true,
                ));
            }
            let Content::Skill { files } = &item.content else {
                unreachable!()
            };
            skills::validate(files)?;
            if actual.as_deref() != Some(&content_digest(&item.content)?)
                && !self.replace(
                    &dest,
                    before,
                    Some(files),
                    None,
                    Some(Self::pending_stamp(
                        locator,
                        item,
                        &content_digest(&item.content)?,
                    )),
                )?
            {
                return Ok(outcome(
                    InstallStatus::WaitingForApp,
                    "The app opened during installation; retry after it closes.",
                    false,
                ));
            }
            self.stamp(locator, item, &content_digest(&item.content)?)?;
            let requirements = skills::requirements(&item.content, target)?;
            return Ok(outcome(
                if requirements.is_empty() {
                    InstallStatus::Ready
                } else {
                    InstallStatus::NeedsSetup
                },
                if requirements.is_empty() {
                    "Complete skill installed; scripts have not been executed."
                } else {
                    "Skill installed with supporting files; review app-specific tools, references and script dependencies before use."
                },
                true,
            ));
        }
        let entries = native_entries(&dest.path, target)?;
        let entry = entries.get(&item.name);
        if !item.active() {
            if entry.is_none() {
                return Ok(outcome(
                    InstallStatus::Ready,
                    "The managed MCP setup is absent.",
                    true,
                ));
            }
            let raw = hash(&serde_json::to_vec(entry.unwrap())?);
            if !self
                .read_stamp(locator, &item.name)?
                .is_some_and(|s| s.entry_hash == raw)
            {
                return Ok(outcome(
                    InstallStatus::Conflict,
                    "Local MCP settings or credentials changed; removal was skipped.",
                    false,
                ));
            }
        }
        let Content::Mcp { definition } = &item.content else {
            unreachable!()
        };
        let mut remote = false;
        let native = if item.active() {
            let slots = mcp::binding_slots(definition)?;
            let bindings = slots
                .iter()
                .filter_map(|slot| {
                    self.bindings
                        .get(&binding_key(&item.id, locator, slot))
                        .map(|value| (slot.clone(), value.clone()))
                })
                .collect();
            // Old destination-only keys have no item ownership. Keep them in
            // settings, but never guess which server they belonged to. The
            // renderer can safely retain a value already in this server's
            // native entry; missing values require an explicit item binding.
            let mut rendered = mcp::render(definition, target, entry, &bindings)?;
            remote = rendered.remote;
            if rendered.native.is_none() {
                if slots
                    .iter()
                    .any(|slot| self.bindings.contains_key(&format!("{locator}:{slot}")))
                {
                    rendered.requirements.push(
                        "Rebind this item's missing local slots; older bindings were shared between MCP setups and cannot be assigned safely.".into(),
                    );
                }
                return Ok(outcome(
                    InstallStatus::NeedsSetup,
                    &rendered.requirements.join(" "),
                    false,
                ));
            }
            rendered.native
        } else {
            None
        };
        let original = optional_file(&dest.path)?;
        if original.as_ref().map(|v| hash(v)) != before {
            return Ok(outcome(
                InstallStatus::Conflict,
                "Native settings changed before staging; they were preserved.",
                false,
            ));
        }
        let bytes = edit_native(original.as_deref(), target, &item.name, native.as_ref())?;
        let mut pending_stamp = native
            .as_ref()
            .map(|v| {
                serde_json::to_vec(v).map(|bytes| Self::pending_stamp(locator, item, &hash(&bytes)))
            })
            .transpose()?;
        if let Some(stamp) = &pending_stamp
            && self.preserve_local_stamp(locator, item, &stamp.stamp.entry_hash)?
        {
            pending_stamp = None;
        }
        if Some(hash(&bytes)) != before
            && !self.replace(&dest, before, None, Some(&bytes), pending_stamp)?
        {
            return Ok(outcome(
                InstallStatus::WaitingForApp,
                "The app opened during installation; retry after it closes.",
                false,
            ));
        }
        let locally_disabled = native
            .as_ref()
            .and_then(|v| v.get("enabled"))
            .and_then(Value::as_bool)
            == Some(false);
        if let Some(native) = native {
            self.stamp(locator, item, &hash(&serde_json::to_vec(&native)?))?;
        }
        Ok(outcome(
            if locally_disabled {
                InstallStatus::NeedsSetup
            } else if remote {
                InstallStatus::SignInNeeded
            } else {
                InstallStatus::Ready
            },
            if locally_disabled {
                "MCP definition installed; this server remains disabled by its existing local settings."
            } else if remote {
                "MCP definition installed. Complete authentication in the app if required; connection status has not been verified."
            } else if item.active() {
                "MCP definition installed; server execution and dependency health have not been tested."
            } else {
                "The unchanged managed MCP setup was removed; unrelated settings were preserved."
            },
            true,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    fn roots(home: &Path) -> NativeRoots {
        NativeRoots {
            user_home: home.into(),
            codex_homes: vec![home.join(".codex")],
            codex_skill_roots: vec![home.join(".agents/skills")],
            claude_homes: vec![home.join(".claude"), home.join("work-claude")],
            claude_config_files: vec![],
        }
    }
    fn skill() -> LibraryItem {
        LibraryItem {
            id: uuid::Uuid::new_v4().to_string(),
            name: "sample".into(),
            targets: [Target::Codex, Target::ClaudeCode].into(),
            enabled: true,
            deleted: false,
            content: Content::Skill {
                files: vec![super::super::model::SkillFile {
                    path: "SKILL.md".into(),
                    content_base64: STANDARD
                        .encode("---\nname: sample\ndescription: fixture\n---\nSynthetic skill.\n"),
                    executable: false,
                }],
            },
            requirements: vec![],
        }
    }
    #[test]
    fn skill_fanout_adopts_and_removes_only_unchanged_owned() {
        let t = tempfile::tempdir().unwrap();
        let ready = |_| Ok(true);
        let mut a = NativeAdapter::new(
            roots(t.path()),
            t.path().join("local"),
            BTreeMap::new(),
            &ready,
        );
        let mut item = skill();
        for target in [Target::Codex, Target::ClaudeCode] {
            for loc in a.destinations(target, &item).unwrap() {
                assert!(a.apply(target, &item, &loc, None).unwrap().applied);
                assert_eq!(
                    a.inspect(target, &item, &loc).unwrap(),
                    Some(item.content.clone())
                );
            }
        }
        let loc = a.destinations(Target::Codex, &item).unwrap().remove(0);
        fs::write(
            t.path().join(".agents/skills/sample/extra"),
            "new local content",
        )
        .unwrap();
        let changed =
            content_digest(&a.inspect(Target::Codex, &item, &loc).unwrap().unwrap()).unwrap();
        item.deleted = true;
        assert_eq!(
            a.apply(Target::Codex, &item, &loc, Some(&changed))
                .unwrap()
                .status,
            InstallStatus::Conflict
        );
        assert!(t.path().join(".agents/skills/sample/extra").exists());
    }
    #[test]
    fn mcp_preserves_comments_models_accounts_and_destination_secrets() {
        let t = tempfile::tempdir().unwrap();
        let r = roots(t.path());
        fs::create_dir_all(&r.codex_homes[0]).unwrap();
        let path = r.codex_homes[0].join("config.toml");
        fs::write(&path,"# retained comment\nmodel='synthetic-model'\nmodel_provider='synthetic-provider'\n[mcp_servers.keep]\nurl='https://example.net/mcp'\n[mcp_servers.sample]\nurl='https://example.com/mcp'\n[mcp_servers.sample.http_headers]\nAuthorization='destination-secret'\n").unwrap();
        let ready = |_| Ok(true);
        let mut a = NativeAdapter::new(r, t.path().join("local"), BTreeMap::new(), &ready);
        let mut item = skill();
        item.content=Content::Mcp{definition:mcp::normalize(&json!({"url":"https://example.com/mcp","http_headers":{"Authorization":"SOURCE_SECRET"}}),Target::Codex).unwrap()};
        let loc = a.destinations(Target::Codex, &item).unwrap().remove(0);
        let expected =
            content_digest(&a.inspect(Target::Codex, &item, &loc).unwrap().unwrap()).unwrap();
        assert!(
            a.apply(Target::Codex, &item, &loc, Some(&expected))
                .unwrap()
                .applied
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# retained comment"));
        assert!(text.contains("synthetic-model"));
        assert!(text.contains("destination-secret"));
        assert!(!text.contains("SOURCE_SECRET"));
        assert!(text.contains("mcp_servers.keep"));
        assert_eq!(
            a.inspect(Target::Codex, &item, &loc).unwrap(),
            Some(item.content.clone())
        );
        let text = text.replace("destination-secret", "changed-local-secret");
        fs::write(&path, text).unwrap();
        let expected =
            content_digest(&a.inspect(Target::Codex, &item, &loc).unwrap().unwrap()).unwrap();
        assert_eq!(expected, content_digest(&item.content).unwrap());
        // A periodic status refresh must not silently take ownership of this
        // account's changed credential and make a later tombstone destructive.
        assert!(
            a.apply(Target::Codex, &item, &loc, Some(&expected))
                .unwrap()
                .applied
        );
        item.deleted = true;
        assert_eq!(
            a.apply(Target::Codex, &item, &loc, Some(&expected))
                .unwrap()
                .status,
            InstallStatus::Conflict
        );
    }
    fn local_mcp(name: &str) -> LibraryItem {
        let mut item = skill();
        item.name = name.into();
        item.content = Content::Mcp {
            definition: mcp::normalize(
                &json!({"command":"python3","args":["/synthetic/source/server.py", "/synthetic/source/data"]}),
                Target::Codex,
            )
            .unwrap(),
        };
        item
    }

    #[test]
    fn mcp_bindings_isolate_items_slots_targets_and_account_destinations() {
        let t = tempfile::tempdir().unwrap();
        let mut roots = roots(t.path());
        roots.codex_homes.push(t.path().join("second-codex"));
        let ready = |_| Ok(true);
        let mut adapter = NativeAdapter::new(
            roots.clone(),
            t.path().join("local"),
            BTreeMap::new(),
            &ready,
        );
        let items = [local_mcp("first"), local_mcp("second")];
        let mut settings = super::super::Settings::default();
        let mut installations = Vec::new();
        for target in [Target::Codex, Target::ClaudeCode] {
            for item in &items {
                for (index, (locator, slots)) in adapter
                    .binding_destinations(item, target)
                    .unwrap()
                    .into_iter()
                    .enumerate()
                {
                    assert_eq!(slots.len(), 2);
                    let args: Vec<_> = (0..2)
                        .map(|slot| {
                            format!("/synthetic/{}/{index}/{}/{slot}", target.key(), item.name)
                        })
                        .collect();
                    for (slot, value) in args.iter().enumerate() {
                        settings.bind(
                            &item.id,
                            &locator,
                            &format!("argument:{slot}"),
                            format!("path:{value}"),
                        );
                    }
                    installations.push((target, locator, item.clone(), args));
                }
            }
        }
        assert_eq!(installations.len(), 8);
        // Exercise the same local settings writer and round trip as `bind`.
        let paths = super::super::Paths::at(t.path().into());
        paths.save(&settings).unwrap();
        let mut adapter = NativeAdapter::new(
            roots,
            t.path().join("local"),
            paths.load().unwrap().bindings,
            &ready,
        );
        for _ in 0..2 {
            for (target, locator, item, args) in &installations {
                let expected = adapter
                    .inspect(*target, item, locator)
                    .unwrap()
                    .as_ref()
                    .map(content_digest)
                    .transpose()
                    .unwrap();
                let result = adapter
                    .apply(*target, item, locator, expected.as_deref())
                    .unwrap();
                assert_eq!(result.status, InstallStatus::Ready);
                assert!(result.applied);
                let destination = adapter.resolve(*target, item, locator).unwrap();
                let entry = &native_entries(&destination.path, *target).unwrap()[&item.name];
                assert_eq!(entry["args"], json!(args));
            }
        }
        assert_eq!(settings.bindings.len(), 16);
    }

    #[test]
    fn legacy_mcp_bindings_require_item_rebinding_without_changing_native_files() {
        let t = tempfile::tempdir().unwrap();
        let ready = |_| Ok(true);
        let mut adapter = NativeAdapter::new(
            roots(t.path()),
            t.path().join("local"),
            BTreeMap::new(),
            &ready,
        );
        let items = [local_mcp("first"), local_mcp("second")];
        let locator = adapter
            .destinations(Target::Codex, &items[0])
            .unwrap()
            .remove(0);
        let path = t.path().join(".codex/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = b"# Preserve unrelated settings\nmodel='synthetic-model'\n";
        fs::write(&path, original).unwrap();
        let mut settings = super::super::Settings::default();
        for slot in ["argument:0", "argument:1"] {
            settings.bindings.insert(
                format!("{locator}:{slot}"),
                "path:/synthetic/ambiguous".into(),
            );
        }
        let legacy = settings.bindings.clone();
        adapter.bindings = legacy.clone();
        for item in &items {
            let result = adapter.apply(Target::Codex, item, &locator, None).unwrap();
            assert_eq!(result.status, InstallStatus::NeedsSetup);
            assert!(!result.applied);
            assert!(
                result
                    .detail
                    .contains("Rebind this item's missing local slots")
            );
            assert_eq!(fs::read(&path).unwrap(), original);
        }
        // Explicitly rebinding one item must neither consume the old keys nor
        // make the other item installable with that item's private path.
        for slot in ["argument:0", "argument:1"] {
            settings.bind(&items[0].id, &locator, slot, "path:/synthetic/first".into());
        }
        let paths = super::super::Paths::at(t.path().into());
        paths.save(&settings).unwrap();
        adapter.bindings = paths.load().unwrap().bindings;
        for (key, value) in legacy {
            assert_eq!(adapter.bindings.get(&key), Some(&value));
        }
        assert!(
            adapter
                .apply(Target::Codex, &items[0], &locator, None)
                .unwrap()
                .applied
        );
        let installed = fs::read(&path).unwrap();
        let result = adapter
            .apply(Target::Codex, &items[1], &locator, None)
            .unwrap();
        assert_eq!(result.status, InstallStatus::NeedsSetup);
        assert_eq!(fs::read(&path).unwrap(), installed);
    }

    #[test]
    fn legacy_mcp_bindings_preserve_each_existing_servers_local_values() {
        let t = tempfile::tempdir().unwrap();
        let ready = |_| Ok(true);
        let mut adapter = NativeAdapter::new(
            roots(t.path()),
            t.path().join("local"),
            BTreeMap::new(),
            &ready,
        );
        let items = [local_mcp("first"), local_mcp("second")];
        let locator = adapter
            .destinations(Target::Codex, &items[0])
            .unwrap()
            .remove(0);
        let path = t.path().join(".codex/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = b"# Keep this comment\nmodel='synthetic-model'\n".to_vec();
        for item in &items {
            bytes = edit_native(Some(&bytes), Target::Codex, &item.name,
                Some(&json!({"command":"python3","args":[format!("/synthetic/{}", item.name),"/synthetic/data"]}))).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        for slot in ["argument:0", "argument:1"] {
            adapter.bindings.insert(
                format!("{locator}:{slot}"),
                "path:/synthetic/ambiguous".into(),
            );
        }
        for item in &items {
            let before = native_entries(&path, Target::Codex).unwrap();
            let expected = content_digest(
                &adapter
                    .inspect(Target::Codex, item, &locator)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            let result = adapter
                .apply(Target::Codex, item, &locator, Some(&expected))
                .unwrap();
            assert_eq!(result.status, InstallStatus::Ready);
            assert_eq!(native_entries(&path, Target::Codex).unwrap(), before);
        }
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("# Keep this comment"));
        assert!(text.contains("synthetic-model"));
        assert!(!text.contains("ambiguous"));
    }

    #[test]
    fn mcp_binding_follows_item_identity_across_rename_but_not_a_new_same_name_item() {
        let t = tempfile::tempdir().unwrap();
        let ready = |_| Ok(true);
        let mut adapter = NativeAdapter::new(
            roots(t.path()),
            t.path().join("local"),
            BTreeMap::new(),
            &ready,
        );
        let mut item = local_mcp("original");
        let locator = adapter
            .destinations(Target::Codex, &item)
            .unwrap()
            .remove(0);
        let mut settings = super::super::Settings::default();
        for slot in ["argument:0", "argument:1"] {
            settings.bind(&item.id, &locator, slot, "path:/synthetic/original".into());
        }
        adapter.bindings = settings.bindings;
        item.name = "renamed".into();
        assert!(
            adapter
                .apply(Target::Codex, &item, &locator, None)
                .unwrap()
                .applied
        );
        let other = local_mcp("original");
        let result = adapter
            .apply(Target::Codex, &other, &locator, None)
            .unwrap();
        assert_eq!(result.status, InstallStatus::NeedsSetup);
        assert!(!result.applied);
        assert_eq!(
            adapter.inspect(Target::Codex, &other, &locator).unwrap(),
            None
        );
    }

    #[test]
    fn staging_race_preserves_new_native_content() {
        use std::cell::Cell;
        let t = tempfile::tempdir().unwrap();
        let item = skill();
        let calls = Cell::new(0);
        let root = t.path().join(".agents/skills/sample");
        let ready = |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                fs::create_dir_all(&root).unwrap();
                fs::write(root.join("SKILL.md"), "local concurrent content").unwrap();
            }
            Ok(true)
        };
        let mut a = NativeAdapter::new(
            roots(t.path()),
            t.path().join("local"),
            BTreeMap::new(),
            &ready,
        );
        let loc = a.destinations(Target::Codex, &item).unwrap().remove(0);
        assert!(a.apply(Target::Codex, &item, &loc, None).is_err());
        assert_eq!(
            fs::read_to_string(root.join("SKILL.md")).unwrap(),
            "local concurrent content"
        );
    }
    #[test]
    fn inventory_is_read_only_and_never_exports_inline_secrets() {
        let t = tempfile::tempdir().unwrap();
        let r = roots(t.path());
        fs::create_dir_all(&r.codex_homes[0]).unwrap();
        let p = r.codex_homes[0].join("config.toml");
        let original = "[mcp_servers.example]\ncommand='node'\nargs=['SECRET_ARG']\n[mcp_servers.example.env]\nAPI_KEY='SECRET_ENV'\n";
        fs::write(&p, original).unwrap();
        let ready = |_| Ok(true);
        let local = t.path().join("local");
        let mut a = NativeAdapter::new(r, local.clone(), BTreeMap::new(), &ready);
        let rows = a.inventory().unwrap();
        assert_eq!(rows.len(), 1);
        let out = serde_json::to_string(&rows).unwrap();
        assert!(!out.contains("SECRET"));
        assert!(!out.contains(t.path().to_str().unwrap()));
        assert!(!local.exists());
        assert_eq!(fs::read_to_string(p).unwrap(), original);
    }
    #[test]
    fn recovery_never_replaces_content_created_during_readiness_check() {
        let t = tempfile::tempdir().unwrap();
        let item = skill();
        let target = t.path().join(".agents/skills/sample");
        let ready = |_| {
            fs::create_dir_all(&target).unwrap();
            fs::write(target.join("SKILL.md"), "new user content").unwrap();
            Ok(true)
        };
        let mut a = NativeAdapter::new(
            roots(t.path()),
            t.path().join("local"),
            BTreeMap::new(),
            &ready,
        );
        let loc = a.destinations(Target::Codex, &item).unwrap().remove(0);
        let dest = a.resolve(Target::Codex, &item, &loc).unwrap();
        let dir = a.journal_path(&dest);
        private_dir(&dir).unwrap();
        let Content::Skill { files } = &item.content else {
            panic!()
        };
        skills::stage(&dir.join("old"), files).unwrap();
        let before = raw_hash(&dir.join("old"), Kind::Skill).unwrap();
        write_private(
            &dir.join("journal.json"),
            &serde_json::to_vec(&Journal {
                version: 1,
                target: target.clone(),
                kind: Kind::Skill,
                before,
                after: Some("new-hash".into()),
                stamp: None,
            })
            .unwrap(),
            false,
        )
        .unwrap();
        assert!(a.recover_pending(Target::Codex, &item, &loc).is_err());
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).unwrap(),
            "new user content"
        );
        assert!(dir.join("old/SKILL.md").exists());
    }
    #[test]
    fn engine_recovers_native_backup_before_deciding_pending_conflict() {
        use super::super::{engine, storage::Archive};
        let t = tempfile::tempdir().unwrap();
        let ready = |_| Ok(true);
        let local = t.path().join("local");
        let cloud = t.path().join("cloud");
        let state_path = t.path().join("state.json");
        let mut a = NativeAdapter::new(roots(t.path()), local.clone(), BTreeMap::new(), &ready);
        let mut old = skill();
        old.targets = [Target::Codex].into();
        let mut archive = Archive::default();
        let old_revision = archive.publish(&cloud, old.clone(), vec![]).unwrap();
        let mut state = engine::State::default();
        engine::run(
            &cloud,
            &state_path,
            &mut state,
            &[Kind::Skill].into(),
            &mut a,
        )
        .unwrap();
        let mut next = old.clone();
        let Content::Skill { files } = &mut next.content else {
            panic!()
        };
        files[0].content_base64 = STANDARD
            .encode("---\nname: sample\ndescription: fixture\n---\nNext complete revision.");
        let new_revision = archive
            .publish(&cloud, next.clone(), vec![old_revision])
            .unwrap();
        let receipt = state.receipts[0].clone();
        let loc = receipt.locator.clone();
        let dest = a.resolve(Target::Codex, &next, &loc).unwrap();
        let dir = a.journal_path(&dest);
        private_dir(&dir).unwrap();
        let before = raw_hash(&dest.path, Kind::Skill).unwrap();
        let Content::Skill { files } = &next.content else {
            panic!()
        };
        skills::stage(&dir.join("new"), files).unwrap();
        let after = raw_hash(&dir.join("new"), Kind::Skill).unwrap();
        write_private(
            &dir.join("journal.json"),
            &serde_json::to_vec(&Journal {
                version: 1,
                target: dest.path.clone(),
                kind: Kind::Skill,
                before: before.clone(),
                after: after.clone(),
                stamp: Some(NativeAdapter::pending_stamp(
                    &loc,
                    &next,
                    after.as_deref().unwrap(),
                )),
            })
            .unwrap(),
            false,
        )
        .unwrap();
        rename_new(&dest.path, &dir.join("old")).unwrap();
        let mut receipt_value = serde_json::to_value(receipt).unwrap();
        receipt_value["revision"] = json!(new_revision);
        receipt_value["baseline"] = json!(content_digest(&next.content).unwrap());
        let mut value = serde_json::to_value(&state).unwrap();
        value["pending"] = json!([{"receipt":receipt_value,"before":before}]);
        state = serde_json::from_value(value).unwrap();
        state.save(&state_path).unwrap();
        drop(a);
        let mut a = NativeAdapter::new(roots(t.path()), local, BTreeMap::new(), &ready);
        let mut state = engine::State::load(&state_path).unwrap();
        let report = engine::run(
            &cloud,
            &state_path,
            &mut state,
            &[Kind::Skill].into(),
            &mut a,
        )
        .unwrap();
        assert_eq!(report.conflicts, 0);
        assert_eq!(
            a.inspect(Target::Codex, &next, &loc).unwrap(),
            Some(next.content)
        );
        assert!(!dir.exists());
    }
    #[test]
    fn committed_mcp_journal_restores_canonical_stamp_before_inspection() {
        let t = tempfile::tempdir().unwrap();
        let ready = |_| Ok(true);
        let mut a = NativeAdapter::new(
            roots(t.path()),
            t.path().join("local"),
            BTreeMap::new(),
            &ready,
        );
        let mut item = skill();
        item.content = Content::Mcp {
            definition: mcp::normalize(&json!({"url":"https://example.com/mcp"}), Target::Codex)
                .unwrap(),
        };
        let loc = a.destinations(Target::Codex, &item).unwrap().remove(0);
        let dest = a.resolve(Target::Codex, &item, &loc).unwrap();
        let entry = json!({"url":"https://example.com/mcp","http_headers":{"Authorization":"local-only-secret"}});
        fs::create_dir_all(dest.path.parent().unwrap()).unwrap();
        fs::write(
            &dest.path,
            edit_native(None, Target::Codex, &item.name, Some(&entry)).unwrap(),
        )
        .unwrap();
        let dir = a.journal_path(&dest);
        private_dir(&dir).unwrap();
        let after = raw_hash(&dest.path, Kind::Mcp).unwrap();
        let stamp =
            NativeAdapter::pending_stamp(&loc, &item, &hash(&serde_json::to_vec(&entry).unwrap()));
        write_private(
            &dir.join("journal.json"),
            &serde_json::to_vec(&Journal {
                version: 1,
                target: dest.path.clone(),
                kind: Kind::Mcp,
                before: None,
                after,
                stamp: Some(stamp),
            })
            .unwrap(),
            false,
        )
        .unwrap();
        a.recover_pending(Target::Codex, &item, &loc).unwrap();
        assert_eq!(
            a.inspect(Target::Codex, &item, &loc).unwrap(),
            Some(item.content)
        );
        assert!(!dir.exists());
    }
    #[test]
    #[ignore = "Starts an installed official client against synthetic roots only"]
    fn installed_codex_discovers_installed_skill_and_supporting_files() {
        use std::{
            io::{BufRead, BufReader},
            process::{Command, Stdio},
            sync::mpsc,
            time::{Duration, Instant},
        };
        let binary = [
            "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex",
            "/Applications/Codex.app/Contents/Resources/codex",
            "/opt/homebrew/bin/codex",
            "/usr/local/bin/codex",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .expect("Install an official Codex client before requesting this smoke test");
        let t = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(t.path()).unwrap();
        let ready = |_| Ok(true);
        let mut item = skill();
        if let Content::Skill { files } = &mut item.content {
            files.push(super::super::model::SkillFile {
                path: "references/proof.txt".into(),
                content_base64: STANDARD.encode("Synthetic supporting resource."),
                executable: false,
            });
        }
        let mut adapter = NativeAdapter::new(
            roots(&home),
            home.join("library-local"),
            BTreeMap::new(),
            &ready,
        );
        let locator = adapter
            .destinations(Target::Codex, &item)
            .unwrap()
            .remove(0);
        assert!(
            adapter
                .apply(Target::Codex, &item, &locator, None)
                .unwrap()
                .applied
        );
        fs::create_dir_all(home.join(".codex")).unwrap();
        let mut child = Command::new(binary)
            .args([
                "app-server",
                "--stdio",
                "-c",
                "model_provider=\"sync_fixture\"",
                "-c",
                "model_providers.sync_fixture.name=\"Synthetic library test\"",
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
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("CODEX_HOME", home.join(".codex"))
            .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        struct Guard(std::process::Child);
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let _guard = Guard(child);
        let (send, recv) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && send.send(value).is_err()
                {
                    break;
                }
            }
        });
        let mut request = |id: u64, method: &str, params: Value| -> Value {
            writeln!(
                input,
                "{}",
                json!({"id":id,"method":method,"params":params})
            )
            .unwrap();
            input.flush().unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let value = recv
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("synthetic native discovery timed out");
                if value["id"] == id {
                    assert!(
                        value.get("error").is_none(),
                        "native synthetic method was rejected"
                    );
                    break value["result"].clone();
                }
            }
        };
        request(
            1,
            "initialize",
            json!({"clientInfo":{"name":"switchboard_library_fixture","version":"1"},"capabilities":{"experimentalApi":true}}),
        );
        let listed = request(2, "skills/list", json!({"cwds":[home],"forceReload":true}));
        fn find_skill(value: &Value) -> bool {
            match value {
                Value::Object(map) => {
                    map.get("name").and_then(Value::as_str) == Some("sample")
                        || map.values().any(find_skill)
                }
                Value::Array(items) => items.iter().any(find_skill),
                _ => false,
            }
        }
        assert!(
            find_skill(&listed),
            "the official client did not discover the installed synthetic skill"
        );
        assert_eq!(
            fs::read(home.join(".agents/skills/sample/references/proof.txt")).unwrap(),
            b"Synthetic supporting resource."
        );
        assert!(!home.join(".codex/auth.json").exists());
    }
}
