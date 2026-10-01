use super::model::*;
use super::storage::{Archive, VERSION, atomic_write, content_digest, error, read_regular};
use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub item_id: String,
    pub target: Target,
    pub locator: String,
    pub revision: String,
    pub baseline: Option<String>,
    pub installed: bool,
    pub status: InstallStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    receipt: Receipt,
    before: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub receipts: Vec<Receipt>,
    pending: Vec<Pending>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: VERSION,
            receipts: vec![],
            pending: vec![],
        }
    }
}
impl State {
    pub fn has_pending(&self, item_id: &str, target: Target, locator: &str) -> bool {
        self.pending.iter().any(|p| {
            p.receipt.item_id == item_id
                && p.receipt.target == target
                && p.receipt.locator == locator
        })
    }
    pub fn load(path: &Path) -> Result<Self> {
        if !path.try_exists()? {
            return Ok(Self::default());
        }
        let state: Self =
            serde_json::from_slice(&read_regular(path, 16 * 1024 * 1024)?).map_err(|_| {
                error("The local library receipts are unreadable. Restore them before syncing.")
            })?;
        if state.version != VERSION {
            return Err(error(
                "Update Switchboard to read the local library receipts.",
            ));
        }
        Ok(state)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write(path, &serde_json::to_vec(self)?)
    }
    fn record(&mut self, receipt: Receipt) {
        self.receipts.retain(|r| !same_destination(r, &receipt));
        self.pending
            .retain(|p| !same_destination(&p.receipt, &receipt));
        self.receipts.push(receipt);
    }
}
fn same_destination(a: &Receipt, b: &Receipt) -> bool {
    a.item_id == b.item_id && a.target == b.target && a.locator == b.locator
}
fn fingerprint(content: Option<&Content>) -> Result<Option<String>> {
    content.map(content_digest).transpose()
}

/// Explicit selection is the only way an inventory candidate enters iCloud.
pub fn select(
    root: &Path,
    state_path: &Path,
    state: &mut State,
    candidate: &InventoryCandidate,
    targets: BTreeSet<Target>,
) -> Result<LibraryItem> {
    let mut archive = Archive::load(root)?;
    let (item, parents) = selection(&archive, candidate, targets)?;
    let revision = if parents.len() == 1 && archive.revisions[&parents[0]].item == item {
        parents[0].clone()
    } else {
        archive.publish(root, item.clone(), parents)?
    };
    record_selection(state, &item, candidate, revision)?;
    state.save(state_path)?;
    Ok(item)
}

/// Read-only preparation shared by adoption and the local plan.
pub fn selection(
    archive: &Archive,
    candidate: &InventoryCandidate,
    targets: BTreeSet<Target>,
) -> Result<(LibraryItem, Vec<String>)> {
    if candidate.classification != "custom" {
        return Err(error("Only custom items can be selected for this library."));
    }
    let content = candidate
        .content
        .clone()
        .ok_or_else(|| error("This item has no supported portable definition."))?;
    if archive.pending > 0 {
        return Err(error(
            "Wait for the library to finish downloading before selecting items.",
        ));
    }
    let existing = archive.ids().into_iter().find_map(|id| {
        let heads = archive.heads(&id);
        if heads.len() != 1 {
            return None;
        }
        let item = &archive.revisions[&heads[0]].item;
        (item.name.eq_ignore_ascii_case(&candidate.name)
            && item.content == content
            && !item.deleted)
            .then(|| (item.clone(), heads[0].clone()))
    });
    let (mut item, parents) = if let Some((item, revision)) = existing {
        (item, vec![revision])
    } else {
        (
            LibraryItem {
                id: uuid::Uuid::new_v4().to_string(),
                name: candidate.name.clone(),
                targets: BTreeSet::new(),
                enabled: true,
                deleted: false,
                content,
                requirements: candidate.requirements.clone(),
            },
            vec![],
        )
    };
    item.targets.extend(targets);
    item.enabled = true;
    Ok((item, parents))
}

pub fn record_selection(
    state: &mut State,
    item: &LibraryItem,
    candidate: &InventoryCandidate,
    revision: String,
) -> Result<()> {
    if item.targets.contains(&candidate.source) && candidate.source != Target::Cowork {
        let receipt = Receipt {
            item_id: item.id.clone(),
            target: candidate.source,
            locator: candidate.locator.clone(),
            revision,
            baseline: Some(content_digest(&item.content)?),
            installed: false,
            status: InstallStatus::NeedsSetup,
            detail: "Selected; installation pending.".into(),
        };
        if !state.receipts.iter().any(|r| same_destination(r, &receipt)) {
            state.record(receipt);
        }
    }
    Ok(())
}

