use super::{engine::*, model::*, storage::*};
use crate::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Default)]
struct Native {
    files: BTreeMap<String, Content>,
    writes: usize,
    fail_after_apply: bool,
    busy: bool,
    homes: usize,
}
impl Native {
    fn location(target: Target, name: &str, index: usize) -> String {
        format!("{}:{index}:{name}", target.key())
    }
    fn get(&self, target: Target, name: &str) -> &Content {
        &self.files[&Self::location(target, name, 0)]
    }
    fn edit(&mut self, target: Target, name: &str, text: &str) {
        self.files
            .insert(Self::location(target, name, 0), content(text));
    }
}
impl Adapter for Native {
    fn destinations(&mut self, target: Target, item: &LibraryItem) -> Result<Vec<String>> {
        Ok((0..self.homes.max(1))
            .map(|index| Self::location(target, &item.name, index))
            .collect())
    }
    fn inspect(
        &mut self,
        _target: Target,
        _item: &LibraryItem,
        locator: &str,
    ) -> Result<Option<Content>> {
        Ok(self.files.get(locator).cloned())
    }
    fn apply(
        &mut self,
        _target: Target,
        item: &LibraryItem,
        locator: &str,
        expected: Option<&str>,
    ) -> Result<ApplyResult> {
        if self.busy {
            return Ok(ApplyResult {
                status: InstallStatus::WaitingForApp,
                detail: "Close app".into(),
                applied: false,
            });
        }
        if self
            .files
            .get(locator)
            .map(content_digest)
            .transpose()?
            .as_deref()
            != expected
        {
            return Err(error("Changed during staging"));
        }
        let desired = item.active().then(|| item.content.clone());
        if self.files.get(locator) != desired.as_ref() {
            self.writes += 1;
        }
        if let Some(content) = desired {
            self.files.insert(locator.into(), content);
        } else {
            self.files.remove(locator);
        }
        if self.fail_after_apply {
            self.fail_after_apply = false;
            return Err(error("Simulated interrupted write"));
        }
        Ok(ApplyResult {
            status: InstallStatus::Ready,
            detail: "Up to date".into(),
            applied: true,
        })
    }
}
fn content(text: &str) -> Content {
    Content::Mcp {
        definition: serde_json::json!({"transport":"http","url":format!("https://{text}.example/mcp")}),
    }
}
fn kinds() -> BTreeSet<Kind> {
    BTreeSet::from([Kind::Mcp, Kind::Skill])
}
fn select_fixture(
    root: &Path,
    state_path: &Path,
    state: &mut State,
    native: &mut Native,
) -> LibraryItem {
    native.edit(Target::Codex, "example", "initial");
    select(
        root,
        state_path,
        state,
        &InventoryCandidate {
            id: "candidate".into(),
            name: "example".into(),
            kind: Kind::Mcp,
            source: Target::Codex,
            classification: "custom".into(),
            detail: "fixture".into(),
            locator: Native::location(Target::Codex, "example", 0),
            content: Some(content("initial")),
            requirements: vec![],
        },
        BTreeSet::from([Target::Codex, Target::ClaudeCode]),
    )
    .unwrap()
}
#[test]
fn two_macs_share_definitions_across_all_local_homes_without_echo() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("cloud");
    let a_path = temp.path().join("a/state");
    let b_path = temp.path().join("b/state");
    let (mut a, mut b) = (State::default(), State::default());
    let (mut an, mut bn) = (
        Native {
            homes: 2,
            ..Native::default()
        },
        Native {
            homes: 2,
            ..Native::default()
        },
    );
    select_fixture(&root, &a_path, &mut a, &mut an);
    run(&root, &a_path, &mut a, &kinds(), &mut an).unwrap();
    run(&root, &b_path, &mut b, &kinds(), &mut bn).unwrap();
    assert_eq!(an.files, bn.files);
    assert_eq!(bn.files.len(), 4);
    bn.edit(Target::ClaudeCode, "example", "second");
    assert_eq!(
        run(&root, &b_path, &mut b, &kinds(), &mut bn)
            .unwrap()
            .published,
        1
    );
    run(&root, &a_path, &mut a, &kinds(), &mut an).unwrap();
    assert_eq!(an.files, bn.files);
    let again = run(&root, &a_path, &mut a, &kinds(), &mut an).unwrap();
    assert_eq!((again.published, again.installed), (0, 0));
}
#[test]
fn concurrent_edits_keep_working_versions_until_explicit_resolution() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let ap = t.path().join("a");
    let bp = t.path().join("b");
    let (mut a, mut b) = (State::default(), State::default());
    let (mut an, mut bn) = (Native::default(), Native::default());
    let item = select_fixture(&root, &ap, &mut a, &mut an);
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    an.edit(Target::Codex, "example", "alpha");
    bn.edit(Target::Codex, "example", "bravo");
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    let conflict = run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    assert_eq!(conflict.items[0].conflicts.len(), 2);
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    assert_eq!(an.get(Target::Codex, "example"), &content("alpha"));
    assert_eq!(bn.get(Target::Codex, "example"), &content("bravo"));
    let archive = Archive::load(&root).unwrap();
    let chosen = archive
        .heads(&item.id)
        .into_iter()
        .find(|r| archive.revisions[r].item.content == content("alpha"))
        .unwrap();
    resolve(&root, &item.id, &chosen).unwrap();
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    assert_eq!(an.files, bn.files);
}
#[test]
fn tombstones_reach_offline_peers_without_resurrection() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let ap = t.path().join("a");
    let bp = t.path().join("b");
    let (mut a, mut b) = (State::default(), State::default());
    let (mut an, mut bn) = (Native::default(), Native::default());
    let item = select_fixture(&root, &ap, &mut a, &mut an);
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    update(&root, &item.id, None, None, true).unwrap();
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    assert!(an.files.is_empty() && bn.files.is_empty());
    assert_eq!(
        run(&root, &bp, &mut b, &kinds(), &mut bn)
            .unwrap()
            .published,
        0
    );
    assert!(list(&root).unwrap()[0].item.deleted);
}
#[test]
fn edit_against_tombstone_is_a_conflict_and_never_reactivates_peer() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let ap = t.path().join("a");
    let bp = t.path().join("b");
    let (mut a, mut b) = (State::default(), State::default());
    let (mut an, mut bn) = (Native::default(), Native::default());
    let item = select_fixture(&root, &ap, &mut a, &mut an);
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    update(&root, &item.id, None, None, true).unwrap();
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    bn.edit(Target::Codex, "example", "offline");
    let conflict = run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    assert_eq!(conflict.items[0].conflicts.len(), 2);
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    assert!(an.files.is_empty());
    assert_eq!(bn.get(Target::Codex, "example"), &content("offline"));
}
#[test]
fn interrupted_install_recovers_without_duplicate_writes() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let ap = t.path().join("a");
    let bp = t.path().join("b");
    let (mut a, mut b) = (State::default(), State::default());
    let (mut an, mut bn) = (
        Native::default(),
        Native {
            fail_after_apply: true,
            ..Native::default()
        },
    );
    select_fixture(&root, &ap, &mut a, &mut an);
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    assert!(run(&root, &bp, &mut b, &kinds(), &mut bn).is_err());
    b = State::load(&bp).unwrap();
    run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    assert_eq!(bn.writes, 2);
    assert_eq!(bn.files, an.files);
}
#[test]
fn edits_after_interruption_are_retained_as_resolvable_conflicts() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let ap = t.path().join("a");
    let bp = t.path().join("b");
    let (mut a, mut b) = (State::default(), State::default());
    let (mut an, mut bn) = (
        Native::default(),
        Native {
            fail_after_apply: true,
            ..Native::default()
        },
    );
    select_fixture(&root, &ap, &mut a, &mut an);
    run(&root, &ap, &mut a, &kinds(), &mut an).unwrap();
    assert!(run(&root, &bp, &mut b, &kinds(), &mut bn).is_err());
    bn.edit(Target::Codex, "example", "after-crash");
    b = State::load(&bp).unwrap();
    let result = run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    assert_eq!(result.items[0].conflicts.len(), 2);
    assert_eq!(bn.get(Target::Codex, "example"), &content("after-crash"));
}
#[test]
fn unmanaged_collision_and_paused_kind_are_never_modified() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let ap = t.path().join("a");
    let bp = t.path().join("b");
    let (mut a, mut b) = (State::default(), State::default());
    let (mut an, mut bn) = (Native::default(), Native::default());
    select_fixture(&root, &ap, &mut a, &mut an);
    bn.edit(Target::Codex, "example", "unmanaged");
    let paused = run(&root, &bp, &mut b, &BTreeSet::new(), &mut bn).unwrap();
    assert_eq!(paused.installed, 0);
    assert_eq!(bn.files.len(), 1);
    let report = run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    assert_eq!(report.conflicts, 1);
    assert_eq!(bn.get(Target::Codex, "example"), &content("unmanaged"));
}
#[test]
fn busy_destinations_leave_retries_and_contents_intact() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let ap = t.path().join("a");
    let bp = t.path().join("b");
    let (mut a, mut b) = (State::default(), State::default());
    let (mut an, mut bn) = (
        Native::default(),
        Native {
            busy: true,
            ..Native::default()
        },
    );
    select_fixture(&root, &ap, &mut a, &mut an);
    let waiting = run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    assert_eq!(waiting.pending, 2);
    assert!(bn.files.is_empty());
    bn.busy = false;
    run(&root, &bp, &mut b, &kinds(), &mut bn).unwrap();
    assert_eq!(bn.files.len(), 2);
}
#[test]
fn same_content_selection_adopts_identity_and_extends_targets() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let path = t.path().join("state");
    let mut state = State::default();
    let mut native = Native::default();
    let first = select_fixture(&root, &path, &mut state, &mut native);
    let second = select_fixture(&root, &path, &mut state, &mut native);
    assert_eq!(first.id, second.id);
    assert_eq!(list(&root).unwrap().len(), 1);
}
#[test]
fn incomplete_and_tampered_downloads_are_deferred() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let path = t.path().join("state");
    let mut state = State::default();
    let mut native = Native::default();
    let item = select_fixture(&root, &path, &mut state, &mut native);
    let revision = list(&root).unwrap()[0].revision.clone();
    let file = root
        .join("items")
        .join(item.id)
        .join(format!("{revision}.json"));
    std::fs::write(file, b"partial download").unwrap();
    let archive = Archive::load(&root).unwrap();
    assert_eq!(archive.pending, 1);
    assert!(archive.revisions.is_empty());
}

