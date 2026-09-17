//! Reconcile archive intent separately from session transcript activity.
//!
//! Claude writes `isArchived` without advancing `lastActivityAt`. Choosing a
//! whole index by activity alone therefore loses archive/unarchive edits.
//! Per-organisation observations identify actual flag changes; a canonical
//! flag carries an edit through an A -> B -> C switch without letting A's
//! unchanged stale copy resurrect it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::merge::{SessionMerge, Synced, account_org_dirs, local_session_files};
use crate::error::{AppError, Result};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SessionStateMerge {
    /// Apply after the ordinary index copies, before any deletion sweep.
    pub rewrites: Vec<(PathBuf, Vec<u8>)>,
    pub canonical_archives: BTreeMap<String, bool>,
    /// Ambiguous histories conservatively remain archived.
    pub conflicts: usize,
}

struct Candidate {
    account: String,
    org: String,
    path: PathBuf,
    document: Value,
    archived: Option<bool>,
}

/// A missing flag is a valid older active document, but is not an explicit
/// unarchive instruction. A malformed flag is an unsupported schema.
fn archive_flag(document: &Value) -> Option<Option<bool>> {
    document.as_object()?;
    match document.get("isArchived") {
        None => Some(None),
        Some(Value::Bool(value)) => Some(Some(*value)),
        Some(_) => None,
    }
}

fn load_document(path: &Path) -> Option<(Value, Option<bool>)> {
    let document: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let flag = archive_flag(&document)?;
    Some((document, flag))
}

fn scope(dir: &Path) -> Option<(String, String)> {
    Some((
        dir.parent()?.file_name()?.to_str()?.to_string(),
        dir.file_name()?.to_str()?.to_string(),
    ))
}

pub(super) fn record_current(sessions_root: &Path, synced: &mut Synced) {
    for dir in account_org_dirs(sessions_root) {
        let Some((account, org)) = scope(&dir) else {
            continue;
        };
        for path in local_session_files(&dir) {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some((_, flag)) = load_document(&path) else {
                continue;
            };
            synced
                .entry(account.clone())
                .or_default()
                .session_archive_states
                .entry(org.clone())
                .or_default()
                .insert(name.to_string(), flag);
        }
    }
}

/// Require agreement across all rows so an older writer that drops the new
/// fields cannot accidentally leave a stale flag authoritative.
pub fn canonical_archives(synced: &Synced) -> BTreeMap<String, bool> {
    let mut rows = synced.values();
    let Some(first) = rows.next() else {
        return BTreeMap::new();
    };
    let mut canonical = first.canonical_session_archives.clone();
    for row in rows {
        canonical.retain(|id, value| row.canonical_session_archives.get(id) == Some(value));
    }
    canonical
}

/// Also drop deleted indexes so they cannot acquire old intent on recreation.
pub fn set_canonical(synced: &mut Synced, canonical: &BTreeMap<String, bool>) {
    let present: BTreeSet<_> = synced
        .values()
        .flat_map(|row| row.sessions.iter().cloned())
        .collect();
    let mut kept = canonical.clone();
    kept.retain(|id, _| present.contains(id));
    for row in synced.values_mut() {
        row.canonical_session_archives.clone_from(&kept);
    }
}

fn previous_flag(synced: &Synced, candidate: &Candidate, id: &str) -> Option<Option<bool>> {
    synced
        .get(&candidate.account)?
        .session_archive_states
        .get(&candidate.org)?
        .get(id)
        .copied()
}

fn choose_archive(
    synced: &Synced,
    id: &str,
    choices: &[Candidate],
    prior: Option<bool>,
) -> (bool, bool) {
    let changed: BTreeSet<bool> = choices
        .iter()
        .filter_map(|candidate| {
            let old = previous_flag(synced, candidate, id)?;
            // Removing a field is not evidence that the user unarchived a chat.
            let current = candidate.archived?;
            (old.unwrap_or(false) != current).then_some(current)
        })
        .collect();
    let unknown_archive = choices.iter().any(|candidate| {
        previous_flag(synced, candidate, id).is_none() && candidate.archived == Some(true)
    });
    if changed.len() == 1 {
        let selected = *changed.first().expect("one changed flag");
        if selected || !unknown_archive {
            return (selected, false);
        }
        return (true, true);
    }
    if changed.len() > 1 {
        return (true, true);
    }
    if let Some(canonical) = prior {
        return (canonical || unknown_archive, !canonical && unknown_archive);
    }
    let archived = choices
        .iter()
        .any(|candidate| candidate.archived == Some(true));
    let active = choices
        .iter()
        .any(|candidate| candidate.archived != Some(true));
    // The first observation cannot reconstruct historical intent. Preserve the
    // archive when copies disagree; an explicit later unarchive is detected.
    (archived, archived && active)
}

