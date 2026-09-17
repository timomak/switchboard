//! Claude Desktop's own record of a deleted chat.
//!
//! When the user deletes a session, the app's main process writes an empty-ish
//! `deleted_<id>` file (its content is the deletion time in ms) beside the
//! session indexes of the account/org folder, for the session's local id and
//! for every CLI transcript id it owned. Its import scan reads those markers so
//! a deleted transcript under `~/.claude/projects/` is not adopted again.
//!
//! The history merge copies indexes between accounts, which would hand a
//! deleted chat straight back — and, because the app only reads the markers of
//! the signed-in account, another account's import scan could re-adopt it too.
//! A marker is the user's recorded intent, so the merge honours it everywhere
//! without asking: the index named by a local-id marker leaves every account,
//! and every marker is copied into every account/org folder. A transcript-id
//! marker on its own only blocks re-import, as it does in the app; it never
//! removes a differently named index that happens to share the transcript.
//! Transcripts are never touched.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::merge::{account_org_dirs, local_session_files};

pub const PREFIX: &str = "deleted_";
const INDEX_PREFIX: &str = "local_";
const INDEX_SUFFIX: &str = ".json";
/// Index files larger than this are not parsed for transcript ids — the app's
/// own scan uses the same bound.
const MAX_INDEX_BYTES: u64 = 10 * 1024 * 1024;

/// One marker id, seen in one or more folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tombstone {
    /// Milliseconds since the epoch when the app wrote it; `None` when the file
    /// does not hold a number (an older or foreign writer).
    pub deleted_at: Option<i64>,
    /// The file's bytes, copied verbatim when the marker is propagated. The
    /// app reads only the file name.
    pub content: Vec<u8>,
    pub dirs: Vec<PathBuf>,
}

/// Every marker across every account/org folder, keyed by id.
pub fn scan(sessions_root: &Path) -> BTreeMap<String, Tombstone> {
    let mut out: BTreeMap<String, Tombstone> = BTreeMap::new();
    for dir in account_org_dirs(sessions_root) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|name| name.strip_prefix(PREFIX)) else {
                continue;
            };
            if id.is_empty() || !entry.path().is_file() {
                continue;
            }
            let content = std::fs::read(entry.path()).unwrap_or_default();
            let deleted_at = std::str::from_utf8(&content)
                .ok()
                .and_then(|text| text.trim().parse::<i64>().ok());
            let tombstone = out.entry(id.to_string()).or_insert(Tombstone {
                deleted_at,
                content,
                dirs: Vec::new(),
            });
            // The earliest known time is the deletion; a copy made later
            // carries the same value anyway.
            tombstone.deleted_at = match (tombstone.deleted_at, deleted_at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            tombstone.dirs.push(dir.clone());
        }
    }
    out
}

/// What the markers imply for the indexes on disk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Index filenames (`local_<id>.json`) that are deleted and must not be
    /// copied or kept anywhere.
    pub doomed: BTreeSet<String>,
    /// Marker ids that an index newer than the deletion has superseded — the
    /// chat was re-imported after it was deleted. Those markers are left alone
    /// and not propagated.
    pub superseded: BTreeSet<String>,
}

/// A chat is deleted when its local id has a marker — unless the index was
/// created after the deletion, which is the app re-importing the transcript on
/// purpose. The app writes a marker for the local id of every deletion, so
/// this catches each one; transcript-id markers are not matched against other
/// indexes, since the app itself uses them only to refuse a re-import.
pub fn verdict(sessions_root: &Path, tombstones: &BTreeMap<String, Tombstone>) -> Verdict {
    let mut out = Verdict::default();
    if tombstones.is_empty() {
        return out;
    }
    // Newest creation time per filename across every copy. Only `createdAt`
    // counts: a re-import makes a new record, whereas other stamps can move on
    // a copy that was merely re-read.
    let mut created: BTreeMap<String, i64> = BTreeMap::new();
    for dir in account_org_dirs(sessions_root) {
        for path in local_session_files(&dir) {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(local_id) = name
                .strip_prefix(INDEX_PREFIX)
                .and_then(|rest| rest.strip_suffix(INDEX_SUFFIX))
            else {
                continue;
            };
            if !tombstones.contains_key(local_id) {
                continue;
            }
            let stamp = read_index(&path)
                .and_then(|document| document.get("createdAt")?.as_i64())
                .unwrap_or(0);
            let entry = created.entry(name.to_string()).or_insert(stamp);
            *entry = (*entry).max(stamp);
        }
    }
    for (name, born) in created {
        let local_id = name
            .strip_prefix(INDEX_PREFIX)
            .and_then(|rest| rest.strip_suffix(INDEX_SUFFIX))
            .unwrap_or(&name);
        let tombstone = &tombstones[local_id];
        if tombstone.deleted_at.is_some_and(|at| born > at) {
            out.superseded.insert(local_id.to_string());
        } else {
            out.doomed.insert(name.clone());
        }
    }
    out
}

/// Index removals and marker copies that make a deletion stick everywhere.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sweep {
    pub removals: Vec<PathBuf>,
    /// `(path, content)` for markers missing from a folder.
    pub writes: Vec<(PathBuf, Vec<u8>)>,
}

impl Sweep {
    pub fn is_empty(&self) -> bool {
        self.removals.is_empty() && self.writes.is_empty()
    }

