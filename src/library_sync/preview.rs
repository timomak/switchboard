//! Read-only, local plans. These contain exact local paths but no native values.
use super::{
    Paths, Settings, engine, mcp,
    model::*,
    native::NativeAdapter,
    storage::{Archive, content_digest, error, readable_directory},
};
use crate::Result;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) fn plan(
    paths: &Paths,
    settings: &Settings,
    adapter: &mut NativeAdapter<'_>,
    id: &str,
    targets: Option<BTreeSet<Target>>,
) -> Result<Value> {
    let icloud_available = readable_directory(&paths.icloud)?;
    let archive = Archive::load(&paths.cloud)?;
    let mut state = engine::State::load(&paths.state())?;
    let catalog = engine::list(&paths.cloud)?;
    let (item, conflicts, source, library_id, publish) = if let Some(entry) =
        catalog.iter().find(|e| e.item.id == id)
    {
        if targets.is_some() {
            return Err(error(
                "For a library item, preview its saved targets without --targets.",
            ));
        }
        (
            entry.item.clone(),
            entry.conflicts.clone(),
            Value::Null,
            Some(entry.item.id.clone()),
            false,
        )
    } else {
        if uuid::Uuid::parse_str(id).is_ok() {
            return Err(error(
                "This library item was not found on this Mac. It may not have arrived or finished downloading from iCloud yet; verify delivery and try again.",
            ));
        }
        let candidate = adapter
            .inventory()?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| {
                error("This item changed or moved. Refresh inventory and choose it again.")
            })?;
        let targets = targets
            .ok_or_else(|| error("For this inventory candidate, specify --targets explicitly."))?;
        let (item, parents) = engine::selection(&archive, &candidate, targets)?;
        let source = json!({"target":candidate.source,"path":adapter.destination_path(candidate.source,&item,&candidate.locator)?});
        let library_id = (!parents.is_empty()).then(|| item.id.clone());
        engine::record_selection(
            &mut state,
            &item,
            &candidate,
            parents.first().cloned().unwrap_or_default(),
        )?;
        let publish = parents
            .first()
            .is_none_or(|r| archive.revisions[r].item != item);
        (item, vec![], source, library_id, publish)
    };
    let category_enabled = match item.content.kind() {
        Kind::Mcp => settings.sync_mcp,
        Kind::Skill => settings.sync_skills,
    };
    let mut destinations = BTreeSet::new();
    let mut rows = Vec::new();
    for target in &item.targets {
        if *target == Target::Cowork {
            rows.push(json!({"target":target,"action":"manual_export","state":"install_required","detail":"Cowork requires a separate library export and manual import."}));
        } else {
            for locator in adapter.destinations(*target, &item)? {
                destinations.insert((*target, locator));
            }
        }
    }
    for receipt in state
        .receipts
        .iter()
        .filter(|r| r.item_id == item.id && r.target != Target::Cowork)
    {
        destinations.insert((receipt.target, receipt.locator.clone()));
    }
    for (target, locator) in destinations {
        let mut installation = item.clone();
        installation.enabled &= item.targets.contains(&target);
        let mut row = adapter.preview_destination(&installation, target, &locator)?;
        let previous = state
            .receipts
            .iter()
            .find(|r| r.item_id == item.id && r.target == target && r.locator == locator);
        let duplicate = catalog.iter().any(|other| {
            other.item.id != item.id
                && other.item.active()
                && other.item.targets.contains(&target)
                && other.item.content.kind() == item.content.kind()
                && other.item.name.eq_ignore_ascii_case(&item.name)
        });
        let reason = if !conflicts.is_empty() {
            Some((
                "conflict",
                "Choose a library revision; all conflicting revisions will be preserved.",
            ))
        } else if state.has_pending(&item.id, target, &locator)
            || row["action"] == "recover_then_replan"
        {
            Some((
                "conflict",
                "An interrupted operation requires recovery; rerun the preview afterwards.",
            ))
        } else if duplicate {
            if installation.active() {
                Some((
                    "conflict",
                    "Another selected library item uses this destination name.",
                ))
            } else {
                Some((
                    "ready",
                    "The destination is retained for another selected library item.",
                ))
            }
        } else {
            let observed = adapter
                .inspect(target, &item, &locator)?
                .as_ref()
                .map(content_digest)
                .transpose()?;
            let desired = installation
                .active()
                .then(|| content_digest(&item.content))
                .transpose()?;
            if let Some(previous) = previous {
                if observed != previous.baseline {
                    Some((
                        "conflict",
                        "A managed local edit needs review. Sync may publish that edit as a new revision before deciding what to install.",
                    ))
                } else {
                    None
                }
            } else if desired.is_none() {
                Some(("ready", "No managed installation to remove."))
            } else if observed.is_some() && observed != desired {
                Some((
                    "conflict",
                    "An unmanaged item has the same name and different content; it will be preserved.",
                ))
            } else {
                None
            }
        };
        if let Some((status, detail)) = reason {
            row["state"] = json!(status);
            row["action"] = json!("preserve");
            row["detail"] = json!(detail);
        }
        if !category_enabled {
            row["category_paused"] = json!(true);
        }
        rows.push(row);
    }
    let requirements = match &item.content {
        Content::Mcp { definition } => mcp::requirements(definition)?,
        _ => item.requirements.clone(),
    };
    Ok(
        json!({"read_only":true,"id":id,"library_id":library_id,"name":crate::display::sanitize_untrusted_line(&item.name),"kind":item.content.kind(),"targets":item.targets,"source":source,"would_publish_selection":publish,"category_enabled":category_enabled,"icloud_available":icloud_available,"pending_downloads":archive.pending,"conflicts":conflicts,"requirements":requirements,"destinations":rows,"message":"Read-only local snapshot; no files, permissions, ownership, authentication or shared revisions were changed. Recheck before applying. Published selections may also be installed by automatic sync on enabled Macs."}),
    )
}

