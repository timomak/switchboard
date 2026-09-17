//! Carry the Code sidebar's groups ("projects") and view mode across accounts.
//!
//! The renderer persists its sidebar as the `dframe-store` localStorage entry.
//! Custom groups live in `customGroupsByScope["<account>/<org>"]` as
//! `{groups, assignments, order}`; the grouping/sorting mode lives in
//! top-level `groupByByMode` / `sortByByMode`. Both are keyed to the account,
//! so switching restores whatever that account last had: the incoming account
//! can open in a different grouping mode with stale empty groups while every
//! merged chat sits under "Ungrouped".
//!
//! This module reconciles one canonical sidebar across accounts with the same
//! baseline pattern as routines: the outgoing account's edits since it was last
//! observed are folded into the canonical state, which is then written into the
//! incoming account's scope. Groups persist until the user deletes one.
//!
//! Group definitions and the view mode are also synced by the app to its
//! server per organisation, and a pull replaces local groups when the two
//! differ. The write therefore also sets the app's own "pending local edit"
//! marker for the store, so the first reconcile after launch pushes the merged
//! state instead of pulling the stale one over it. Assignments of local chats
//! to groups are local-only and are never uploaded.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::merge::Synced;
use super::{Paths, ProfileMeta, local_storage};
use crate::error::{AppError, Result};

/// localStorage entry holding the persisted zustand store.
pub const STORE_KEY: &str = "dframe-store";
/// The app's marker that the store holds a local edit not yet on the server.
/// Its value is the identity the edit belongs to, `<account>/<org>`.
pub const PENDING_KEY: &str = "ccd-sync-pending:ccd/dframe-store";
/// Main-process preference mirroring the group scopes, merged into the store on
/// hydration. Kept consistent so it cannot resurrect a removed assignment.
pub const MIRROR_PREF: &str = "dframe-group-scopes";
/// Top-level store fields describing how the sidebar is viewed. Server-synced
/// per account by the app, hence carried and pushed like the groups.
pub const VIEW_FIELDS: [&str; 5] = [
    "groupByByMode",
    "sortByByMode",
    "recentsTypeFilter",
    "recentsStatusFilter",
    "routinesSidebarPlacement",
];

/// One account/org scope's groups plus the view fields, as observed or as
/// reconciled. Group objects keep whatever fields the app wrote (`id`, `name`,
/// `icon`, `color`, …); only `id` and `name` are relied on.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarState {
    #[serde(default)]
    pub groups: Vec<Value>,
    #[serde(default)]
    pub assignments: BTreeMap<String, String>,
    #[serde(default)]
    pub order: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub view: BTreeMap<String, Value>,
}

impl SidebarState {
    fn group_ids(&self) -> BTreeSet<String> {
        self.groups.iter().filter_map(group_id).collect()
    }

    fn group(&self, id: &str) -> Option<&Value> {
        self.groups
            .iter()
            .find(|group| group_id(group).as_deref() == Some(id))
    }

    fn is_empty_scope(&self) -> bool {
        self.groups.is_empty() && self.assignments.is_empty() && self.order.is_empty()
    }

    /// Drop assignments and order entries that point at no group, as the app
    /// does when it loads a scope.
    fn prune(&mut self) {
        let ids = self.group_ids();
        self.assignments.retain(|_, group| ids.contains(group));
        self.order
            .retain(|group, keys| ids.contains(group) && !keys.is_empty());
    }
}

