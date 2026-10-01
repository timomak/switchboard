use super::model::{Content, LibraryItem};
pub use crate::chat_sync::engine::{
    atomic_write, digest, error, private_dir, read_regular, safe_child,
};
use crate::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

pub const VERSION: u32 = 1;
pub const MAX_PACKAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SCAN_BYTES: u64 = 128 * 1024 * 1024;
const MAX_REVISIONS: usize = 10_000;

fn archive_io_error(path: &Path, source: std::io::Error) -> AppError {
    let reason = if source.kind() == std::io::ErrorKind::PermissionDenied
        || (cfg!(unix) && source.raw_os_error() == Some(1))
    {
        "access denied; check this app or terminal's permission to read the folder, then retry"
    } else {
        "the folder or file could not be read; check its availability and try again"
    };
    error(&format!(
        "Cannot read the shared library at {}: {reason}.",
        crate::display::sanitize_untrusted_path(path)
    ))
}

fn archive_read_error(path: &Path, source: AppError) -> AppError {
    match source {
        AppError::Io { source, .. } | AppError::IoBare(source) => archive_io_error(path, source),
        other => other,
    }
}

/// An absent descendant is not proof that its existing iCloud ancestor is
/// readable: macOS privacy controls can permit metadata but deny enumeration.
/// Never interpret that case as an empty library. This probe performs no writes.
pub(super) fn readable_directory(path: &Path) -> Result<bool> {
    readable_directory_with(
        path,
        |p| std::fs::symlink_metadata(p).map(|m| m.is_dir() && !m.file_type().is_symlink()),
        |p| std::fs::read_dir(p)?.next().transpose().map(|_| ()),
    )
}

fn readable_directory_with(
    path: &Path,
    inspect: impl Fn(&Path) -> std::io::Result<bool>,
    probe: impl Fn(&Path) -> std::io::Result<()>,
) -> Result<bool> {
    for ancestor in path.ancestors() {
        let ancestor = if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        };
        match inspect(ancestor) {
            Ok(true) => {
                probe(ancestor).map_err(|err| archive_io_error(ancestor, err))?;
                return Ok(ancestor == path);
            }
            Ok(false) => {
                return Err(error(
                    "A shared library path is not a regular directory; the library could not be inspected.",
                ));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(archive_io_error(ancestor, err)),
        }
    }
    Ok(false)
}

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
        if !readable_directory(root)? {
            return Ok(archive);
        }
        let items = safe_child(root, "items")?;
        if !readable_directory(&items)? {
            return Ok(archive);
        }
        let mut total = 0u64;
        let mut entries = 0usize;
        for directory in std::fs::read_dir(&items).map_err(|err| archive_io_error(&items, err))? {
            entries += 1;
            if entries > MAX_REVISIONS {
                return Err(error(
                    "The library exceeds the supported archive entry count.",
                ));
            }
            let directory = directory.map_err(|err| archive_io_error(&items, err))?;
            let id = directory.file_name().to_string_lossy().into_owned();
            if id.starts_with('.') {
                archive.pending += usize::from(id.ends_with(".icloud"));
                continue;
            }
            if uuid::Uuid::parse_str(&id).is_err()
                || !directory
                    .file_type()
                    .map_err(|err| archive_io_error(&directory.path(), err))?
                    .is_dir()
            {
                return Err(error("The library archive contains an unexpected entry."));
            }
            let folder = safe_child(&items, &id)?;
            for entry in std::fs::read_dir(&folder).map_err(|err| archive_io_error(&folder, err))? {
                entries += 1;
                if entries > MAX_REVISIONS {
                    return Err(error(
                        "The library exceeds the 10,000 revision limit; no revisions were discarded.",
                    ));
                }
                let entry = entry.map_err(|err| archive_io_error(&folder, err))?;
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
                let size = path
                    .symlink_metadata()
                    .map_err(|err| archive_io_error(&path, err))?
                    .len();
                total = total.saturating_add(size);
                if total > MAX_SCAN_BYTES {
                    return Err(error(
                        "The library exceeds the 128 MiB scan limit; no revisions were discarded.",
                    ));
                }
                let bytes = read_regular(&path, MAX_PACKAGE_BYTES)
                    .map_err(|err| archive_read_error(&path, err))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn missing_descendant_of_denied_archive_directory_is_not_empty() {
        for denied in [
            Error::new(ErrorKind::PermissionDenied, "PRIVATE_SENTINEL"),
            #[cfg(unix)]
            Error::from_raw_os_error(1),
        ] {
            let denied_kind = denied.kind();
            let denied_code = denied.raw_os_error();
            let result = readable_directory_with(
                Path::new("/synthetic/private-library/not-delivered/items"),
                |path| {
                    if path == Path::new("/synthetic/private-library") {
                        Ok(true)
                    } else {
                        Err(ErrorKind::NotFound.into())
                    }
                },
                |_| {
                    Err(if let Some(code) = denied_code {
                        Error::from_raw_os_error(code)
                    } else {
                        Error::new(denied_kind, "PRIVATE_SENTINEL")
                    })
                },
            );
            let message = result.unwrap_err().to_string();
            assert!(message.contains("access denied"));
            assert!(message.contains("/synthetic/private-library"));
            assert!(!message.contains("PRIVATE_SENTINEL"));
        }
    }

    #[test]
    fn denied_archive_metadata_is_not_treated_as_absent() {
        let result = readable_directory_with(
            Path::new("/synthetic/library"),
            |_| Err(ErrorKind::PermissionDenied.into()),
            |_| panic!("denied metadata must stop the probe"),
        );
        assert!(result.unwrap_err().to_string().contains("access denied"));
    }

    #[test]
    fn genuinely_missing_readable_archive_stays_empty_without_writes() {
        let t = tempfile::tempdir().unwrap();
        assert!(
            Archive::load(&t.path().join("not-created/archive"))
                .unwrap()
                .revisions
                .is_empty()
        );
        assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 0);
    }
}