#[cfg(test)]
mod tests {
    use super::super::native::NativeRoots;
    use super::*;
    use std::{collections::BTreeMap, fs, path::Path};

    fn roots(home: &Path) -> NativeRoots {
        NativeRoots {
            user_home: home.into(),
            codex_homes: vec![home.join(".codex"), home.join("codex-work")],
            codex_skill_roots: vec![home.join(".agents/skills")],
            claude_homes: vec![home.join(".claude")],
            claude_config_files: vec![
                home.join(".claude.json"),
                home.join("claude-work/.claude.json"),
            ],
        }
    }
    fn snapshot(root: &Path) -> BTreeMap<String, (Vec<u8>, u32)> {
        fn walk(root: &Path, path: &Path, out: &mut BTreeMap<String, (Vec<u8>, u32)>) {
            use std::os::unix::fs::PermissionsExt;
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                out.insert(
                    path.strip_prefix(root).unwrap().to_string_lossy().into(),
                    (
                        if meta.is_file() {
                            fs::read(&path).unwrap()
                        } else {
                            vec![]
                        },
                        meta.permissions().mode(),
                    ),
                );
                if meta.is_dir() {
                    walk(root, &path, out);
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }
    fn item(name: &str) -> LibraryItem {
        LibraryItem {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            targets: [Target::Codex, Target::ClaudeCode].into(),
            enabled: true,
            deleted: false,
            content: Content::Mcp {
                definition: mcp::normalize(
                    &json!({"url":format!("https://{name}.example/mcp")}),
                    Target::Codex,
                )
                .unwrap(),
            },
            requirements: vec![],
        }
    }
    #[test]
    fn undelivered_library_id_reports_missing_delivery_without_candidate_instructions() {
        let t = tempfile::tempdir().unwrap();
        let paths = Paths::at(t.path().into());
        let ready = |_| panic!("An unknown library ID must not inspect native readiness");
        let mut adapter = NativeAdapter::new(
            roots(t.path()),
            paths.local.join("native"),
            BTreeMap::new(),
            &ready,
        );
        let result = plan(
            &paths,
            &Settings::default(),
            &mut adapter,
            &uuid::Uuid::new_v4().to_string(),
            None,
        );
        let message = result.unwrap_err().to_string();
        assert!(message.contains("not found on this Mac"));
        assert!(message.contains("iCloud"));
        assert!(!message.contains("--targets"));
        assert!(snapshot(t.path()).is_empty());
    }

    #[test]
    fn no_sync_adoption_while_paused_publishes_only_selection_and_keeps_native_files() {
        let t = tempfile::tempdir().unwrap();
        let paths = Paths::at(t.path().into());
        fs::create_dir_all(&paths.icloud).unwrap();
        fs::create_dir_all(t.path().join(".codex")).unwrap();
        let original = "model='keep-model'\n[mcp_servers.selected]\nurl='https://selected.example/mcp'\n[mcp_servers.unrelated]\nurl='https://unrelated.example/mcp'\n";
        fs::write(t.path().join(".codex/config.toml"), original).unwrap();
        paths.save(&Settings::default()).unwrap();
        let before_settings = fs::read(paths.settings()).unwrap();
        let ready = |_| panic!("Adoption without sync must not invoke native readiness or apply");
        let mut adapter = NativeAdapter::new(
            roots(t.path()),
            paths.local.join("native"),
            BTreeMap::new(),
            &ready,
        );
        let candidate = adapter
            .inventory()
            .unwrap()
            .into_iter()
            .find(|c| c.name == "selected")
            .unwrap();
        let action = super::super::Action::Adopt {
            id: candidate.id,
            targets: "codex,claude-code".into(),
            no_sync: true,
            json: true,
        };
        let report = super::super::execute_with_adapter(
            &paths,
            &action,
            Settings::default(),
            json!({}),
            adapter,
        )
        .unwrap();
        assert!(report["adopted_item"].is_string());
        let archive = Archive::load(&paths.cloud).unwrap();
        assert_eq!(archive.revisions.len(), 1);
        assert_eq!(
            archive.revisions.values().next().unwrap().item.name,
            "selected"
        );
        assert_eq!(fs::read(paths.settings()).unwrap(), before_settings);
        assert!(!paths.load().unwrap().sync_mcp);
        assert!(!paths.load().unwrap().sync_skills);
        assert_eq!(
            fs::read_to_string(t.path().join(".codex/config.toml")).unwrap(),
            original
        );
        assert!(!t.path().join(".claude.json").exists());
        assert!(!t.path().join("codex-work").exists());
        assert!(!paths.local.join("native").exists());
        assert!(!paths.report().exists());
        let state = engine::State::load(&paths.state()).unwrap();
        assert_eq!(state.receipts.len(), 1);
        assert_eq!(state.receipts[0].item_id, report["adopted_item"]);
    }
    #[test]
    fn candidate_plan_is_read_only_redacted_and_lists_exact_account_destinations() {
        let t = tempfile::tempdir().unwrap();
        let paths = Paths::at(t.path().into());
        fs::create_dir_all(t.path().join(".codex")).unwrap();
        fs::write(t.path().join(".codex/config.toml"),"model='keep-model'\n[mcp_servers.example]\nurl='https://example.com/mcp'\n[mcp_servers.example.http_headers]\nAuthorization='DO_NOT_PRINT_SOURCE_SECRET'\n").unwrap();
        fs::write(t.path().join(".claude.json"),br#"{"mcpServers":{"example":{"type":"http","url":"https://different.example/mcp","headers":{"Authorization":"DO_NOT_PRINT_DEST_SECRET"}}}}"#).unwrap();
        let ready = |target| Ok(target != Target::Codex);
        let mut adapter = NativeAdapter::new(
            roots(t.path()),
            paths.local.join("native"),
            BTreeMap::new(),
            &ready,
        );
        let candidate = adapter
            .inventory()
            .unwrap()
            .into_iter()
            .find(|c| c.source == Target::Codex)
            .unwrap();
        let before = snapshot(t.path());
        let result = plan(
            &paths,
            &Settings::default(),
            &mut adapter,
            &candidate.id,
            Some([Target::Codex, Target::ClaudeCode].into()),
        )
        .unwrap();
        assert_eq!(before, snapshot(t.path()));
        assert_eq!(result["read_only"], true);
        assert_eq!(result["category_enabled"], false);
        assert_eq!(
            result["source"]["path"],
            json!(t.path().join(".codex/config.toml"))
        );
        assert_eq!(result["destinations"].as_array().unwrap().len(), 4);
        let output = serde_json::to_string(&result).unwrap();
        assert!(!output.contains("DO_NOT_PRINT"));
        assert!(!output.contains("keep-model"));
        assert!(output.contains("header:Authorization"));
        assert_eq!(result["destinations"][0]["state"], "waiting_for_app");
        assert!(
            result["destinations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["state"] == "conflict")
        );
        assert!(
            result["destinations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["state"] == "needs_setup")
        );
        assert!(!paths.local.exists());
        assert!(!paths.icloud.exists());
    }
    #[test]
    fn existing_plan_never_changes_an_interrupted_operation_receipt() {
        let t = tempfile::tempdir().unwrap();
        let paths = Paths::at(t.path().into());
        let selected = item("example");
        let mut archive = Archive::default();
        let revision = archive
            .publish(&paths.cloud, selected.clone(), vec![])
            .unwrap();
        let ready = |_| Ok(true);
        let mut adapter = NativeAdapter::new(
            roots(t.path()),
            paths.local.join("native"),
            BTreeMap::new(),
            &ready,
        );
        let locator = adapter
            .destinations(Target::Codex, &selected)
            .unwrap()
            .remove(0);
        let state:engine::State = serde_json::from_value(json!({"version":1,"receipts":[],"pending":[{"receipt":{"item_id":selected.id,"target":"codex","locator":locator,"revision":revision,"baseline":null,"installed":true,"status":"needs_setup","detail":"Synthetic interrupted operation"},"before":null}]})).unwrap();
        state.save(&paths.state()).unwrap();
        let before = snapshot(t.path());
        let result = plan(
            &paths,
            &Settings {
                sync_mcp: true,
                ..Settings::default()
            },
            &mut adapter,
            &selected.id,
            None,
        )
        .unwrap();
        assert_eq!(before, snapshot(t.path()));
        assert!(
            result["destinations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["state"] == "conflict")
        );
        assert_eq!(result["library_id"], selected.id);
    }
    #[test]
    fn scoped_apply_preserves_other_native_entries_settings_and_unpublished_edits() {
        let t = tempfile::tempdir().unwrap();
        let paths = Paths::at(t.path().into());
        let first = item("first");
        let other = item("other");
        let mut archive = Archive::default();
        let base = archive
            .publish(&paths.cloud, first.clone(), vec![])
            .unwrap();
        archive
            .publish(&paths.cloud, other.clone(), vec![])
            .unwrap();
        fs::create_dir_all(t.path().join(".codex")).unwrap();
        fs::write(
            t.path().join(".codex/config.toml"),
            "# Keep me\nmodel='keep-model'\n",
        )
        .unwrap();
        fs::write(t.path().join(".claude.json"), br#"{"theme":"keep-theme"}"#).unwrap();
        let ready = |_| Ok(true);
        let mut adapter = NativeAdapter::new(
            roots(t.path()),
            paths.local.join("native"),
            BTreeMap::new(),
            &ready,
        );
        let mut state = engine::State::default();
        engine::run(
            &paths.cloud,
            &paths.state(),
            &mut state,
            &[Kind::Mcp].into(),
            &mut adapter,
        )
        .unwrap();
        let original = fs::read_to_string(t.path().join(".codex/config.toml")).unwrap();
        fs::write(
            t.path().join(".codex/config.toml"),
            original.replace(
                "https://other.example/mcp",
                "https://locally-edited.example/mcp",
            ),
        )
        .unwrap();
        let unrelated_receipts = serde_json::to_value(
            state
                .receipts
                .iter()
                .filter(|r| r.item_id == other.id)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let mut next = first.clone();
        next.content = Content::Mcp {
            definition: mcp::normalize(
                &json!({"url":"https://first-updated.example/mcp"}),
                Target::Codex,
            )
            .unwrap(),
        };
        archive.publish(&paths.cloud, next, vec![base]).unwrap();
        let before_count = Archive::load(&paths.cloud).unwrap().revisions.len();
        let report = engine::run_selected(
            &paths.cloud,
            &paths.state(),
            &mut state,
            &[Kind::Mcp].into(),
            &mut adapter,
            Some(&first.id),
        )
        .unwrap();
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.published, 0);
        assert_eq!(report.installed, 4);
        assert_eq!(
            before_count,
            Archive::load(&paths.cloud).unwrap().revisions.len()
        );
        assert_eq!(
            unrelated_receipts,
            serde_json::to_value(
                state
                    .receipts
                    .iter()
                    .filter(|r| r.item_id == other.id)
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
        let text = fs::read_to_string(t.path().join(".codex/config.toml")).unwrap();
        assert!(text.contains("# Keep me"));
        assert!(text.contains("keep-model"));
        assert!(text.contains("https://locally-edited.example/mcp"));
        assert!(text.contains("https://first-updated.example/mcp"));
        let claude: Value =
            serde_json::from_slice(&fs::read(t.path().join(".claude.json")).unwrap()).unwrap();
        assert_eq!(claude["theme"], "keep-theme");
        assert_eq!(
            claude["mcpServers"]["other"]["url"],
            "https://other.example/mcp"
        );
    }
    #[test]
    fn scoped_apply_waits_for_busy_apps_and_leaves_shared_unrelated_recovery_pending() {
        let t = tempfile::tempdir().unwrap();
        let paths = Paths::at(t.path().into());
        let first = item("first");
        let other = item("other");
        let mut archive = Archive::default();
        let first_revision = archive
            .publish(&paths.cloud, first.clone(), vec![])
            .unwrap();
        let revision = archive
            .publish(&paths.cloud, other.clone(), vec![])
            .unwrap();
        let busy = |_| Ok(false);
        let mut adapter = NativeAdapter::new(
            roots(t.path()),
            paths.local.join("native"),
            BTreeMap::new(),
            &busy,
        );
        let mut state = engine::State::default();
        let report = engine::run_selected(
            &paths.cloud,
            &paths.state(),
            &mut state,
            &[Kind::Mcp].into(),
            &mut adapter,
            Some(&first.id),
        )
        .unwrap();
        assert_eq!(report.installed, 0);
        assert!(
            report.items[0]
                .destinations
                .iter()
                .all(|d| d.status == InstallStatus::WaitingForApp)
        );
        assert!(!paths.state().exists());
        let locator = adapter
            .destinations(Target::Codex, &other)
            .unwrap()
            .remove(0);
        let pending = json!({"receipt":{"item_id":other.id,"target":"codex","locator":locator,"revision":revision,"baseline":null,"installed":true,"status":"needs_setup","detail":"Synthetic interrupted operation"},"before":null});
        let mut selected_pending = pending.clone();
        selected_pending["receipt"]["item_id"] = json!(first.id);
        selected_pending["receipt"]["revision"] = json!(first_revision);
        let mut value = serde_json::to_value(&state).unwrap();
        value["pending"] = json!([selected_pending.clone(), pending.clone()]);
        state = serde_json::from_value(value).unwrap();
        let ready = |_| Ok(true);
        let mut adapter = NativeAdapter::new(
            roots(t.path()),
            paths.local.join("native"),
            BTreeMap::new(),
            &ready,
        );
        let report = engine::run_selected(
            &paths.cloud,
            &paths.state(),
            &mut state,
            &[Kind::Mcp].into(),
            &mut adapter,
            Some(&first.id),
        )
        .unwrap();
        assert!(
            report.items[0]
                .destinations
                .iter()
                .any(|d| d.status == InstallStatus::Conflict)
        );
        assert_eq!(
            serde_json::to_value(&state).unwrap()["pending"],
            json!([selected_pending, pending])
        );
        assert!(!t.path().join(".codex/config.toml").exists());
    }
}