pub fn update(
    root: &Path,
    item_id: &str,
    targets: Option<BTreeSet<Target>>,
    enabled: Option<bool>,
    deleted: bool,
) -> Result<LibraryItem> {
    let mut archive = Archive::load(root)?;
    if archive.pending > 0 {
        return Err(error(
            "Wait for the library to finish downloading before changing this item.",
        ));
    }
    let heads = archive.heads(item_id);
    if heads.len() != 1 {
        return Err(error(
            "Resolve this item's conflicting revisions before changing its settings.",
        ));
    }
    let mut item = archive.revisions[&heads[0]].item.clone();
    if let Some(targets) = targets {
        item.targets = targets;
    }
    if let Some(enabled) = enabled {
        item.enabled = enabled;
    }
    item.deleted |= deleted;
    if item.deleted {
        item.enabled = false;
    }
    if item != archive.revisions[&heads[0]].item {
        archive.publish(root, item.clone(), heads)?;
    }
    Ok(item)
}

pub fn resolve(root: &Path, item_id: &str, revision: &str) -> Result<LibraryItem> {
    let mut archive = Archive::load(root)?;
    if archive.pending > 0 {
        return Err(error(
            "Wait for the library to finish downloading before resolving conflicts.",
        ));
    }
    let heads = archive.heads(item_id);
    if !heads.iter().any(|r| r == revision) {
        return Err(error("Choose one of the current conflicting revisions."));
    }
    let item = archive.revisions[revision].item.clone();
    if heads.len() > 1 {
        archive.publish(root, item.clone(), heads)?;
    }
    Ok(item)
}

