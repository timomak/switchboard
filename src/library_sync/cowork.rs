//! A reproducible Claude plugin. Export and explicit user confirmation are distinct.
use super::{
    engine, mcp,
    model::{Content, Target},
    skills, storage,
};
use crate::Result;
use base64::{Engine, prelude::BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    exported_version: Option<String>,
    confirmed_version: Option<String>,
    zip_path: Option<PathBuf>,
    zip_digest: Option<String>,
    skipped: usize,
    #[serde(default)]
    library_revision_count: usize,
    #[serde(default)]
    outdated: bool,
}
fn receipt_path(local: &Path) -> PathBuf {
    local.join("cowork.json")
}
fn load(local: &Path) -> Result<Receipt> {
    let path = receipt_path(local);
    if !path.try_exists()? {
        return Ok(Receipt::default());
    }
    serde_json::from_slice(&storage::read_regular(&path, 64 * 1024)?)
        .map_err(|_| storage::error("The local Cowork export receipt is unreadable."))
}
pub fn status(local: &Path) -> Result<Value> {
    let receipt = load(local)?;
    let (state, detail) = if receipt.exported_version.is_none() {
        (
            "install_required",
            "Export a plugin, then import it in Claude Customize for each account.",
        )
    } else if receipt.outdated {
        (
            "update_available",
            "The shared library changed. Export and import a replacement plugin for each Claude account.",
        )
    } else if receipt.exported_version == receipt.confirmed_version {
        (
            "confirmed",
            "You confirmed this export was imported. Switchboard has not verified the active Claude account or plugin version.",
        )
    } else if receipt.confirmed_version.is_some() {
        (
            "update_available",
            "A newer plugin export is ready. Import it in Claude Customize for each account.",
        )
    } else {
        (
            "install_required",
            "Import the exported ZIP in Claude Customize. Sign into MCP services separately when prompted.",
        )
    };
    Ok(
        json!({"state":state,"detail":format!("{detail}{}",if receipt.skipped > 0 {format!(" {} selected items require setup and were excluded.",receipt.skipped)} else {String::new()}),"zip_path":receipt.zip_path,"exported_version":receipt.exported_version,"installed_version":receipt.confirmed_version}),
    )
}
pub fn confirm(local: &Path, version: &str) -> Result<()> {
    let mut receipt = load(local)?;
    if receipt.outdated || receipt.exported_version.as_deref() != Some(version) {
        return Err(storage::error(
            "Export the current Cowork plugin before confirming its import.",
        ));
    }
    verify_export(&receipt)?;
    receipt.confirmed_version = Some(version.to_owned());
    storage::atomic_write(&receipt_path(local), &serde_json::to_vec(&receipt)?)
}
fn verify_export(receipt: &Receipt) -> Result<()> {
    let path = receipt
        .zip_path
        .as_ref()
        .ok_or_else(|| storage::error("Export the Cowork plugin first."))?;
    let bytes = storage::read_regular(path, 128 * 1024 * 1024)?;
    if receipt.zip_digest.as_deref() != Some(storage::digest(&bytes).as_str()) {
        return Err(storage::error(
            "The exported ZIP changed. Export it again before importing.",
        ));
    }
    Ok(())
}

pub fn check_updates(cloud: &Path, local: &Path) -> Result<()> {
    let mut receipt = load(local)?;
    if receipt.exported_version.is_none() {
        return Ok(());
    }
    let archive = storage::Archive::load(cloud)?;
    let outdated = archive.pending > 0 || archive.revisions.len() != receipt.library_revision_count;
    if outdated != receipt.outdated {
        receipt.outdated = outdated;
        storage::atomic_write(&receipt_path(local), &serde_json::to_vec(&receipt)?)?;
    }
    Ok(())
}

