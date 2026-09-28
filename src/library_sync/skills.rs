//! Bounded complete skill folders. No managed plugin or account cache is read.
use super::model::{Content, SkillFile, Target};
use crate::{AppError, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path},
};

pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SKILL_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_FILES: usize = 2048;
pub(crate) fn error(message: &str) -> AppError {
    AppError::Other(format!("Tools & skills: {message}"))
}

pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub(crate) fn relative(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 1024
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(error("an item contains an unsafe relative path"));
    }
    Ok(())
}
fn credential_member(path: &str) -> bool {
    path.split('/').any(|part| {
        let name = part.to_ascii_lowercase();
        matches!(
            name.as_str(),
            ".git"
                | ".svn"
                | ".hg"
                | ".npmrc"
                | ".pypirc"
                | ".netrc"
                | ".credentials.json"
                | "credentials.json"
                | "auth.json"
                | "id_rsa"
                | "id_ed25519"
        ) || name == ".env"
            || name.starts_with(".env.")
            || name.ends_with(".p12")
            || name.ends_with(".pfx")
            || name.ends_with(".pem")
    })
}

pub(crate) fn no_symlink(path: &Path) -> Result<()> {
    let mut current = std::path::PathBuf::new();
    for part in path.components() {
        if matches!(part, Component::ParentDir) {
            return Err(error("parent traversal is not supported"));
        }
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                // macOS exposes its temporary roots through these OS-owned
                // aliases. Every component below the alias is still checked.
                let system_alias = (current == Path::new("/var")
                    && fs::read_link(&current).ok().as_deref() == Some(Path::new("private/var")))
                    || (current == Path::new("/tmp")
                        && fs::read_link(&current).ok().as_deref()
                            == Some(Path::new("private/tmp")));
                if !system_alias {
                    return Err(error(
                        "linked native locations are unsupported; originals were preserved",
                    ));
                }
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(error("could not inspect a native location")),
        }
    }
    Ok(())
}

pub(crate) fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    no_symlink(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = options
        .open(path)
        .map_err(|_| error("could not read a native file"))?;
    let metadata = file
        .metadata()
        .map_err(|_| error("could not inspect a native file"))?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(error(
            "a native file exceeds the supported size or file type",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error("could not read a native file"))?;
    if bytes.len() > limit {
        return Err(error("a native file exceeds the supported size"));
    }
    Ok(bytes)
}

pub fn capture(root: &Path) -> Result<Content> {
    fn visit(
        root: &Path,
        dir: &Path,
        depth: usize,
        files: &mut Vec<SkillFile>,
        bytes: &mut usize,
        count: &mut usize,
    ) -> Result<()> {
        if depth > 32 {
            return Err(error("a skill exceeds the supported folder depth"));
        }
        no_symlink(dir)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(dir).map_err(|_| error("could not inspect a skill folder"))? {
            *count += 1;
            if *count > MAX_FILES {
                return Err(error("a skill has too many files or folders"));
            }
            entries.push(entry.map_err(|_| error("could not inspect a skill folder"))?);
        }
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if files.len() >= MAX_FILES {
                return Err(error("a skill has too many files"));
            }
            let meta = fs::symlink_metadata(entry.path())
                .map_err(|_| error("could not inspect a skill file"))?;
            if meta.is_dir() {
                visit(root, &entry.path(), depth + 1, files, bytes, count)?;
                continue;
            }
            if !meta.is_file() {
                return Err(error("linked or special skill files are unsupported"));
            }
            let path = entry.path();
            let relative_path = path
                .strip_prefix(root)
                .map_err(|_| error("invalid skill path"))?
                .to_str()
                .ok_or_else(|| error("skill filenames must be UTF-8"))?
                .to_owned();
            relative(&relative_path)?;
            if credential_member(&relative_path) {
                return Err(error(
                    "a skill contains credential or version-control files; remove those from the selected skill folder before syncing",
                ));
            }
            if meta.len() > MAX_SKILL_BYTES.saturating_sub(*bytes) as u64 {
                return Err(error("a skill exceeds the supported total size"));
            }
            let data = read(&path, MAX_FILE_BYTES)?;
            *bytes += data.len();
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            files.push(SkillFile {
                path: relative_path,
                content_base64: STANDARD.encode(data),
                executable,
            });
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, 0, &mut files, &mut 0, &mut 0)?;
    validate(&files)?;
    Ok(Content::Skill { files })
}

pub fn validate(files: &[SkillFile]) -> Result<()> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err(error("a skill has an unsupported file count"));
    }
    let mut paths = BTreeSet::new();
    let mut size = 0usize;
    for file in files {
        relative(&file.path)?;
        if credential_member(&file.path) {
            return Err(error(
                "a skill contains credential or version-control files and cannot be published",
            ));
        }
        // Case-insensitive APFS must not collapse two distinct transport members.
        if !paths.insert(file.path.to_lowercase()) {
            return Err(error("a skill has duplicate or case-colliding filenames"));
        }
        if file.content_base64.len() > MAX_FILE_BYTES * 2 {
            return Err(error("a skill file exceeds the supported size"));
        }
        let data = STANDARD
            .decode(&file.content_base64)
            .map_err(|_| error("invalid skill content"))?;
        size = size
            .checked_add(data.len())
            .ok_or_else(|| error("skill size overflow"))?;
        if data.len() > MAX_FILE_BYTES || size > MAX_SKILL_BYTES {
            return Err(error("a skill exceeds the supported size"));
        }
    }
    if !files.iter().any(|f| f.path == "SKILL.md") {
        return Err(error("a skill must contain SKILL.md"));
    }
    for path in &paths {
        let mut parent = Path::new(path).parent();
        while let Some(p) = parent {
            if paths.contains(p.to_string_lossy().as_ref()) {
                return Err(error("a skill file conflicts with a folder"));
            }
            parent = p.parent();
        }
    }
    Ok(())
}

