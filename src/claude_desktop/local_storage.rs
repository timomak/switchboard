//! Chromium's `localStorage` for the `https://claude.ai` origin, as stored in
//! the Claude Desktop data directory's `Local Storage/leveldb`.
//!
//! The renderer keeps its sidebar store (`dframe-store`) and its
//! settings-sync bookkeeping here, so carrying sidebar state between accounts
//! means reading and writing this LevelDB while the app is stopped. Only the
//! record layout Chromium's `LocalStorageImpl` uses is relied on:
//!
//! - key `_<origin>\0\x01<name>` (`\x01` = Latin-1 name, `\x00` = UTF-16LE);
//! - value `\x01<Latin-1 bytes>` or `\x00<UTF-16LE bytes>`.
//!
//! Every read opens a private copy: opening a LevelDB replays and rewrites its
//! log and manifest, which must never happen to the app's own files or to a
//! saved profile. Every write is staged on a copy too, read back through a
//! fresh open, and only then swapped into place.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusty_leveldb::{DB, LdbIterator, Options};

use crate::error::{AppError, Result};

const ORIGIN: &str = "https://claude.ai";
const KEY_LATIN1: u8 = 0x01;
const KEY_UTF16: u8 = 0x00;

fn storage_key(name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(ORIGIN.len() + name.len() + 3);
    key.push(b'_');
    key.extend_from_slice(ORIGIN.as_bytes());
    key.push(0);
    if name.chars().all(|c| (c as u32) < 0x100) {
        key.push(KEY_LATIN1);
        key.extend(name.chars().map(|c| c as u8));
    } else {
        key.push(KEY_UTF16);
        key.extend(name.encode_utf16().flat_map(u16::to_le_bytes));
    }
    key
}