pub fn export(cloud: &Path, local: &Path) -> Result<()> {
    let archive = storage::Archive::load(cloud)?;
    if archive.pending > 0 {
        return Err(storage::error(
            "Wait for all library revisions to download before exporting Cowork.",
        ));
    }
    let items = engine::list(cloud)?;
    let mut files: BTreeMap<String, (Vec<u8>, bool)> = BTreeMap::new();
    let mut servers = serde_json::Map::new();
    let mut names = std::collections::BTreeSet::new();
    let mut selected = 0;
    for entry in items {
        if !entry.conflicts.is_empty()
            && (entry.item.targets.contains(&Target::Cowork)
                || entry
                    .conflicts
                    .iter()
                    .any(|h| archive.revisions[h].item.targets.contains(&Target::Cowork)))
        {
            return Err(storage::error(
                "Resolve Cowork item conflicts before exporting a replacement plugin.",
            ));
        }
        let item = entry.item;
        if !item.active() || !item.targets.contains(&Target::Cowork) {
            continue;
        }
        if !skills::safe_name(&item.name)
            || !names.insert((item.content.kind(), item.name.to_lowercase()))
        {
            return Err(storage::error(
                "Resolve duplicate or unsupported Cowork item names before exporting.",
            ));
        }
        match &item.content {
            Content::Skill { files: skill_files } => {
                let requirements = skills::requirements(&item.content, Target::Cowork)?;
                if requirements
                    .iter()
                    .any(|r| !r.starts_with("Supporting scripts"))
                {
                    return Err(storage::error(
                        "A selected Cowork skill needs compatibility review. Fix it or remove its Cowork destination before replacing the plugin; the previous export is kept.",
                    ));
                }
                for file in skill_files {
                    let data = BASE64_STANDARD
                        .decode(&file.content_base64)
                        .map_err(|_| storage::error("A selected skill has invalid content."))?;
                    files.insert(
                        format!("skills/{}/{}", item.name, file.path),
                        (data, file.executable),
                    );
                }
                selected += 1;
            }
            Content::Mcp { definition } => match mcp::cowork_definition(definition) {
                Ok(server) => {
                    servers.insert(item.name, server);
                    selected += 1;
                }
                Err(_) => {
                    return Err(storage::error(
                        "A selected Cowork MCP setup needs local bindings or an unsupported runtime. Remove its Cowork destination before exporting; the previous export is kept.",
                    ));
                }
            },
        }
    }
    // Empty exports are intentional when the last selected item was disabled:
    // importing the replacement removes that content from the account plugin.
    if !servers.is_empty() {
        files.insert(
            ".mcp.json".into(),
            (
                serde_json::to_vec_pretty(&json!({"mcpServers":servers}))?,
                false,
            ),
        );
    }
    let number = archive.revisions.len();
    let version = storage::digest(&serde_json::to_vec(&(number, &files))?);
    files.insert(".claude-plugin/plugin.json".into(),(serde_json::to_vec_pretty(&json!({"name":"switchboard-personal-library","version":format!("1.0.{number}"),"author":{"name":"Switchboard"},"description":"Selected personal skills and MCP setups synced by Switchboard."}))?,false));
    files.insert("README.md".into(),(format!("# Switchboard personal library\n\nVersion: {version}\n\n{selected} selected compatible items.\n\nImport this ZIP through Claude Customize for each account. Credentials are not included. Authenticate supported MCP connections in Claude when prompted.\n").into_bytes(),false));
    let bytes = zip(&files)?;
    let path = local.join("cowork-exports").join(format!(
        "switchboard-personal-library-{}.zip",
        &version[..16]
    ));
    storage::atomic_write(&path, &bytes)?;
    let mut receipt = load(local)?;
    receipt.exported_version = Some(version);
    receipt.zip_path = Some(path);
    receipt.zip_digest = Some(storage::digest(&bytes));
    receipt.skipped = 0;
    receipt.library_revision_count = number;
    receipt.outdated = false;
    storage::atomic_write(&receipt_path(local), &serde_json::to_vec(&receipt)?)
}

