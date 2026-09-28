//! Explicitly requested official-client smoke fixture. Ordinary test runs never
//! persist files or launch an installed client.
use super::{
    cowork, engine,
    model::{Kind, Target},
    native::{NativeAdapter, NativeRoots},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

fn roots(home: &Path) -> NativeRoots {
    NativeRoots {
        claude_config_files: vec![],
        user_home: home.into(),
        codex_homes: vec![home.join(".codex")],
        codex_skill_roots: vec![home.join(".agents/skills")],
        claude_homes: vec![home.join(".claude")],
    }
}

#[test]
#[ignore = "Creates isolated native-client fixtures only when explicitly requested"]
fn generate_official_client_smoke_fixture() {
    let output = PathBuf::from(
        std::env::var_os("SWITCHBOARD_LIBRARY_SMOKE_DIR")
            .expect("Set SWITCHBOARD_LIBRARY_SMOKE_DIR to a new /private/tmp directory"),
    );
    assert!(
        output.is_absolute()
            && output.starts_with("/private/tmp")
            && output != Path::new("/private/tmp")
    );
    assert!(
        output
            .components()
            .all(|p| matches!(p, Component::RootDir | Component::Normal(_)))
    );
    assert!(!output.exists(), "Use a fresh fixture directory");
    std::fs::create_dir(&output).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let (a, b) = (output.join("mac-a"), output.join("mac-b"));
    let (ar, br) = (roots(&a), roots(&b));
    for home in [&a, &b] {
        for dir in [".codex", ".claude", ".agents/skills"] {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
    }
    let skill = a.join(".agents/skills/switchboard-smoke");
    std::fs::create_dir_all(skill.join("references")).unwrap();
    std::fs::create_dir_all(skill.join("scripts")).unwrap();
    std::fs::write(skill.join("SKILL.md"),"---\nname: switchboard-smoke\ndescription: Synthetic Switchboard library installation verification.\n---\nRead [the fixture reference](references/proof.txt) when explicitly asked to verify this synthetic skill. The supporting script is a fixture and need not be executed.\n").unwrap();
    std::fs::write(
        skill.join("references/proof.txt"),
        "Switchboard synthetic portable reference.\n",
    )
    .unwrap();
    std::fs::write(
        skill.join("scripts/proof.sh"),
        "#!/bin/sh\nprintf '%s\\n' 'Switchboard synthetic script'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            skill.join("scripts/proof.sh"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    std::fs::write(a.join(".codex/config.toml"),"# Synthetic Switchboard fixture only.\n[mcp_servers.switchboard-smoke-remote]\nurl = 'https://example.com/mcp'\n").unwrap();
    let ready = |_| Ok(true);
    let mut source = NativeAdapter::new(ar, a.join("library-local"), BTreeMap::new(), &ready);
    let mut destination = NativeAdapter::new(br, b.join("library-local"), BTreeMap::new(), &ready);
    let cloud = output.join("cloud");
    let ap = a.join("library-state.json");
    let bp = b.join("library-state.json");
    let (mut ast, mut bst) = (engine::State::default(), engine::State::default());
    let inventory = source.inventory().unwrap();
    assert_eq!(inventory.len(), 2);
    for candidate in inventory {
        engine::select(
            &cloud,
            &ap,
            &mut ast,
            &candidate,
            BTreeSet::from([Target::Codex, Target::ClaudeCode, Target::Cowork]),
        )
        .unwrap();
    }
    let kinds = BTreeSet::from([Kind::Skill, Kind::Mcp]);
    let first = engine::run(&cloud, &ap, &mut ast, &kinds, &mut source).unwrap();
    assert_eq!(first.conflicts, 0);
    let second = engine::run(&cloud, &bp, &mut bst, &kinds, &mut destination).unwrap();
    assert_eq!(second.conflicts, 0);
    let source_again = engine::run(&cloud, &ap, &mut ast, &kinds, &mut source).unwrap();
    let destination_again = engine::run(&cloud, &bp, &mut bst, &kinds, &mut destination).unwrap();
    assert_eq!(
        (source_again.published, destination_again.published),
        (0, 0)
    );
    for (a_item, b_item) in source_again.items.iter().zip(&destination_again.items) {
        assert_eq!(a_item.item, b_item.item);
        assert_eq!(
            a_item
                .destinations
                .iter()
                .map(|d| d.status)
                .collect::<Vec<_>>(),
            b_item
                .destinations
                .iter()
                .map(|d| d.status)
                .collect::<Vec<_>>()
        );
    }
    for relative in [
        ".agents/skills/switchboard-smoke/references/proof.txt",
        ".claude/skills/switchboard-smoke/references/proof.txt",
    ] {
        assert_eq!(
            std::fs::read(b.join(relative)).unwrap(),
            b"Switchboard synthetic portable reference.\n"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            std::fs::metadata(b.join(".claude/skills/switchboard-smoke/scripts/proof.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
    let export_dir = output.join("cowork");
    cowork::export(&cloud, &export_dir).unwrap();
    let report = serde_json::json!({"fixture_root":output,"source_home":a,"destination_home":b,"cowork":cowork::status(&export_dir).unwrap(),"source":source_again,"destination":destination_again});
    std::fs::write(
        output.join("smoke-report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!("{}",serde_json::to_string_pretty(&serde_json::json!({"fixture_root":output,"destination_home":b,"cowork":report["cowork"]})).unwrap());
}