#[test]
fn removing_duplicate_preserves_the_surviving_selected_item() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let path = t.path().join("state");
    let mut state = State::default();
    let mut native = Native::default();
    let first = select_fixture(&root, &path, &mut state, &mut native);
    run(&root, &path, &mut state, &kinds(), &mut native).unwrap();
    let mut duplicate = first.clone();
    duplicate.id = uuid::Uuid::new_v4().to_string();
    Archive::load(&root)
        .unwrap()
        .publish(&root, duplicate.clone(), vec![])
        .unwrap();
    assert!(
        run(&root, &path, &mut state, &kinds(), &mut native)
            .unwrap()
            .conflicts
            > 0
    );
    update(&root, &duplicate.id, None, None, true).unwrap();
    assert_eq!(
        run(&root, &path, &mut state, &kinds(), &mut native)
            .unwrap()
            .conflicts,
        0
    );
    assert_eq!(native.files.len(), 2);
}

#[test]
fn missing_ancestry_defers_the_entire_item() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cloud");
    let path = t.path().join("state");
    let mut state = State::default();
    let mut native = Native::default();
    let item = select_fixture(&root, &path, &mut state, &mut native);
    let before = list(&root).unwrap()[0].revision.clone();
    update(&root, &item.id, None, Some(false), false).unwrap();
    std::fs::remove_file(
        root.join("items")
            .join(item.id)
            .join(format!("{before}.json")),
    )
    .unwrap();
    let archive = Archive::load(&root).unwrap();
    assert_eq!(archive.pending, 1);
    assert!(archive.revisions.is_empty());
}