pub fn requirements(content: &Content, target: Target) -> Result<Vec<String>> {
    let Content::Skill { files } = content else {
        return Err(error("expected a skill"));
    };
    validate(files)?;
    let main = files.iter().find(|f| f.path == "SKILL.md").unwrap();
    let bytes = STANDARD
        .decode(&main.content_base64)
        .map_err(|_| error("invalid skill content"))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| error("SKILL.md must be UTF-8"))?;
    let mut requirements = Vec::new();
    if text.contains("/Users/") || text.contains("/home/") || text.contains("file://") {
        requirements.push(
            "This skill refers to a machine-specific path; review those references on this Mac."
                .into(),
        );
    }
    if files.iter().any(|f| {
        f.executable
            || f.path.ends_with(".py")
            || f.path.ends_with(".sh")
            || f.path.ends_with(".js")
    }) {
        requirements.push("Supporting scripts are included; required runtimes and dependencies must be available locally.".into());
    }
    let header = text
        .strip_prefix("---")
        .and_then(|s| s.split_once("---").map(|p| p.0))
        .unwrap_or("");
    if header.contains("allowed-tools:")
        || header.contains("context:")
        || header.contains("agent:")
        || header.contains("hooks:")
        || text.contains("mcp__")
        || (target == Target::Cowork && files.iter().any(|f| f.path == "agents/openai.yaml"))
    {
        requirements.push("This skill uses app-specific tools or metadata; review compatibility for each selected app.".into());
    }
    // Relative Markdown resource links must remain inside the complete folder.
    for segment in text.split("](").skip(1) {
        if let Some(link) = segment.split(')').next() {
            let link = link
                .trim_matches(['<', '>'])
                .split('#')
                .next()
                .unwrap_or("");
            if !link.is_empty()
                && !link.contains("://")
                && !link.starts_with('#')
                && (link.starts_with("../")
                    || link.starts_with('/')
                    || (!link.contains(char::is_whitespace)
                        && !files.iter().any(|f| f.path == link)))
            {
                requirements.push("A referenced resource is outside this skill or missing; provide it separately.".into());
                break;
            }
        }
    }
    Ok(requirements)
}

pub(crate) fn stage(root: &Path, files: &[SkillFile]) -> Result<()> {
    validate(files)?;
    fs::create_dir(root).map_err(|_| error("could not stage a skill"))?;
    for file in files {
        let path = root.join(&file.path);
        fs::create_dir_all(path.parent().unwrap())
            .map_err(|_| error("could not stage skill folders"))?;
        let bytes = STANDARD
            .decode(&file.content_base64)
            .map_err(|_| error("invalid skill content"))?;
        super::native::write_private(&path, &bytes, file.executable)?;
    }
    super::native::sync_tree(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_assets_and_modes_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir(t.path().join("scripts")).unwrap();
        fs::write(
            t.path().join("SKILL.md"),
            "---\nname: sample\n---\nRead [asset](asset.bin).\n",
        )
        .unwrap();
        fs::write(t.path().join("asset.bin"), [0, 255, 1]).unwrap();
        fs::write(t.path().join("scripts/run.sh"), "#!/bin/sh\ntrue\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                t.path().join("scripts/run.sh"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let content = capture(t.path()).unwrap();
        let Content::Skill { files } = content else {
            panic!()
        };
        assert_eq!(files.len(), 3);
        #[cfg(unix)]
        assert!(
            files
                .iter()
                .find(|f| f.path == "scripts/run.sh")
                .unwrap()
                .executable
        );
    }
    #[test]
    fn paths_and_symlinks_fail_closed() {
        let file = SkillFile {
            path: "../escape".into(),
            content_base64: STANDARD.encode("x"),
            executable: false,
        };
        assert!(validate(&[file]).is_err());
        #[cfg(unix)]
        {
            let t = tempfile::tempdir().unwrap();
            fs::write(t.path().join("SKILL.md"), "safe").unwrap();
            std::os::unix::fs::symlink("/etc/hosts", t.path().join("secret")).unwrap();
            assert!(capture(t.path()).is_err());
        }
    }
    #[test]
    fn credential_members_block_the_complete_folder_instead_of_silent_redaction() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("SKILL.md"), "synthetic skill").unwrap();
        fs::write(t.path().join(".env"), "API_KEY=PRIVATE_FIXTURE").unwrap();
        let err = capture(t.path()).unwrap_err().to_string();
        assert!(!err.contains("PRIVATE_FIXTURE"));
        assert!(err.contains("credential"));
    }
}