pub fn plan_merge(
    sessions_root: &Path,
    target_account: &str,
    target_org: &str,
    synced: &Synced,
    sessions: &SessionMerge,
) -> Result<SessionStateMerge> {
    let target_dir = sessions_root.join(target_account).join(target_org);
    let mut by_id: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for dir in account_org_dirs(sessions_root) {
        let Some((account, org)) = scope(&dir) else {
            continue;
        };
        for path in local_session_files(&dir) {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some((document, archived)) = load_document(&path) else {
                continue;
            };
            by_id.entry(name.to_string()).or_default().push(Candidate {
                account: account.clone(),
                org: org.clone(),
                path,
                document,
                archived,
            });
        }
    }
    let planned_copies: BTreeMap<_, _> = sessions
        .copied
        .iter()
        .chain(&sessions.updated)
        .map(|(source, destination)| (destination.clone(), source.clone()))
        .collect();
    // Never overwrite a malformed selected source or destination. Such data
    // needs investigation; treating it as empty could silently discard it.
    for (destination, source) in &planned_copies {
        if load_document(source).is_none()
            || (destination.exists() && load_document(destination).is_none())
        {
            return Err(AppError::Other(
                "Claude session archive reconciliation found an unreadable or unsupported index"
                    .into(),
            ));
        }
    }
    let prior = canonical_archives(synced);
    let mut result = SessionStateMerge::default();
    for (id, choices) in by_id {
        let destination = target_dir.join(&id);
        let chosen_path = planned_copies.get(&destination).unwrap_or(&destination);
        let Some(content) = choices
            .iter()
            .find(|candidate| candidate.path == *chosen_path)
        else {
            continue;
        };
        let (archived, conflict) = choose_archive(synced, &id, &choices, prior.get(&id).copied());
        result.conflicts += usize::from(conflict);
        result.canonical_archives.insert(id, archived);
        if content.archived.unwrap_or(false) == archived {
            continue;
        }
        let mut document = content.document.clone();
        document["isArchived"] = Value::Bool(archived);
        result
            .rewrites
            .push((destination, serde_json::to_vec(&document)?));
    }
    Ok(result)
}

/// The app's per-folder load hint: which index ids are archived, so their
/// full records can be loaded lazily. The flags in the indexes stay the source
/// of truth, but a hint that disagrees with rewritten flags defers the wrong
/// chats, so it is regenerated wherever it already exists.
pub const ARCHIVED_HINT: &str = "archived-sessions.idx";

/// `(path, bytes)` to bring `dir`'s hint in line with the flags on disk, or
/// `None` when the folder has no hint or it is already right. Its layout is
/// the app's own: `{"v":1,"archived":[sorted ids]}`.
pub fn archived_hint_rewrite(dir: &Path) -> Option<(PathBuf, Vec<u8>)> {
    let path = dir.join(ARCHIVED_HINT);
    let current = std::fs::read(&path).ok()?;
    let mut archived: Vec<String> = local_session_files(dir)
        .iter()
        .filter(|index| load_document(index).is_some_and(|(_, flag)| flag == Some(true)))
        .filter_map(|index| Some(index.file_stem()?.to_str()?.to_string()))
        .collect();
    archived.sort();
    // Same member order as the app's writer, so an unchanged hint is
    // byte-identical and left alone.
    let bytes = format!(
        r#"{{"v":1,"archived":{}}}"#,
        serde_json::to_string(&archived).ok()?
    )
    .into_bytes();
    (bytes != current).then_some((path, bytes))
}

#[cfg(test)]
mod tests {
    use super::super::merge::{current_state, plan_session_merge};
    use super::*;

    #[test]
    fn the_archived_hint_is_regenerated_only_where_it_exists() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O", Some(true), 10);
        assert_eq!(archived_hint_rewrite(&root.join("A/O")), None);
        std::fs::write(
            root.join("A/O").join(ARCHIVED_HINT),
            r#"{"v":1,"archived":[]}"#,
        )
        .unwrap();
        let (path, bytes) = archived_hint_rewrite(&root.join("A/O")).unwrap();
        assert_eq!(path, root.join("A/O").join(ARCHIVED_HINT));
        assert_eq!(bytes, br#"{"v":1,"archived":["local_chat"]}"#);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(archived_hint_rewrite(&root.join("A/O")), None);
    }