#[test]
fn unsafe_skill_paths_cannot_enter_transport() {
    let mut item = LibraryItem {
        id: uuid::Uuid::new_v4().to_string(),
        name: "test".into(),
        targets: BTreeSet::new(),
        enabled: true,
        deleted: false,
        content: Content::Skill {
            files: vec![SkillFile {
                path: "../SKILL.md".into(),
                content_base64: "eA==".into(),
                executable: false,
            }],
        },
        requirements: vec![],
    };
    assert!(validate_item(&item).is_err());
    item.content = Content::Skill {
        files: vec![
            SkillFile {
                path: "SKILL.md".into(),
                content_base64: "eA==".into(),
                executable: false,
            },
            SkillFile {
                path: "SKILL.md".into(),
                content_base64: "eA==".into(),
                executable: true,
            },
        ],
    };
    assert!(validate_item(&item).is_err());
}

#[test]
fn real_native_mcp_adapters_converge_across_macs_and_preserve_each_accounts_secrets() {
    use super::native::{NativeAdapter, NativeRoots};
    fn fixture(home: &Path, secret: &str) -> NativeRoots {
        let roots = NativeRoots {
            claude_config_files: vec![],
            user_home: home.into(),
            codex_homes: vec![home.join(".codex"), home.join("codex-work")],
            codex_skill_roots: vec![home.join(".agents/skills")],
            claude_homes: vec![home.join(".claude"), home.join("claude-work")],
        };
        for root in &roots.codex_homes {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("config.toml"),format!("# retained comment\nmodel='fixture-model'\n[mcp_servers.example]\nurl='https://initial.example/mcp'\n[mcp_servers.example.http_headers]\nAuthorization='{secret}'\n")).unwrap();
        }
        for root in &roots.claude_homes {
            std::fs::create_dir_all(root).unwrap();
            let path = if root == &home.join(".claude") {
                home.join(".claude.json")
            } else {
                root.join(".claude.json")
            };
            std::fs::write(path,serde_json::to_vec(&serde_json::json!({"accountSetting":"keep","mcpServers":{"example":{"type":"http","url":"https://initial.example/mcp","headers":{"Authorization":secret}}}})).unwrap()).unwrap();
        }
        roots
    }
    let t = tempfile::tempdir().unwrap();
    let cloud = t.path().join("cloud");
    let ah = t.path().join("mac-a");
    let bh = t.path().join("mac-b");
    let ar = fixture(&ah, "A_ONLY_SECRET");
    let br = fixture(&bh, "B_ONLY_SECRET");
    let ready = |_| Ok(true);
    let mut an = NativeAdapter::new(ar.clone(), ah.join("local"), BTreeMap::new(), &ready);
    let mut bn = NativeAdapter::new(br.clone(), bh.join("local"), BTreeMap::new(), &ready);
    let (mut a, mut b) = (State::default(), State::default());
    let ap = ah.join("state");
    let bp = bh.join("state");
    let source = an
        .inventory()
        .unwrap()
        .into_iter()
        .find(|c| c.source == Target::Codex && c.name == "example")
        .unwrap();
    select(
        &cloud,
        &ap,
        &mut a,
        &source,
        BTreeSet::from([Target::Codex, Target::ClaudeCode]),
    )
    .unwrap();
    assert_eq!(
        run(&cloud, &ap, &mut a, &kinds(), &mut an)
            .unwrap()
            .conflicts,
        0
    );
    assert_eq!(
        run(&cloud, &bp, &mut b, &kinds(), &mut bn)
            .unwrap()
            .conflicts,
        0
    );
    let changed = bh.join(".claude.json");
    let bytes = std::fs::read_to_string(&changed).unwrap();
    std::fs::write(changed, bytes.replace("initial.example", "edited.example")).unwrap();
    assert_eq!(
        run(&cloud, &bp, &mut b, &kinds(), &mut bn)
            .unwrap()
            .published,
        1
    );
    let guarded = run(&cloud, &ap, &mut a, &kinds(), &mut an).unwrap();
    assert_eq!(guarded.conflicts, 0);
    assert!(
        guarded.items[0]
            .destinations
            .iter()
            .all(|d| d.status == InstallStatus::NeedsSetup)
    );
    for root in &ar.codex_homes {
        let value = std::fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(value.contains("initial.example") && value.contains("A_ONLY_SECRET"));
    }
    // A changed public endpoint must never receive the destination's existing
    // credential automatically. Model explicit local setup in each account.
    for roots in [&ar, &br] {
        for root in &roots.codex_homes {
            let path = root.join("config.toml");
            let value = std::fs::read_to_string(&path).unwrap();
            std::fs::write(path, value.replace("initial.example", "edited.example")).unwrap();
        }
        for root in &roots.claude_homes {
            let path = if root == &roots.user_home.join(".claude") {
                roots.user_home.join(".claude.json")
            } else {
                root.join(".claude.json")
            };
            let value = std::fs::read_to_string(&path).unwrap();
            std::fs::write(path, value.replace("initial.example", "edited.example")).unwrap();
        }
    }
    assert_eq!(
        run(&cloud, &ap, &mut a, &kinds(), &mut an)
            .unwrap()
            .conflicts,
        0
    );
    assert_eq!(
        run(&cloud, &bp, &mut b, &kinds(), &mut bn)
            .unwrap()
            .conflicts,
        0
    );
    assert_eq!(
        run(&cloud, &ap, &mut a, &kinds(), &mut an)
            .unwrap()
            .published,
        0
    );
    assert_eq!(
        run(&cloud, &bp, &mut b, &kinds(), &mut bn)
            .unwrap()
            .published,
        0
    );
    for (roots, secret) in [(ar, "A_ONLY_SECRET"), (br, "B_ONLY_SECRET")] {
        for root in roots.codex_homes {
            let value = std::fs::read_to_string(root.join("config.toml")).unwrap();
            assert!(
                value.contains(secret)
                    && value.contains("edited.example")
                    && value.contains("# retained comment")
            );
        }
        for root in roots.claude_homes {
            let path = if root == roots.user_home.join(".claude") {
                roots.user_home.join(".claude.json")
            } else {
                root.join(".claude.json")
            };
            let value = std::fs::read_to_string(path).unwrap();
            assert!(
                value.contains(secret)
                    && value.contains("edited.example")
                    && value.contains("accountSetting")
            );
        }
    }
    let archive = Archive::load(&cloud).unwrap();
    for package in archive.revisions.values() {
        let json = serde_json::to_string(package).unwrap();
        assert!(!json.contains("ONLY_SECRET"));
        assert!(!json.contains(ah.to_str().unwrap()));
        assert!(!json.contains(bh.to_str().unwrap()));
    }
}