    /// Distinct chats leaving, regardless of how many accounts hold a copy.
    pub fn chats(&self) -> usize {
        self.removals
            .iter()
            .filter_map(|path| path.file_name())
            .collect::<BTreeSet<_>>()
            .len()
    }
}

/// Nothing is touched here; the caller applies the sweep after the history
/// merge so it also strips anything the merge just re-added.
pub fn plan_sweep(
    sessions_root: &Path,
    tombstones: &BTreeMap<String, Tombstone>,
    verdict: &Verdict,
) -> Sweep {
    let mut sweep = Sweep::default();
    let dirs = account_org_dirs(sessions_root);
    for dir in &dirs {
        for path in local_session_files(dir) {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| verdict.doomed.contains(name))
            {
                sweep.removals.push(path);
            }
        }
    }
    for (id, tombstone) in tombstones {
        if verdict.superseded.contains(id) {
            continue;
        }
        for dir in &dirs {
            if tombstone.dirs.contains(dir) {
                continue;
            }
            sweep
                .writes
                .push((dir.join(format!("{PREFIX}{id}")), tombstone.content.clone()));
        }
    }
    sweep
}

fn read_index(path: &Path) -> Option<Value> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_INDEX_BYTES {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn index(created: i64, cli: &str) -> String {
        format!(
            r#"{{"sessionId":"x","createdAt":{created},"lastActivityAt":{created},"cliSessionId":"{cli}"}}"#
        )
    }

    #[test]
    fn a_marker_in_one_account_dooms_the_index_everywhere() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O/local_one.json", &index(100, "cli-one"));
        write(root, "B/O/local_one.json", &index(100, "cli-one"));
        write(root, "B/O/local_two.json", &index(100, "cli-two"));
        write(root, "A/O/deleted_one", "500");
        write(root, "A/O/deleted_cli-one", "500");

        let tombstones = scan(root);
        assert_eq!(tombstones["one"].deleted_at, Some(500));
        let verdict = verdict(root, &tombstones);
        assert_eq!(
            verdict.doomed,
            BTreeSet::from(["local_one.json".to_string()])
        );
        assert!(verdict.superseded.is_empty());

        let sweep = plan_sweep(root, &tombstones, &verdict);
        assert_eq!(
            sweep.removals,
            vec![
                root.join("A/O/local_one.json"),
                root.join("B/O/local_one.json")
            ]
        );
        // Both markers reach B, neither is rewritten in A.
        assert_eq!(
            sweep.writes,
            vec![
                (root.join("B/O/deleted_cli-one"), b"500".to_vec()),
                (root.join("B/O/deleted_one"), b"500".to_vec()),
            ]
        );
    }

    /// The app's own rule: a transcript-id marker refuses a re-import, it does
    /// not delete a different session that shares the transcript.
    #[test]
    fn a_transcript_id_marker_alone_only_propagates() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O/local_a.json", &index(100, "shared-cli"));
        write(root, "B/O/local_b.json", &index(100, "shared-cli"));
        write(root, "A/O/deleted_shared-cli", "900");
        let tombstones = scan(root);
        let verdict = verdict(root, &tombstones);
        assert_eq!(verdict, Verdict::default());
        let sweep = plan_sweep(root, &tombstones, &verdict);
        assert!(sweep.removals.is_empty());
        assert_eq!(
            sweep.writes,
            vec![(root.join("B/O/deleted_shared-cli"), b"900".to_vec())]
        );
    }

    /// A copy re-read elsewhere after the deletion is still the deleted chat;
    /// only a newly created record supersedes the marker.
    #[test]
    fn a_bumped_index_stamp_does_not_supersede_the_marker() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O/local_one.json", &index(100, "cli-one"));
        write(
            root,
            "B/O/local_one.json",
            r#"{"sessionId":"x","createdAt":100,"indexedAt":900,"lastActivityAt":900}"#,
        );
        write(root, "A/O/deleted_one", "500");
        let tombstones = scan(root);
        let verdict = verdict(root, &tombstones);
        assert_eq!(
            verdict.doomed,
            BTreeSet::from(["local_one.json".to_string()])
        );
    }

    #[test]
    fn an_index_created_after_the_deletion_supersedes_the_marker() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O/local_one.json", &index(1000, "cli-one"));
        write(root, "A/O/deleted_one", "500");
        write(root, "B/O/other.txt", "");
        let tombstones = scan(root);
        let verdict = verdict(root, &tombstones);
        assert!(verdict.doomed.is_empty());
        assert_eq!(verdict.superseded, BTreeSet::from(["one".to_string()]));
        let sweep = plan_sweep(root, &tombstones, &verdict);
        assert!(sweep.is_empty(), "{sweep:?}");
    }

    #[test]
    fn an_unreadable_marker_time_still_counts_as_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O/local_one.json", &index(1000, "cli-one"));
        write(root, "B/O/deleted_one", "");
        let tombstones = scan(root);
        assert_eq!(tombstones["one"].deleted_at, None);
        let verdict = verdict(root, &tombstones);
        assert_eq!(
            verdict.doomed,
            BTreeSet::from(["local_one.json".to_string()])
        );
    }

    #[test]
    fn no_markers_means_nothing_is_doomed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O/local_one.json", &index(1, "cli-one"));
        let tombstones = scan(root);
        assert!(tombstones.is_empty());
        assert_eq!(verdict(root, &tombstones), Verdict::default());
    }
}