pub fn list(root: &Path) -> Result<Vec<ItemStatus>> {
    let archive = Archive::load(root)?;
    Ok(list_archive(&archive))
}
fn list_archive(archive: &Archive) -> Vec<ItemStatus> {
    archive
        .ids()
        .into_iter()
        .filter_map(|id| {
            let heads = archive.heads(&id);
            let revision = heads.first()?.clone();
            Some(ItemStatus {
                item: archive.revisions[&revision].item.clone(),
                revision,
                conflicts: if heads.len() > 1 { heads } else { vec![] },
                destinations: vec![],
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn recover(
    root: &Path,
    state_path: &Path,
    state: &mut State,
    archive: &mut Archive,
    enabled_kinds: &BTreeSet<Kind>,
    selected: Option<&str>,
    adapter: &mut impl Adapter,
) -> Result<BTreeSet<(String, Target, String)>> {
    let mut blocked = BTreeSet::new();
    if let Some(id) = selected {
        for pending in &state.pending {
            let receipt = &pending.receipt;
            if receipt.item_id == id {
                continue;
            }
            // MCP entries share one native configuration file. Do not let a
            // selected apply recover another item's interrupted file swap.
            blocked.insert((id.to_owned(), receipt.target, receipt.locator.clone()));
        }
    }
    for pending in state.pending.clone() {
        let receipt = &pending.receipt;
        if selected.is_some_and(|id| receipt.item_id != id)
            || blocked.contains(&(
                receipt.item_id.clone(),
                receipt.target,
                receipt.locator.clone(),
            ))
        {
            continue;
        }
        let Some(package) = archive.revisions.get(&receipt.revision).cloned() else {
            blocked.insert((
                receipt.item_id.clone(),
                receipt.target,
                receipt.locator.clone(),
            ));
            continue;
        };
        if !enabled_kinds.contains(&package.item.content.kind()) {
            continue;
        }
        if !adapter.ready(receipt.target)? {
            blocked.insert((
                receipt.item_id.clone(),
                receipt.target,
                receipt.locator.clone(),
            ));
            continue;
        }
        adapter.recover_pending(receipt.target, &package.item, &receipt.locator)?;
        let observed = adapter.inspect(receipt.target, &package.item, &receipt.locator)?;
        let current = fingerprint(observed.as_ref())?;
        if current == receipt.baseline {
            state.record(receipt.clone());
            state.save(state_path)?;
        } else if current == pending.before {
            state
                .pending
                .retain(|p| !same_destination(&p.receipt, receipt));
            state.save(state_path)?;
        } else if let Some(content) = observed {
            // Preserve a continuation after interruption as a competing
            // revision. The UI can then resolve it without discarding files.
            let mut item = package.item;
            item.content = content;
            item.enabled = true;
            item.deleted = false;
            let parents = state
                .receipts
                .iter()
                .find(|r| same_destination(r, receipt))
                .map(|r| vec![r.revision.clone()])
                .unwrap_or(package.parents);
            let revision = archive.publish(root, item, parents)?;
            state.record(Receipt {
                revision,
                baseline: current,
                installed: false,
                ..receipt.clone()
            });
            state.save(state_path)?;
        } else {
            // A user edited the destination after a crash. Never infer that
            // a rollback or overwrite is safe from the portable revision.
            blocked.insert((
                receipt.item_id.clone(),
                receipt.target,
                receipt.locator.clone(),
            ));
        }
    }
    Ok(blocked)
}

pub fn run(
    root: &Path,
    state_path: &Path,
    state: &mut State,
    enabled_kinds: &BTreeSet<Kind>,
    adapter: &mut impl Adapter,
) -> Result<RunReport> {
    run_selected(root, state_path, state, enabled_kinds, adapter, None)
}

pub fn run_selected(
    root: &Path,
    state_path: &Path,
    state: &mut State,
    enabled_kinds: &BTreeSet<Kind>,
    adapter: &mut impl Adapter,
    selected: Option<&str>,
) -> Result<RunReport> {
    adapter.limit_recovery_to(selected);
    let mut archive = Archive::load(root)?;
    if selected.is_some_and(|id| archive.heads(id).is_empty()) {
        return Err(error(
            "This library item is unavailable; no items were synced.",
        ));
    }
    let mut report = RunReport {
        pending: archive.pending,
        ..RunReport::default()
    };
    let blocked = recover(
        root,
        state_path,
        state,
        &mut archive,
        enabled_kinds,
        selected,
        adapter,
    )?;
    // Export every independent local edit before deciding which cloud head
    // may be installed. This is essential for account and Mac conflicts.
    for receipt in state.receipts.clone() {
        if selected.is_some_and(|id| receipt.item_id != id) {
            continue;
        }
        if blocked.contains(&(
            receipt.item_id.clone(),
            receipt.target,
            receipt.locator.clone(),
        )) {
            continue;
        }
        if !adapter.ready(receipt.target)? {
            continue;
        }
        let Some(package) = archive.revisions.get(&receipt.revision) else {
            report.pending += 1;
            continue;
        };
        let mut item = package.item.clone();
        if !enabled_kinds.contains(&item.content.kind())
            || !item.active()
            || !item.targets.contains(&receipt.target)
        {
            continue;
        }
        let observed = adapter.inspect(receipt.target, &item, &receipt.locator)?;
        if let Some(content) = observed
            && Some(content_digest(&content)?) != receipt.baseline
        {
            if !adapter.ready(receipt.target)?
                || adapter
                    .inspect(receipt.target, &item, &receipt.locator)?
                    .as_ref()
                    != Some(&content)
            {
                report.pending += 1;
                continue;
            }
            item.content = content;
            let revision = archive.publish(root, item.clone(), vec![receipt.revision.clone()])?;
            state.record(Receipt {
                revision,
                baseline: Some(content_digest(&item.content)?),
                installed: false,
                ..receipt
            });
            state.save(state_path)?;
            report.published += 1;
        }
    }
    report.items = list_archive(&archive);
    let catalog = report.items.clone();
    report
        .items
        .retain(|s| selected.is_none_or(|id| s.item.id == id));
    for status in &mut report.items {
        let item = &status.item;
        if !enabled_kinds.contains(&item.content.kind()) {
            continue;
        }
        if !status.conflicts.is_empty() {
            report.conflicts += 1;
            status.destinations = item.targets.iter().map(|target| DestinationStatus { target: *target, status: InstallStatus::Conflict, detail: "Choose a revision. Both edits are retained; the last working installation is kept.".into() }).collect();
            continue;
        }
        let mut destinations = BTreeSet::new();
        for target in &item.targets {
            if *target == Target::Cowork {
                if item.active() {
                    status.destinations.push(DestinationStatus {
                        target: *target,
                        status: InstallStatus::InstallRequired,
                        detail: "Export the library plugin and install it in each Cowork account."
                            .into(),
                    });
                }
                continue;
            }
            for locator in adapter.destinations(*target, item)? {
                destinations.insert((*target, locator));
            }
        }
        // Removed targets still need an ownership-aware uninstall operation.
        for receipt in state.receipts.iter().filter(|r| r.item_id == item.id) {
            destinations.insert((receipt.target, receipt.locator.clone()));
        }
        for (target, locator) in destinations {
            if target == Target::Cowork {
                continue;
            }
            if catalog.iter().any(|other| {
                other.item.id != item.id
                    && other.item.active()
                    && other.item.targets.contains(&target)
                    && other.item.content.kind() == item.content.kind()
                    && other.item.name.eq_ignore_ascii_case(&item.name)
            }) {
                if !item.active() || !item.targets.contains(&target) {
                    // An explicit removal of a duplicate must not remove the
                    // surviving library item's shared native destination.
                    state.receipts.retain(|r| {
                        !(r.item_id == item.id && r.target == target && r.locator == locator)
                    });
                    state.pending.retain(|p| {
                        !(p.receipt.item_id == item.id
                            && p.receipt.target == target
                            && p.receipt.locator == locator)
                    });
                    state.save(state_path)?;
                    status.destinations.push(DestinationStatus {
                        target,
                        status: InstallStatus::Ready,
                        detail: "The shared destination is retained for another selected item."
                            .into(),
                    });
                    continue;
                }
                status.destinations.push(DestinationStatus { target, status: InstallStatus::Conflict, detail: "Another selected library item uses the same destination name. Existing content was preserved.".into() });
                report.conflicts += 1;
                continue;
            }
            let result = sync_destination(
                state_path,
                state,
                &blocked,
                status.revision.as_str(),
                item,
                target,
                &locator,
                adapter,
            )?;
            report.installed += usize::from(result.1);
            match result.0.status {
                InstallStatus::Conflict => report.conflicts += 1,
                InstallStatus::Ready => (),
                _ => report.pending += 1,
            }
            status.destinations.push(DestinationStatus {
                target,
                status: result.0.status,
                detail: result.0.detail,
            });
        }
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn sync_destination(
    state_path: &Path,
    state: &mut State,
    blocked: &BTreeSet<(String, Target, String)>,
    revision: &str,
    item: &LibraryItem,
    target: Target,
    locator: &str,
    adapter: &mut impl Adapter,
) -> Result<(ApplyResult, bool)> {
    let result = |status, detail: &str| {
        (
            ApplyResult {
                status,
                detail: detail.into(),
                applied: false,
            },
            false,
        )
    };
    if !adapter.ready(target)? {
        return Ok(result(
            InstallStatus::WaitingForApp,
            "Close the app and its CLI sessions to sync this destination.",
        ));
    }
    if blocked.contains(&(item.id.clone(), target, locator.to_owned())) {
        return Ok(result(
            InstallStatus::Conflict,
            "The destination changed after an interrupted installation. Its contents were preserved.",
        ));
    }
    let previous = state
        .receipts
        .iter()
        .find(|r| r.item_id == item.id && r.target == target && r.locator == locator)
        .cloned();
    let mut installation = item.clone();
    installation.enabled &= item.targets.contains(&target);
    let desired = if installation.active() {
        Some(content_digest(&item.content)?)
    } else {
        None
    };
    if desired.is_none() && previous.is_none() {
        return Ok(result(
            InstallStatus::Ready,
            "No managed installation to remove.",
        ));
    }
    let observed = fingerprint(adapter.inspect(target, item, locator)?.as_ref())?;
    if let Some(previous) = &previous {
        if observed != previous.baseline {
            return Ok(result(
                InstallStatus::Conflict,
                "This managed destination changed locally. Its contents were preserved.",
            ));
        }
        if previous.installed
            && previous.status == InstallStatus::Ready
            && previous.revision == revision
            && previous.baseline == desired
        {
            return Ok(result(previous.status, &previous.detail));
        }
    } else if observed.is_some() && observed != desired {
        return Ok(result(
            InstallStatus::Conflict,
            "An existing item has the same name and different content. It was preserved.",
        ));
    }
    let mut receipt = Receipt {
        item_id: item.id.clone(),
        target,
        locator: locator.to_owned(),
        revision: revision.to_owned(),
        baseline: desired.clone(),
        installed: true,
        status: InstallStatus::NeedsSetup,
        detail: "Installation requires verification.".into(),
    };
    state
        .pending
        .retain(|p| !same_destination(&p.receipt, &receipt));
    state.pending.push(Pending {
        receipt: receipt.clone(),
        before: observed.clone(),
    });
    state.save(state_path)?;
    let applied = adapter.apply(target, &installation, locator, observed.as_deref())?;
    if !applied.applied {
        state
            .pending
            .retain(|p| !same_destination(&p.receipt, &receipt));
        state.save(state_path)?;
        return Ok((applied, false));
    }
    if fingerprint(adapter.inspect(target, &installation, locator)?.as_ref())? != desired {
        return Err(error(
            "The library installation could not be verified. Its recovery receipt was retained.",
        ));
    }
    receipt.status = applied.status;
    receipt.detail = applied.detail.clone();
    state.record(receipt);
    state.save(state_path)?;
    Ok((applied, true))
}
