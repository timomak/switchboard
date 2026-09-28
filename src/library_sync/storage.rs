use super::model::{Content, LibraryItem};
use crate::Result;
pub use crate::chat_sync::engine::{
    atomic_write, digest, error, private_dir, read_regular, safe_child,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

pub const VERSION: u32 = 1;
pub const MAX_PACKAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SCAN_BYTES: u64 = 128 * 1024 * 1024;
const MAX_REVISIONS: usize = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub version: u32,
    pub parents: Vec<String>,
    pub item: LibraryItem,
}

#[derive(Default)]
pub struct Archive {
    pub revisions: BTreeMap<String, Package>,
    pub pending: usize,
}

pub fn content_digest(content: &Content) -> Result<String> {
    Ok(digest(&serde_json::to_vec(content)?))
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn validate_item(item: &LibraryItem) -> Result<()> {
    if uuid::Uuid::parse_str(&item.id).is_err()
        || item.name.trim().is_empty()
        || item.name.len() > 160
        || item.name.contains(['/', '\\', '\0'])
        || matches!(item.name.as_str(), "." | "..")
        || item.requirements.len() > 128
        || item.requirements.iter().any(|r| r.len() > 2048)
    {
        return Err(error("A library item has invalid identity or metadata."));
    }
    if let Content::Skill { files } = &item.content {
        use base64::Engine;
        if files.is_empty() || files.len() > 10_000 {
            return Err(error("A skill has an unsupported file count."));
        }
        let mut paths = BTreeSet::new();
        let mut total = 0usize;
        for file in files {
            if file.path.len() > 2048 || file.path.contains('\\') || !paths.insert(&file.path) {
                return Err(error("A skill contains unsafe or duplicate paths."));
            }
            if std::path::Path::new(&file.path)
                .components()
                .any(|p| !matches!(p, std::path::Component::Normal(_)))
                || file.path.is_empty()
            {
                return Err(error("A skill contains an unsafe path."));
            }
            let bytes = base64::prelude::BASE64_STANDARD
                .decode(&file.content_base64)
                .map_err(|_| error("A skill contains invalid file data."))?;
            total = total.saturating_add(bytes.len());
            if total > 20 * 1024 * 1024 {
                return Err(error("A skill exceeds the 20 MiB portable size limit."));
            }
        }
        if !paths.contains(&"SKILL.md".to_owned()) {
            return Err(error("A skill is missing SKILL.md."));
        }
    }
    Ok(())
}

impl Archive {
    pub fn load(root: &Path) -> Result<Self> {
        let mut archive = Self::default();
        let items = safe_child(root, "items")?;
        if !items.try_exists()? {
            return Ok(archive);
        }
        if !items.symlink_metadata()?.is_dir() {
            return Err(error("The library archive is not a directory."));
        }
        let mut total = 0u64;
        let mut entries = 0usize;
        for directory in std::fs::read_dir(&items)? {
            entries += 1;
            if entries > MAX_REVISIONS {
                return Err(error(
                    "The library exceeds the supported archive entry count.",
                ));
            }
            let directory = directory?;
            let id = directory.file_name().to_string_lossy().into_owned();
            if id.starts_with('.') {
                archive.pending += usize::from(id.ends_with(".icloud"));
                continue;
            }
            if uuid::Uuid::parse_str(&id).is_err() || !directory.file_type()?.is_dir() {
                return Err(error("The library archive contains an unexpected entry."));
            }
            let folder = safe_child(&items, &id)?;
            for entry in std::fs::read_dir(&folder)? {
                entries += 1;
                if entries > MAX_REVISIONS {
                    return Err(error(
                        "The library exceeds the 10,000 revision limit; no revisions were discarded.",
                    ));
                }
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    archive.pending += usize::from(name.ends_with(".icloud"));
                    continue;
                }
                let Some(hash) = name.strip_suffix(".json") else {
                    return Err(error("The library contains an unexpected revision file."));
                };
                if !valid_hash(hash) {
                    return Err(error("The library contains an invalid revision name."));
                }
                let path = safe_child(&folder, &name)?;
                let size = path.symlink_metadata()?.len();
                total = total.saturating_add(size);
                if total > MAX_SCAN_BYTES {
                    return Err(error(
                        "The library exceeds the 128 MiB scan limit; no revisions were discarded.",
                    ));
                }
                let bytes = read_regular(&path, MAX_PACKAGE_BYTES)?;
                if digest(&bytes) != hash {
                    archive.pending += 1;
                    continue;
                }
                let package: Package = serde_json::from_slice(&bytes)
                    .map_err(|_| error("A library revision has an unsupported format."))?;
                if package.version != VERSION {
                    return Err(error("Update Switchboard to read this library revision."));
                }
                validate_item(&package.item)?;
                if package.item.id != id
                    || package.parents.len() > 256
                    || package.parents.iter().any(|p| !valid_hash(p))
                {
                    return Err(error("A library revision has invalid ancestry."));
                }
                archive.revisions.insert(hash.to_owned(), package);
            }
        }
        // Missing parents are typical while iCloud is downloading. Do not use
        // an incomplete graph to decide whether edits conflict or supersede.
        let incomplete: BTreeSet<String> = archive
            .revisions
            .iter()
            .filter(|(_, p)| {
                p.parents
                    .iter()
                    .any(|parent| !archive.revisions.contains_key(parent))
            })
            .map(|(_, p)| p.item.id.clone())
            .collect();
        archive.pending += incomplete.len();
        archive
            .revisions
            .retain(|_, p| !incomplete.contains(&p.item.id));
        if archive.revisions.values().any(|p| {
            p.parents.iter().any(|parent| {
                archive
                    .revisions
                    .get(parent)
                    .is_some_and(|ancestor| ancestor.item.id != p.item.id)
            })
        }) {
            return Err(error(
                "A library revision refers to another item's ancestry.",
            ));
        }
        Ok(archive)
    }

    pub fn heads(&self, id: &str) -> Vec<String> {
        let revisions: BTreeSet<_> = self
            .revisions
            .iter()
            .filter(|(_, p)| p.item.id == id)
            .map(|(r, _)| r.clone())
            .collect();
        let parents: BTreeSet<_> = revisions
            .iter()
            .flat_map(|r| self.revisions[r].parents.iter().cloned())
            .collect();
        revisions.difference(&parents).cloned().collect()
    }

    pub fn ids(&self) -> BTreeSet<String> {
        self.revisions.values().map(|p| p.item.id.clone()).collect()
    }

    pub fn publish(
        &mut self,
        root: &Path,
        item: LibraryItem,
        mut parents: Vec<String>,
    ) -> Result<String> {
        validate_item(&item)?;
        parents.sort();
        parents.dedup();
        if parents.len() > 256
            || parents
                .iter()
                .any(|p| self.revisions.get(p).is_none_or(|p| p.item.id != item.id))
        {
            return Err(error(
                "The parent library revision is unavailable; wait for iCloud to finish downloading.",
            ));
        }
        let package = Package {
            version: VERSION,
            parents,
            item,
        };
        let bytes = serde_json::to_vec(&package)?;
        if bytes.len() as u64 > MAX_PACKAGE_BYTES {
            return Err(error(
                "The portable item exceeds the 32 MiB revision limit.",
            ));
        }
        let revision = digest(&bytes);
        let path = safe_child(root, &format!("items/{}/{revision}.json", package.item.id))?;
        let parent = path
            .parent()
            .ok_or_else(|| error("Invalid archive path."))?;
        private_dir(parent)?;
        if path.try_exists()? {
            if read_regular(&path, MAX_PACKAGE_BYTES)? != bytes {
                return Err(error(
                    "An immutable library revision was changed. Its existing data was kept.",
                ));
            }
        } else {
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            if temporary.persist_noclobber(&path).is_err()
                && read_regular(&path, MAX_PACKAGE_BYTES)? != bytes
            {
                return Err(error("A library revision could not be published safely."));
            }
            #[cfg(unix)]
            std::fs::File::open(parent)?.sync_all()?;
        }
        self.revisions.insert(revision.clone(), package);
        Ok(revision)
    }
}