fn group_id(group: &Value) -> Option<String> {
    let object = group.as_object()?;
    let id = object.get("id")?.as_str()?;
    object.get("name")?.as_str()?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Parse a scope object from the store. Malformed members are skipped the way
/// the app skips them; a non-object scope reads as empty.
fn parse_scope(value: Option<&Value>) -> SidebarState {
    let mut state = SidebarState::default();
    let Some(object) = value.and_then(Value::as_object) else {
        return state;
    };
    if let Some(groups) = object.get("groups").and_then(Value::as_array) {
        let mut seen = BTreeSet::new();
        for group in groups {
            if let Some(id) = group_id(group)
                && seen.insert(id)
            {
                state.groups.push(group.clone());
            }
        }
    }
    if let Some(assignments) = object.get("assignments").and_then(Value::as_object) {
        for (key, group) in assignments {
            if let Some(group) = group.as_str() {
                state.assignments.insert(key.clone(), group.to_string());
            }
        }
    }
    if let Some(order) = object.get("order").and_then(Value::as_object) {
        for (group, keys) in order {
            if let Some(keys) = keys.as_array() {
                let keys: Vec<String> = keys
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                if !keys.is_empty() {
                    state.order.insert(group.clone(), keys);
                }
            }
        }
    }
    state
}

fn scope_value(state: &SidebarState) -> Value {
    serde_json::json!({
        "groups": state.groups,
        "assignments": state.assignments,
        "order": state.order,
    })
}

/// The persisted store, split into the part this module owns and the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct Store {
    document: Value,
}

impl Store {
    pub fn parse(text: &str) -> Result<Self> {
        let document: Value = serde_json::from_str(text)?;
        let version = document.get("version").and_then(Value::as_i64);
        if version != Some(1) || !document.get("state").is_some_and(Value::is_object) {
            return Err(AppError::Other(format!(
                "unsupported {STORE_KEY} layout (version {version:?}); sidebar state not carried"
            )));
        }
        Ok(Self { document })
    }

    fn state(&self) -> &Map<String, Value> {
        self.document["state"]
            .as_object()
            .expect("validated at parse")
    }

    fn state_mut(&mut self) -> &mut Map<String, Value> {
        self.document["state"]
            .as_object_mut()
            .expect("validated at parse")
    }

    /// The scope the sidebar last rendered, when it belongs to `account`.
    pub fn last_scope_for(&self, account: &str) -> Option<String> {
        let key = self.state().get("lastSidebarScopeKey")?.as_str()?;
        let (owner, org) = key.split_once('/')?;
        (owner == account && !org.is_empty() && !org.contains('/')).then(|| key.to_string())
    }

    pub fn scope(&self, key: &str) -> SidebarState {
        let mut state = parse_scope(
            self.state()
                .get("customGroupsByScope")
                .and_then(|scopes| scopes.get(key)),
        );
        for field in VIEW_FIELDS {
            if let Some(value) = self.state().get(field) {
                state.view.insert(field.to_string(), value.clone());
            }
        }
        state
    }

    pub fn has_scope(&self, key: &str) -> bool {
        self.state()
            .get("customGroupsByScope")
            .and_then(Value::as_object)
            .is_some_and(|scopes| scopes.contains_key(key))
    }

    pub fn set_scope(&mut self, key: &str, state: &SidebarState) {
        let scopes = self
            .state_mut()
            .entry("customGroupsByScope")
            .or_insert_with(|| Value::Object(Map::new()));
        if !scopes.is_object() {
            *scopes = Value::Object(Map::new());
        }
        scopes[key] = scope_value(state);
        for field in VIEW_FIELDS {
            match state.view.get(field) {
                Some(value) => {
                    self.state_mut().insert(field.to_string(), value.clone());
                }
                None => {
                    self.state_mut().remove(field);
                }
            }
        }
    }

    pub fn to_text(&self) -> Result<String> {
        Ok(serde_json::to_string(&self.document)?)
    }
}