    fn write(root: &Path, scope: &str, flag: Option<bool>, activity: i64) {
        let path = root.join(scope).join("local_chat.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut document = serde_json::json!({
            "lastActivityAt": activity, "cliSessionId": format!("resume-{activity}"),
            "futureField": {"preserve": true}
        });
        if let Some(value) = flag {
            document["isArchived"] = Value::Bool(value);
        }
        std::fs::write(path, serde_json::to_vec(&document).unwrap()).unwrap();
    }

    fn apply(root: &Path, account: &str, org: &str, baseline: &Synced) -> Synced {
        let sessions = plan_session_merge(root, account, org);
        let state = plan_merge(root, account, org, baseline, &sessions).unwrap();
        for (source, destination) in sessions.copied.iter().chain(&sessions.updated) {
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(source, destination).unwrap();
        }
        for (destination, bytes) in &state.rewrites {
            std::fs::write(destination, bytes).unwrap();
        }
        let mut next = current_state(root);
        set_canonical(&mut next, &state.canonical_archives);
        next
    }

    fn document(root: &Path, account: &str) -> Value {
        serde_json::from_slice(&std::fs::read(root.join(account).join("local_chat.json")).unwrap())
            .unwrap()
    }

    #[test]
    fn equal_activity_archive_then_unarchive_propagate_across_three_accounts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for account in ["A/O", "B/O", "C/O"] {
            write(root, account, Some(false), 10);
        }
        let initial = current_state(root);
        write(root, "A/O", Some(true), 10);
        let baseline = apply(root, "B", "O", &initial);
        assert_eq!(document(root, "B/O")["isArchived"], true);
        let baseline = apply(root, "C", "O", &baseline);
        assert_eq!(document(root, "C/O")["isArchived"], true);
        write(root, "C/O", Some(false), 10);
        let baseline = apply(root, "B", "O", &baseline);
        assert_eq!(document(root, "B/O")["isArchived"], false);
        apply(root, "A", "O", &baseline);
        assert_eq!(document(root, "A/O")["isArchived"], false);
    }

    #[test]
    fn archive_intent_preserves_newest_resume_data_and_unknown_fields() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O", Some(false), 10);
        write(root, "B/O", Some(false), 20);
        let baseline = current_state(root);
        write(root, "A/O", Some(true), 10);
        apply(root, "B", "O", &baseline);
        let result = document(root, "B/O");
        assert_eq!(result["isArchived"], true);
        assert_eq!(result["cliSessionId"], "resume-20");
        assert_eq!(result["futureField"]["preserve"], true);
    }

    #[test]
    fn first_observation_keeps_archives_without_claiming_historical_intent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O", Some(true), 10);
        write(root, "B/O", Some(false), 100);
        let sessions = plan_session_merge(root, "B", "O");
        let state = plan_merge(root, "B", "O", &Synced::new(), &sessions).unwrap();
        assert_eq!(state.conflicts, 1);
        assert!(state.canonical_archives["local_chat.json"]);
        apply(root, "B", "O", &Synced::new());
        assert_eq!(document(root, "B/O")["isArchived"], true);
    }

    #[test]
    fn field_removal_is_not_an_explicit_unarchive() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O", Some(true), 10);
        write(root, "B/O", Some(true), 10);
        let mut baseline = current_state(root);
        set_canonical(
            &mut baseline,
            &BTreeMap::from([("local_chat.json".into(), true)]),
        );
        write(root, "A/O", None, 20);
        apply(root, "B", "O", &baseline);
        assert_eq!(document(root, "B/O")["isArchived"], true);
        assert_eq!(document(root, "B/O")["lastActivityAt"], 20);
    }

    #[test]
    fn observations_are_scoped_to_organisation_and_survive_serialization() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O1", Some(true), 10);
        write(root, "A/O2", None, 10);
        let baseline = current_state(root);
        let bytes = serde_json::to_vec(&baseline).unwrap();
        let baseline = super::super::merge::parse_synced(&bytes);
        assert_eq!(
            baseline["A"].session_archive_states["O1"]["local_chat.json"],
            Some(true)
        );
        assert_eq!(
            baseline["A"].session_archive_states["O2"]["local_chat.json"],
            None
        );
        write(root, "A/O1", Some(false), 10);
        apply(root, "A", "O2", &baseline);
        assert_ne!(document(root, "A/O2")["isArchived"], true);
    }

    #[test]
    fn malformed_selected_source_or_destination_aborts_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "A/O", Some(true), 10);
        let target = root.join("B/O/local_chat.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "not json").unwrap();
        let sessions = plan_session_merge(root, "B", "O");
        assert!(plan_merge(root, "B", "O", &Synced::new(), &sessions).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"not json");
        std::fs::remove_file(&target).unwrap();
        std::fs::write(root.join("A/O/local_chat.json"), "{\"isArchived\":\"yes\"}").unwrap();
        let sessions = plan_session_merge(root, "B", "O");
        assert!(plan_merge(root, "B", "O", &Synced::new(), &sessions).is_err());
        assert!(!target.exists());
    }
}