// ZIP method 0, fixed 1980 timestamp, UTF-8 names and Unix executable bits.
// No external command executes files from a selected skill while packaging.
fn zip(files: &BTreeMap<String, (Vec<u8>, bool)>) -> Result<Vec<u8>> {
    fn u16le(out: &mut Vec<u8>, n: u16) {
        out.extend(n.to_le_bytes());
    }
    fn u32le(out: &mut Vec<u8>, n: u32) {
        out.extend(n.to_le_bytes());
    }
    fn crc(bytes: &[u8]) -> u32 {
        let mut value = !0u32;
        for byte in bytes {
            value ^= *byte as u32;
            for _ in 0..8 {
                value = (value >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(value & 1));
            }
        }
        !value
    }
    let mut out = Vec::new();
    let mut central = Vec::new();
    if files.len() > u16::MAX as usize {
        return Err(storage::error("Too many files for the Cowork plugin."));
    }
    for (path, (data, executable)) in files {
        skills::relative(path)?;
        if path.len() > u16::MAX as usize
            || data.len() > u32::MAX as usize
            || out.len() > 128 * 1024 * 1024
        {
            return Err(storage::error(
                "The Cowork plugin exceeds the supported package size.",
            ));
        }
        let start = out.len() as u32;
        let checksum = crc(data);
        u32le(&mut out, 0x04034b50);
        for n in [20, 0x800, 0, 0, 33] {
            u16le(&mut out, n);
        }
        for n in [checksum, data.len() as u32, data.len() as u32] {
            u32le(&mut out, n);
        }
        u16le(&mut out, path.len() as u16);
        u16le(&mut out, 0);
        out.extend(path.as_bytes());
        out.extend(data);
        u32le(&mut central, 0x02014b50);
        for n in [0x314, 20, 0x800, 0, 0, 33] {
            u16le(&mut central, n);
        }
        for n in [checksum, data.len() as u32, data.len() as u32] {
            u32le(&mut central, n);
        }
        for n in [path.len() as u16, 0, 0, 0, 0] {
            u16le(&mut central, n);
        }
        u32le(
            &mut central,
            (if *executable { 0o100755 } else { 0o100644 }) << 16,
        );
        u32le(&mut central, start);
        central.extend(path.as_bytes());
    }
    let start = out.len() as u32;
    let length = central.len() as u32;
    out.extend(central);
    u32le(&mut out, 0x06054b50);
    for n in [0, 0, files.len() as u16, files.len() as u16] {
        u16le(&mut out, n);
    }
    u32le(&mut out, length);
    u32le(&mut out, start);
    u16le(&mut out, 0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incompatible_revision_keeps_previous_export_and_marks_it_outdated() {
        use super::super::model::{Content, LibraryItem};
        let temp = tempfile::tempdir().unwrap();
        let cloud = temp.path().join("cloud");
        export(&cloud, temp.path()).unwrap();
        let original = load(temp.path()).unwrap();
        let path = original.zip_path.unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let mut archive = storage::Archive::load(&cloud).unwrap();
        archive
            .publish(
                &cloud,
                LibraryItem {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: "private-endpoint".into(),
                    targets: std::collections::BTreeSet::from([Target::Cowork]),
                    enabled: true,
                    deleted: false,
                    content: Content::Mcp {
                        definition: mcp::normalize(
                            &json!({"url":"https://example.com/mcp?key=PRIVATE"}),
                            Target::Codex,
                        )
                        .unwrap(),
                    },
                    requirements: vec![],
                },
                vec![],
            )
            .unwrap();
        check_updates(&cloud, temp.path()).unwrap();
        assert_eq!(status(temp.path()).unwrap()["state"], "update_available");
        assert!(export(&cloud, temp.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(confirm(temp.path(), &original.exported_version.unwrap()).is_err());
    }
    #[test]
    fn zip_is_deterministic_and_preserves_scripts_for_native_import() {
        let files = BTreeMap::from([
            (
                "skills/example/SKILL.md".into(),
                (
                    b"---\nname: example\ndescription: Test\n---\nUse this skill.".to_vec(),
                    false,
                ),
            ),
            (
                "skills/example/scripts/run.sh".into(),
                (b"#!/bin/sh\necho example\n".to_vec(), true),
            ),
        ]);
        let bytes = zip(&files).unwrap();
        assert_eq!(bytes, zip(&files).unwrap());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("plugin.zip");
        std::fs::write(&path, &bytes).unwrap();
        let result = std::process::Command::new("/usr/bin/unzip")
            .args(["-tq"])
            .arg(path)
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(bytes.windows(4).filter(|w| *w == b"PK\x03\x04").count(), 2);
    }
    #[test]
    fn confirmation_requires_exact_export_and_never_claims_verified_install() {
        let temp = tempfile::tempdir().unwrap();
        assert!(confirm(temp.path(), "not-exported").is_err());
        export(&temp.path().join("cloud"), temp.path()).unwrap();
        let current = load(temp.path()).unwrap();
        let version = current.exported_version.unwrap();
        confirm(temp.path(), &version).unwrap();
        assert_eq!(status(temp.path()).unwrap()["state"], "confirmed");
        std::fs::write(current.zip_path.unwrap(), b"changed").unwrap();
        assert!(confirm(temp.path(), &version).is_err());
    }
}