/// Fold what one account shows now into the canonical state.
///
/// With a baseline, only what changed since then counts: added, renamed and
/// deleted groups, moved chats, reordered members, a switched view mode. An
/// empty scope where the baseline had groups is not trusted as a deletion of
/// everything: the app's server merge produces exactly that when it replaces
/// local groups, and deleting the whole set at once is rare. Without a baseline
/// the two are unioned; `observed_wins` says which side keeps a conflicting
/// name, assignment or view value.
fn fold(
    canonical: &mut SidebarState,
    observed: &SidebarState,
    baseline: Option<&SidebarState>,
    observed_wins: bool,
) {
    let Some(baseline) = baseline else {
        for group in &observed.groups {
            let Some(id) = group_id(group) else { continue };
            match canonical
                .groups
                .iter_mut()
                .find(|existing| group_id(existing).as_deref() == Some(id.as_str()))
            {
                Some(existing) if observed_wins => *existing = group.clone(),
                Some(_) => {}
                None => canonical.groups.push(group.clone()),
            }
        }
        for (key, group) in &observed.assignments {
            if observed_wins || !canonical.assignments.contains_key(key) {
                canonical.assignments.insert(key.clone(), group.clone());
            }
        }
        for (group, keys) in &observed.order {
            let merged = canonical.order.entry(group.clone()).or_default();
            if observed_wins {
                let mut combined = keys.clone();
                combined.extend(merged.iter().filter(|k| !keys.contains(k)).cloned());
                *merged = combined;
            } else {
                let missing: Vec<String> = keys
                    .iter()
                    .filter(|k| !merged.contains(k))
                    .cloned()
                    .collect();
                merged.extend(missing);
            }
        }
        for (field, value) in &observed.view {
            if observed_wins || !canonical.view.contains_key(field) {
                canonical.view.insert(field.clone(), value.clone());
            }
        }
        canonical.prune();
        return;
    };

    let wiped = observed.is_empty_scope() && !baseline.groups.is_empty();
    let observed_ids = observed.group_ids();
    for group in &observed.groups {
        let Some(id) = group_id(group) else { continue };
        if baseline.group(&id) == Some(group) {
            continue; // unchanged since last observed
        }
        match canonical
            .groups
            .iter_mut()
            .find(|existing| group_id(existing).as_deref() == Some(id.as_str()))
        {
            Some(existing) => *existing = group.clone(),
            None => canonical.groups.push(group.clone()),
        }
    }
    if !wiped {
        canonical.groups.retain(|group| {
            group_id(group)
                .is_none_or(|id| observed_ids.contains(&id) || baseline.group(&id).is_none())
        });
    }
    let keys: BTreeSet<&String> = observed
        .assignments
        .keys()
        .chain(baseline.assignments.keys())
        .collect();
    for key in keys {
        let now = observed.assignments.get(key);
        if now == baseline.assignments.get(key) {
            continue;
        }
        match now {
            Some(group) => {
                canonical.assignments.insert(key.clone(), group.clone());
            }
            None if wiped => {}
            None => {
                canonical.assignments.remove(key);
            }
        }
    }
    let groups: BTreeSet<&String> = observed.order.keys().chain(baseline.order.keys()).collect();
    for group in groups {
        let now = observed.order.get(group);
        if now == baseline.order.get(group) {
            continue;
        }
        match now {
            Some(keys) => {
                canonical.order.insert(group.clone(), keys.clone());
            }
            None if wiped => {}
            None => {
                canonical.order.remove(group);
            }
        }
    }
    for field in VIEW_FIELDS {
        let now = observed.view.get(field);
        if now == baseline.view.get(field) {
            continue;
        }
        match now {
            Some(value) => {
                canonical.view.insert(field.to_string(), value.clone());
            }
            None => {
                canonical.view.remove(field);
            }
        }
    }
    canonical.prune();
}

/// Everything the switch will write, decided before anything is touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarPlan {
    pub target_account: String,
    pub target_scope: String,
    /// The reconciled sidebar to install for the target.
    pub state: SidebarState,
    /// What the target's saved store holds now, for the sync record.
    pub target_observed: SidebarState,
    /// The outgoing account, its scope, and what it shows now.
    pub outgoing: Option<(String, String, SidebarState)>,
    /// Whether the target's store needs a write at all.
    pub changed: bool,
}

impl SidebarPlan {
    pub fn groups(&self) -> usize {
        self.state.groups.len()
    }