fn decode(bytes: &[u8]) -> Option<String> {
    match *bytes.first()? {
        KEY_LATIN1 => Some(bytes[1..].iter().map(|&b| b as char).collect()),
        KEY_UTF16 => {
            let units: Vec<u16> = bytes[1..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            String::from_utf16(&units).ok()
        }
        _ => None,
    }
}

fn encode(text: &str) -> Vec<u8> {
    if text.chars().all(|c| (c as u32) < 0x100) {
        let mut out = Vec::with_capacity(text.len() + 1);
        out.push(KEY_LATIN1);
        out.extend(text.chars().map(|c| c as u8));
        out
    } else {
        let mut out = Vec::with_capacity(text.len() * 2 + 1);
        out.push(KEY_UTF16);
        out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        out
    }
}

/// The origin's name for a stored key, when the key belongs to it.
fn storage_name(key: &[u8]) -> Option<String> {
    let rest = key.strip_prefix(b"_")?.strip_prefix(ORIGIN.as_bytes())?;
    decode(rest.strip_prefix(&[0])?)
}

fn options() -> Options {
    Options {
        create_if_missing: false,
        reuse_logs: false,
        reuse_manifest: false,
        ..Options::default()
    }
}

fn open(dir: &Path) -> Result<DB> {
    DB::open(dir, options())
        .map_err(|error| AppError::Other(format!("cannot open {}: {error}", dir.display())))
}

/// Copy `dir` into `parent` under a fresh private name.
fn stage(dir: &Path, parent: &Path) -> Result<tempfile::TempDir> {
    let staged = tempfile::Builder::new()
        .prefix(".leveldb.switchboard-")
        .tempdir_in(parent)
        .map_err(|e| AppError::io_at(parent, e))?;
    super::copy_dir(dir, &staged.path().join("leveldb"))?;
    Ok(staged)
}

/// Every `https://claude.ai` entry, by name. Missing store reads as empty.
pub fn read(leveldb_dir: &Path, scratch: &Path) -> Result<BTreeMap<String, String>> {
    if !leveldb_dir.is_dir() {
        return Ok(BTreeMap::new());
    }
    let staged = stage(leveldb_dir, scratch)?;
    let mut db = open(&staged.path().join("leveldb"))?;
    let mut entries = BTreeMap::new();
    let mut iter = db
        .new_iter()
        .map_err(|error| AppError::Other(format!("cannot iterate localStorage: {error}")))?;
    while iter.advance() {
        let Some((key, value)) = iter.current() else {
            continue;
        };
        if let Some(name) = storage_name(&key)
            && let Some(text) = decode(&value)
        {
            entries.insert(name, text);
        }
    }
    Ok(entries)
}

/// Put `updates` into the live store. Staged, verified by a second open, then
/// swapped in; on any failure the original directory is left as it was.
pub fn write(leveldb_dir: &Path, updates: &BTreeMap<String, String>) -> Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    let parent = leveldb_dir
        .parent()
        .ok_or_else(|| AppError::Other("localStorage store has no parent directory".into()))?;
    let retired = parent.join(".leveldb.switchboard-retired");
    // A crash between the two renames of an earlier write leaves the store
    // retired and nothing in its place. Put it back before anything else.
    if !leveldb_dir.exists() && retired.is_dir() {
        std::fs::rename(&retired, leveldb_dir).map_err(|e| AppError::io_at(&retired, e))?;
    }
    let metadata = std::fs::symlink_metadata(leveldb_dir).map_err(|e| {
        AppError::Other(format!(
            "no localStorage store at {}: {e}",
            leveldb_dir.display()
        ))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(AppError::Other(format!(
            "localStorage store {} is a symlink; not rewriting it",
            leveldb_dir.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(AppError::Other(format!(
            "no localStorage store at {}",
            leveldb_dir.display()
        )));
    }
    let staged = stage(leveldb_dir, parent)?;
    let staged_db = staged.path().join("leveldb");
    {
        let mut db = open(&staged_db)?;
        for (name, text) in updates {
            db.put(&storage_key(name), &encode(text))
                .map_err(|error| AppError::Other(format!("cannot write {name}: {error}")))?;
        }
        db.flush()
            .map_err(|error| AppError::Other(format!("cannot flush localStorage: {error}")))?;
        db.close()
            .map_err(|error| AppError::Other(format!("cannot close localStorage: {error}")))?;
    }
    {
        let mut db = open(&staged_db)?;
        for (name, text) in updates {
            let stored = db.get(&storage_key(name)).and_then(|bytes| decode(&bytes));
            if stored.as_deref() != Some(text.as_str()) {
                return Err(AppError::Other(format!(
                    "localStorage write of {name} did not read back"
                )));
            }
        }
    }
    super::remove_if_present(&retired)?;
    std::fs::rename(leveldb_dir, &retired).map_err(|e| AppError::io_at(leveldb_dir, e))?;
    if let Err(error) = std::fs::rename(&staged_db, leveldb_dir) {
        let restore = std::fs::rename(&retired, leveldb_dir);
        return Err(match restore {
            Ok(()) => AppError::io_at(&staged_db, error),
            Err(rollback) => AppError::Other(format!(
                "could not install the localStorage store: {error}; could not restore the original: {rollback}"
            )),
        });
    }
    // Installed. A retired copy left beside the store is harmless and is
    // cleared by the next write, so its removal must not fail the write.
    let _ = super::remove_if_present(&retired);
    Ok(())
}

/// Path of the store inside a Desktop data (or saved profile state) directory.
pub fn leveldb_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("Local Storage").join("leveldb")
}

/// A store laid out the way Chromium would have written it, for switch tests.
#[cfg(test)]
pub(super) fn seed_for_tests(dir: &Path, entries: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    let options = Options {
        create_if_missing: true,
        ..Options::default()
    };
    let mut db = DB::open(dir, options).unwrap();
    db.put(b"VERSION", b"1").unwrap();
    for (name, text) in entries {
        db.put(&storage_key(name), &encode(text)).unwrap();
    }
    db.flush().unwrap();
    db.close().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a store the way Chromium would have: one Latin-1 and one UTF-16
    /// value under the origin, plus a foreign-origin entry and a meta key.
    fn seed(dir: &Path) {
        let options = Options {
            create_if_missing: true,
            ..Options::default()
        };
        let mut db = DB::open(dir, options).unwrap();
        db.put(b"VERSION", b"1").unwrap();
        db.put(&storage_key("plain"), &encode("hello")).unwrap();
        db.put(&storage_key("wide"), &encode("caf\u{e9} \u{1F600}"))
            .unwrap();
        db.put(b"_https://other.example\x00\x01k", b"\x01v")
            .unwrap();
        db.flush().unwrap();
        db.close().unwrap();
    }

    #[test]
    fn encodings_round_trip() {
        assert_eq!(decode(&encode("plain")).as_deref(), Some("plain"));
        assert_eq!(encode("plain")[0], KEY_LATIN1);
        assert_eq!(encode("\u{1F600}")[0], KEY_UTF16);
        assert_eq!(decode(&encode("\u{1F600}")).as_deref(), Some("\u{1F600}"));
        assert_eq!(decode(&encode("caf\u{e9}")).as_deref(), Some("caf\u{e9}"));
        assert_eq!(
            storage_name(&storage_key("dframe-store")).as_deref(),
            Some("dframe-store")
        );
        assert_eq!(storage_name(b"_https://other\x00\x01k"), None);
        assert_eq!(storage_name(b"META:https://claude.ai"), None);
    }

    #[test]
    fn read_sees_only_the_origin_and_leaves_the_store_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("leveldb");
        seed(&dir);
        let before: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| (e.file_name(), std::fs::read(e.path()).unwrap()))
            .collect();
        let entries = read(&dir, temp.path()).unwrap();
        assert_eq!(entries["plain"], "hello");
        assert_eq!(entries["wide"], "caf\u{e9} \u{1F600}");
        assert_eq!(entries.len(), 2);
        let after: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| (e.file_name(), std::fs::read(e.path()).unwrap()))
            .collect();
        assert_eq!(before, after, "reading must not rewrite the store");
        assert!(
            read(&temp.path().join("absent"), temp.path())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn write_replaces_values_and_reads_back_through_a_fresh_open() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("leveldb");
        seed(&dir);
        let updates = BTreeMap::from([
            ("plain".to_string(), "changed \u{1F600}".to_string()),
            ("new".to_string(), "1".to_string()),
        ]);
        write(&dir, &updates).unwrap();
        let entries = read(&dir, temp.path()).unwrap();
        assert_eq!(entries["plain"], "changed \u{1F600}");
        assert_eq!(entries["new"], "1");
        assert_eq!(entries["wide"], "caf\u{e9} \u{1F600}");
        // No staging leftovers beside the store.
        let leftovers: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n != "leveldb")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn write_to_a_missing_store_fails_without_creating_one() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("leveldb");
        let updates = BTreeMap::from([("k".to_string(), "v".to_string())]);
        assert!(write(&dir, &updates).is_err());
        assert!(!dir.exists());
    }
}