    pub fn assignments(&self) -> usize {
        self.state.assignments.len()
    }

    pub fn group_by(&self) -> Option<String> {
        self.state
            .view
            .get("groupByByMode")
            .and_then(|modes| modes.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// Accept the canonical only when every recorded row agrees, like the other
/// canonical fields.
pub fn canonical(synced: &Synced) -> Option<SidebarState> {
    let mut rows = synced.values();
    let first = rows.next()?.canonical_sidebar.clone()?;
    rows.all(|row| row.canonical_sidebar.as_ref() == Some(&first))
        .then_some(first)
}

fn baseline<'a>(synced: &'a Synced, account: &str, scope: &str) -> Option<&'a SidebarState> {
    synced.get(account)?.sidebar_scopes.get(scope)
}

/// `merge::current_state` rebuilds the record from the session folders, which
/// hold nothing about the sidebar. Copy every scope's last observation and the
/// canonical forward, or an account not part of this switch loses its
/// baseline and its unchanged stale copy would later read as fresh edits.
pub fn carry_baselines(prior: &Synced, next: &mut Synced) {
    for (account, row) in prior {
        if row.sidebar_scopes.is_empty() {
            continue;
        }
        next.entry(account.clone())
            .or_default()
            .sidebar_scopes
            .extend(row.sidebar_scopes.clone());
    }
    if let Some(canonical) = canonical(prior) {
        for row in next.values_mut() {
            row.canonical_sidebar = Some(canonical.clone());
        }
    }
}

/// Read a saved profile's or the live data directory's store.
fn read_store(state_dir: &Path, scratch: &Path) -> Result<Option<Store>> {
    let entries = local_storage::read(&local_storage::leveldb_dir(state_dir), scratch)?;
    entries
        .get(STORE_KEY)
        .map(|text| Store::parse(text))
        .transpose()
}

/// Decide the sidebar for a switch to `target` away from `outgoing`.
///
/// `None` when the target has no saved store or no known org: there is nowhere
/// to carry the state into, and the switch proceeds as before.
pub fn plan(
    paths: &Paths,
    target: &ProfileMeta,
    outgoing: Option<&ProfileMeta>,
    synced: &Synced,
) -> Result<Option<SidebarPlan>> {
    let Some(target_org) = target.org_uuid.as_deref() else {
        return Ok(None);
    };
    let scratch = scratch_dir(paths);
    let target_state_dir = paths.profile_dir(&target.label).join(super::DESKTOP_STATE);
    let Some(target_store) = read_store(&target_state_dir, &scratch)? else {
        return Ok(None);
    };
    let target_scope = format!("{}/{target_org}", target.account_uuid);
    let target_observed = target_store.scope(&target_scope);

    let outgoing = match outgoing {
        Some(profile) => match read_store(&paths.data_dir, &scratch)? {
            Some(store) => {
                let scope = store.last_scope_for(&profile.account_uuid).or_else(|| {
                    profile
                        .org_uuid
                        .as_ref()
                        .map(|org| format!("{}/{org}", profile.account_uuid))
                });
                scope.map(|scope| {
                    let observed = store.scope(&scope);
                    (profile.account_uuid.clone(), scope, observed)
                })
            }
            None => None,
        },
        None => None,
    };

    let mut state = canonical(synced).unwrap_or_default();
    let seeded = canonical(synced).is_some();
    // The target first, so an outgoing account without a baseline wins the
    // conflicts of a first union; with baselines the order is irrelevant.
    fold(
        &mut state,
        &target_observed,
        baseline(synced, &target.account_uuid, &target_scope),
        !seeded,
    );
    if let Some((account, scope, observed)) = &outgoing {
        fold(&mut state, observed, baseline(synced, account, scope), true);
    }
    let changed = state != target_observed || !target_store.has_scope(&target_scope);
    Ok(Some(SidebarPlan {
        target_account: target.account_uuid.clone(),
        target_scope,
        state,
        target_observed,
        outgoing,
        changed,
    }))
}

/// Install the plan into the live data directory, after the target's browser
/// state has been restored there. Returns whether a write happened.
pub fn apply(paths: &Paths, plan: &SidebarPlan, notes: &mut Vec<String>) -> Result<bool> {
    if !plan.changed {
        return Ok(false);
    }
    let scratch = scratch_dir(paths);
    let leveldb = local_storage::leveldb_dir(&paths.data_dir);
    let entries = local_storage::read(&leveldb, &scratch)?;
    let Some(text) = entries.get(STORE_KEY) else {
        return Err(AppError::Other(format!(
            "the restored browser state has no {STORE_KEY}; sidebar state not carried"
        )));
    };
    let mut store = Store::parse(text)?;
    store.set_scope(&plan.target_scope, &plan.state);
    let updates = BTreeMap::from([
        (STORE_KEY.to_string(), store.to_text()?),
        (PENDING_KEY.to_string(), plan.target_scope.clone()),
    ]);
    local_storage::write(&leveldb, &updates)?;
    if let Err(error) = update_mirror(&paths.data_dir, &plan.target_scope, &plan.state) {
        notes.push(format!(
            "could not update the {MIRROR_PREF} preference: {error}"
        ));
    }
    Ok(true)
}

/// Keep the main-process mirror of the scope in step with what was written.
fn update_mirror(data_dir: &Path, scope: &str, state: &SidebarState) -> Result<()> {
    let path = data_dir.join("claude_desktop_config.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(()); // nothing to keep consistent
    };
    let mut document: Value = serde_json::from_slice(&bytes)?;
    let Some(prefs) = document
        .get_mut("preferences")
        .and_then(|p| p.get_mut("epitaxyPrefs"))
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    let mirror = prefs
        .entry(MIRROR_PREF)
        .or_insert_with(|| Value::Object(Map::new()));
    if !mirror.is_object() {
        *mirror = Value::Object(Map::new());
    }
    if state.is_empty_scope() {
        mirror.as_object_mut().expect("object").remove(scope);
    } else {
        mirror[scope] = scope_value(state);
    }
    crate::cache::atomic_write(&path, serde_json::to_string_pretty(&document)?.as_bytes())
}

/// Record the observations and the reconciled state, so the next switch can
/// tell an edit from a stale copy. `installed` says whether the target now
/// holds `plan.state`; otherwise its observed store is what it still holds.
pub fn record(synced: &mut Synced, plan: &SidebarPlan, installed: bool) {
    if let Some((account, scope, observed)) = &plan.outgoing {
        synced
            .entry(account.clone())
            .or_default()
            .sidebar_scopes
            .insert(scope.clone(), observed.clone());
    }
    let target_now = if installed {
        &plan.state
    } else {
        &plan.target_observed
    };
    synced
        .entry(plan.target_account.clone())
        .or_default()
        .sidebar_scopes
        .insert(plan.target_scope.clone(), target_now.clone());
    for row in synced.values_mut() {
        row.canonical_sidebar = Some(plan.state.clone());
    }
}

/// Where the private store copies are staged: beside the profile store, which
/// exists whenever there is a profile to switch to, and never inside the app's
/// data directory. Planning must not create anything, so no directory is made.
fn scratch_dir(paths: &Paths) -> PathBuf {
    paths
        .profiles_dir
        .parent()
        .map_or_else(|| paths.profiles_dir.clone(), Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: &str, name: &str) -> Value {
        serde_json::json!({"id": id, "name": name})
    }

    fn state(groups: &[(&str, &str)], assignments: &[(&str, &str)]) -> SidebarState {
        SidebarState {
            groups: groups.iter().map(|(id, name)| group(id, name)).collect(),
            assignments: assignments
                .iter()
                .map(|(k, g)| (k.to_string(), g.to_string()))
                .collect(),
            order: BTreeMap::new(),
            view: BTreeMap::new(),
        }
    }

    fn with_view(mut state: SidebarState, mode: &str) -> SidebarState {
        state
            .view
            .insert("groupByByMode".into(), serde_json::json!({"code": mode}));
        state
    }

    #[test]
    fn first_union_prefers_the_outgoing_side() {
        let mut canonical = SidebarState::default();
        let target = with_view(
            state(
                &[("g1", "Old name"), ("g2", "Target only")],
                &[("code:local_a", "g1")],
            ),
            "custom",
        );
        let outgoing = with_view(
            state(
                &[("g1", "New name"), ("g3", "Outgoing only")],
                &[("code:local_a", "g3")],
            ),
            "project",
        );
        fold(&mut canonical, &target, None, false);
        fold(&mut canonical, &outgoing, None, true);
        assert_eq!(canonical.group_ids().len(), 3);
        assert_eq!(canonical.group("g1").unwrap()["name"], "New name");
        assert_eq!(canonical.assignments["code:local_a"], "g3");
        assert_eq!(canonical.view["groupByByMode"]["code"], "project");
    }

    #[test]
    fn edits_since_the_baseline_propagate_and_unchanged_stale_copies_do_not() {
        let base = state(&[("g1", "One"), ("g2", "Two")], &[("code:local_a", "g1")]);
        let mut canonical = base.clone();
        // Outgoing renamed g1, deleted g2, moved chat a, added g3.
        let observed = state(
            &[("g1", "One renamed"), ("g3", "Three")],
            &[("code:local_a", "g3")],
        );
        fold(&mut canonical, &observed, Some(&base), true);
        assert_eq!(canonical.group("g1").unwrap()["name"], "One renamed");
        assert!(canonical.group("g2").is_none());
        assert!(canonical.group("g3").is_some());
        assert_eq!(canonical.assignments["code:local_a"], "g3");
        // A stale copy identical to its baseline changes nothing, even though
        // it still holds g2.
        let mut again = canonical.clone();
        fold(&mut again, &base, Some(&base), true);
        assert_eq!(again, canonical);
    }

    #[test]
    fn a_full_wipe_is_not_a_deletion_of_every_group() {
        let base = state(&[("g1", "One"), ("g2", "Two")], &[("code:local_a", "g1")]);
        let mut canonical = base.clone();
        fold(&mut canonical, &SidebarState::default(), Some(&base), true);
        assert_eq!(canonical, base);
    }

    #[test]
    fn removing_a_chat_from_a_group_propagates() {
        let base = state(
            &[("g1", "One")],
            &[("code:local_a", "g1"), ("code:local_b", "g1")],
        );
        let mut canonical = base.clone();
        let observed = state(&[("g1", "One")], &[("code:local_b", "g1")]);
        fold(&mut canonical, &observed, Some(&base), true);
        assert_eq!(canonical.assignments.len(), 1);
        assert!(!canonical.assignments.contains_key("code:local_a"));
    }

    #[test]
    fn a_view_mode_change_propagates_and_an_unchanged_one_keeps_the_canonical() {
        let base = with_view(state(&[], &[]), "custom");
        let mut canonical = with_view(state(&[], &[]), "project");
        fold(&mut canonical, &base, Some(&base), true);
        assert_eq!(canonical.view["groupByByMode"]["code"], "project");
        let observed = with_view(state(&[], &[]), "date");
        fold(&mut canonical, &observed, Some(&base), true);
        assert_eq!(canonical.view["groupByByMode"]["code"], "date");
    }

    #[test]
    fn store_round_trips_scope_and_view_fields() {
        let text = r#"{"state":{"collapsed":false,"groupByByMode":{"code":"custom"},"customGroupsByScope":{"acct/org":{"groups":[{"id":"g1","name":"One","icon":"x"},{"id":"","name":"bad"},{"id":"g2"}],"assignments":{"code:local_a":"g1","bad":1},"order":{"g1":["code:local_a"],"g9":[]}}},"lastSidebarScopeKey":"acct/org","other":1},"version":1}"#;
        let mut store = Store::parse(text).unwrap();
        assert_eq!(store.last_scope_for("acct").as_deref(), Some("acct/org"));
        assert_eq!(store.last_scope_for("else"), None);
        let scope = store.scope("acct/org");
        assert_eq!(scope.groups.len(), 1);
        assert_eq!(scope.groups[0]["icon"], "x");
        assert_eq!(scope.assignments.len(), 1);
        assert_eq!(scope.order.len(), 1);
        assert_eq!(scope.view["groupByByMode"]["code"], "custom");
        assert!(store.scope("acct/none").is_empty_scope());

        let mut next = scope.clone();
        next.view.insert(
            "groupByByMode".into(),
            serde_json::json!({"code": "project"}),
        );
        next.groups.push(group("g3", "Three"));
        store.set_scope("acct/org", &next);
        let written: Value = serde_json::from_str(&store.to_text().unwrap()).unwrap();
        assert_eq!(written["state"]["other"], 1, "unrelated fields survive");
        assert_eq!(written["state"]["collapsed"], false);
        assert_eq!(written["state"]["groupByByMode"]["code"], "project");
        assert_eq!(
            written["state"]["customGroupsByScope"]["acct/org"]["groups"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(written["version"], 1);
        assert!(Store::parse(r#"{"state":{},"version":2}"#).is_err());
        assert!(Store::parse(r#"{"version":1}"#).is_err());
    }

    #[test]
    fn canonical_requires_every_row_to_agree() {
        let mut synced = Synced::new();
        let a = state(&[("g1", "One")], &[]);
        synced.entry("A".into()).or_default().canonical_sidebar = Some(a.clone());
        synced.entry("B".into()).or_default().canonical_sidebar = Some(a.clone());
        assert_eq!(canonical(&synced), Some(a.clone()));
        synced.get_mut("B").unwrap().canonical_sidebar = None;
        assert_eq!(canonical(&synced), None);
    }

    #[test]
    fn record_keeps_the_target_observation_when_nothing_was_installed() {
        let plan = SidebarPlan {
            target_account: "T".into(),
            target_scope: "T/O".into(),
            state: state(&[("g1", "One")], &[]),
            target_observed: state(&[], &[]),
            outgoing: Some(("S".into(), "S/O".into(), state(&[("g1", "One")], &[]))),
            changed: true,
        };
        let mut synced = Synced::new();
        record(&mut synced, &plan, false);
        assert_eq!(synced["T"].sidebar_scopes["T/O"], plan.target_observed);
        assert_eq!(
            synced["S"].sidebar_scopes["S/O"],
            plan.outgoing.as_ref().unwrap().2
        );
        assert_eq!(synced["T"].canonical_sidebar, Some(plan.state.clone()));
        record(&mut synced, &plan, true);
        assert_eq!(synced["T"].sidebar_scopes["T/O"], plan.state);
    }

    /// A→B, then B→C: A's baseline must survive the rebuilt record, or the
    /// later C→A switch would union A's stale copy back in.
    #[test]
    fn baselines_of_accounts_outside_the_switch_carry_forward() {
        let mut prior = Synced::new();
        let a = state(&[("g1", "One"), ("g2", "Two")], &[("code:local_a", "g2")]);
        prior
            .entry("A".into())
            .or_default()
            .sidebar_scopes
            .insert("A/O".into(), a.clone());
        for row in prior.values_mut() {
            row.canonical_sidebar = Some(a.clone());
        }
        let mut next = Synced::new();
        next.entry("B".into()).or_default();
        carry_baselines(&prior, &mut next);
        assert_eq!(next["A"].sidebar_scopes["A/O"], a);
        assert_eq!(next["B"].canonical_sidebar, Some(a.clone()));
        assert_eq!(canonical(&next), Some(a));
    }
}
