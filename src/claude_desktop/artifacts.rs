//! Best-effort, private snapshots of local artifact sources. No rendering,
//! transcript edits, cloud publishing, or account-specific monitor restoration.
use super::Paths;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const FILE_LIMIT: u64 = 16 * 1024 * 1024;
const STORE_LIMIT: u64 = 256 * 1024 * 1024;
const TAIL_LIMIT: u64 = 8 * 1024 * 1024;
const SCAN_LIMIT: u64 = 64 * 1024 * 1024;
const ENTRY_LIMIT: usize = 4096;

#[derive(Deserialize)]
struct Frame {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "sessionId")]
    session_id: String,
    path: PathBuf,
    #[serde(rename = "frameUrl")]
    url: String,
}

fn is_artifact_url(url: &str) -> bool {
    if let Some(id) = url.strip_prefix("https://claude.ai/code/artifact/") {
        return uuid::Uuid::parse_str(id).is_ok();
    }
    // Current native frame-link records use a short, opaque sharing token;
    // it is not the artifact UUID stored in tool results and monitor state.
    // Accept the observed 22-character token on the exact HTTPS host. This
    // does not make a network request or change the artifact's permissions.
    url.strip_prefix("https://claude.ai/artifact/")
        .is_some_and(|token| token.len() == 22 && token.bytes().all(|b| b.is_ascii_alphanumeric()))
}

// Never follow directory symlinks while discovering indexes/transcripts/copies.
fn children(path: &Path) -> Vec<PathBuf> {
    if !fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        return Vec::new();
    }
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .take(ENTRY_LIMIT)
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect()
}

fn regular(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let file = File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    #[cfg(not(unix))]
    let file = {
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(io::Error::other("symlink"));
        }
        File::open(path)?
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(file)
}

fn read_source(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut file = regular(path)?;
    let before = file.metadata()?;
    if before.len() > limit {
        return Err(io::Error::other("size limit"));
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 != before.len()
        || before.len() != after.len()
        || before.modified()? != after.modified()?
    {
        return Err(io::Error::other("source changed"));
    }
    Ok(bytes)
}

fn private_dir(path: &Path) -> io::Result<()> {
    if !path.try_exists()? {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)?;
    }
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(io::Error::other("not a directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn store_size(root: &Path) -> io::Result<u64> {
    if !root.try_exists()? {
        return Ok(0);
    }
    let mut size = 0u64;
    // Refuse an unexpectedly shaped or excessively large store; never prune it.
    let entries = children(root);
    if entries.len() >= ENTRY_LIMIT {
        return Err(io::Error::other("entry limit"));
    }
    for entry in entries {
        if !fs::symlink_metadata(&entry)?.is_dir() {
            return Err(io::Error::other("unexpected entry"));
        }
        let files = fs::read_dir(entry)?
            .take(3)
            .collect::<io::Result<Vec<_>>>()?;
        if files.len() > 2 {
            return Err(io::Error::other("unexpected files"));
        }
        for file in files {
            let metadata = fs::symlink_metadata(file.path())?;
            if !metadata.is_file() {
                return Err(io::Error::other("unexpected file"));
            }
            size = size.saturating_add(metadata.len());
        }
    }
    Ok(size)
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn snapshot(root: &Path, frame: &Frame, used: &mut u64) -> io::Result<()> {
    let extension = frame
        .path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if !frame.path.is_absolute() || !matches!(extension, "html" | "htm" | "md") {
        return Ok(());
    }
    let bytes = read_source(&frame.path, FILE_LIMIT)?;
    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Ok(());
    }
    let digest = hash(&bytes);
    let identity = hash(format!("{}\n{}", frame.session_id, frame.path.display()).as_bytes());
    let destination = root.join(format!("{identity}-{digest}"));
    if destination.try_exists()? {
        return Ok(());
    }
    let metadata = serde_json::to_vec_pretty(&serde_json::json!({
        "session_id": frame.session_id,
        "source_path": frame.path,
        "source_url": frame.url,
        "sha256": digest,
        "saved_at": chrono::Utc::now().to_rfc3339(),
    }))?;
    let added = (bytes.len() + metadata.len()) as u64;
    if used.saturating_add(added) > STORE_LIMIT {
        return Ok(());
    }
    private_dir(root)?;
    let staging = tempfile::Builder::new()
        .prefix(".saving-")
        .tempdir_in(root)?;
    for (name, content) in [
        (format!("artifact.{extension}"), bytes),
        ("metadata.json".into(), metadata),
    ] {
        let mut file = tempfile::NamedTempFile::new_in(staging.path())?;
        file.write_all(&content)?;
        file.as_file().sync_all()?;
        file.persist(staging.path().join(name))
            .map_err(|e| e.error)?;
    }
    fs::rename(staging.path(), destination)?;
    *used += added;
    Ok(())
}

pub(super) fn preserve(paths: &Paths) {
    let _ = preserve_inner(paths);
}

fn preserve_inner(paths: &Paths) -> io::Result<()> {
    let started = Instant::now();
    let expired = || started.elapsed() > Duration::from_secs(2);
    let root = paths.backups_dir.join("artifact-copies");
    let mut used = store_size(&root)?;
    if used >= STORE_LIMIT {
        return Ok(());
    }
    let mut sessions = HashSet::new();
    for account in children(&paths.sessions_root()) {
        for org in children(&account) {
            for index in children(&org) {
                if expired() {
                    return Ok(());
                }
                if !index
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("local_"))
                {
                    continue;
                }
                if let Ok(bytes) = read_source(&index, 1024 * 1024)
                    && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                    && let Some(id) = value.get("cliSessionId").and_then(|v| v.as_str())
                    && uuid::Uuid::parse_str(id).is_ok()
                {
                    sessions.insert(id.to_owned());
                }
            }
        }
    }
    let mut scanned = 0;
    for project in children(&paths.claude_code_root.join("projects")) {
        for transcript in children(&project) {
            if expired() || scanned >= SCAN_LIMIT {
                return Ok(());
            }
            let Some(id) = transcript.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if transcript.extension().is_none_or(|e| e != "jsonl") || !sessions.contains(id) {
                continue;
            }
            let Ok(mut file) = regular(&transcript) else {
                continue;
            };
            let length = file.metadata()?.len();
            let start = length.saturating_sub(TAIL_LIMIT.min(SCAN_LIMIT - scanned));
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = Vec::new();
            file.take(length - start).read_to_end(&mut bytes)?;
            scanned += bytes.len() as u64;
            for line in bytes.split(|b| *b == b'\n').skip(usize::from(start > 0)) {
                if expired() {
                    return Ok(());
                }
                let Ok(frame) = serde_json::from_slice::<Frame>(line) else {
                    continue;
                };
                if frame.kind != "frame-link" || frame.session_id != id {
                    continue;
                }
                if !is_artifact_url(&frame.url) {
                    continue;
                }
                // Individual missing/unreadable files must not prevent other snapshots.
                if private_dir(&paths.backups_dir).is_ok() {
                    let _ = snapshot(&root, &frame, &mut used);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const SESSION: &str = "11111111-1111-4111-8111-111111111111";
    const URL: &str = "https://claude.ai/code/artifact/22222222-2222-4222-8222-222222222222";
    const SHORT_URL: &str = "https://claude.ai/artifact/AbCdEf0123456789GhIjKl";

    fn fixture() -> (tempfile::TempDir, Paths, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::at(
            temp.path().join("desktop"),
            temp.path().join("profiles"),
            temp.path().join("backups"),
        );
        let indexes = paths.sessions_root().join("account/org");
        fs::create_dir_all(&indexes).unwrap();
        fs::create_dir_all(&paths.backups_dir).unwrap();
        fs::write(
            indexes.join("local_example.json"),
            serde_json::json!({"cliSessionId": SESSION}).to_string(),
        )
        .unwrap();
        let project = paths.claude_code_root.join("projects/example");
        fs::create_dir_all(&project).unwrap();
        let transcript = project.join(format!("{SESSION}.jsonl"));
        let source = temp.path().join("plan.html");
        fs::write(&source, "<html>Saved plan</html>").unwrap();
        fs::write(&transcript, record(&source)).unwrap();
        (temp, paths, source, transcript)
    }

    fn record(source: &Path) -> String {
        serde_json::json!({"type":"frame-link", "sessionId":SESSION, "path":source, "frameUrl":URL})
            .to_string()
            + "\n"
    }
    fn copies(paths: &Paths) -> Vec<PathBuf> {
        children(&paths.backups_dir.join("artifact-copies"))
    }

    #[test]
    fn saves_content_without_changing_source_or_transcript_and_deduplicates() {
        let (_temp, paths, source, transcript) = fixture();
        let original = fs::read(&transcript).unwrap();
        preserve(&paths);
        preserve(&paths);
        let saved = copies(&paths);
        assert_eq!(saved.len(), 1);
        assert_eq!(
            fs::read(saved[0].join("artifact.html")).unwrap(),
            fs::read(&source).unwrap()
        );
        assert_eq!(fs::read(&transcript).unwrap(), original);
        let metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(saved[0].join("metadata.json")).unwrap()).unwrap();
        assert_eq!(metadata["source_url"], URL);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(saved[0].parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(saved[0].join("artifact.html"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn preserves_native_short_url_frames_without_changing_monitor_state() {
        let (_temp, paths, source, transcript) = fixture();
        let original_source = fs::read(&source).unwrap();
        let frame = serde_json::json!({
            "type": "frame-link",
            "sessionId": SESSION,
            "path": source,
            "frameUrl": SHORT_URL,
            "artifactCount": 1,
            "title": "Sample artifact",
            "timestamp": "2026-01-01T00:00:00Z"
        });
        let monitor = serde_json::json!({
            "type": "artifact-comment-monitor",
            "sessionId": SESSION,
            "v": 1,
            "artifacts": {"22222222-2222-4222-8222-222222222222": {
                "state": "stopped", "writtenAtMs": 1, "title": "Sample artifact"
            }}
        });
        let original = format!("{frame}\n{monitor}\n");
        fs::write(&transcript, &original).unwrap();
        preserve(&paths);
        preserve(&paths);
        let saved = copies(&paths);
        assert_eq!(saved.len(), 1);
        assert_eq!(
            fs::read(saved[0].join("artifact.html")).unwrap(),
            original_source
        );
        assert_eq!(fs::read(&source).unwrap(), original_source);
        assert_eq!(fs::read_to_string(&transcript).unwrap(), original);
        let metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(saved[0].join("metadata.json")).unwrap()).unwrap();
        assert_eq!(metadata["source_url"], SHORT_URL);
    }

    #[test]
    fn short_url_frames_reject_other_origins_and_non_token_suffixes() {
        let (_temp, paths, source, transcript) = fixture();
        for url in [
            SHORT_URL.replace("https://", "http://"),
            SHORT_URL.replace("claude.ai", "claude.ai.example.com"),
            SHORT_URL.replace("claude.ai", "claude.ai@example.com"),
            SHORT_URL.replace("claude.ai", "example.com@claude.ai"),
            "https://claude.ai/artifact/".into(),
            "https://claude.ai/artifact/../private".into(),
            "https://claude.ai/artifact/%2e%2e".into(),
            format!("{SHORT_URL}/extra"),
            format!("{SHORT_URL}?token=secret"),
            format!("{SHORT_URL}#fragment"),
            format!("https://claude.ai/artifact/{}", "a".repeat(21)),
            format!("https://claude.ai/artifact/{}", "a".repeat(23)),
            format!("https://claude.ai/artifact/{}-", "a".repeat(21)),
        ] {
            fs::write(&transcript, record(&source).replace(URL, &url)).unwrap();
            preserve(&paths);
            assert!(copies(&paths).is_empty(), "unexpected snapshot for {url}");
        }
    }

    #[test]
    fn retains_revisions_and_survives_deleted_source() {
        let (_temp, paths, source, _transcript) = fixture();
        preserve(&paths);
        fs::write(&source, "Updated plan").unwrap();
        preserve(&paths);
        fs::remove_file(source).unwrap();
        preserve(&paths);
        assert_eq!(copies(&paths).len(), 2);
        assert!(
            copies(&paths)
                .iter()
                .any(|p| fs::read_to_string(p.join("artifact.html")).unwrap()
                    == "<html>Saved plan</html>")
        );
    }

    #[test]
    fn only_accepts_native_frames_for_known_desktop_sessions() {
        let (_temp, paths, source, transcript) = fixture();
        for text in [
            record(&source).replace("frame-link", "assistant"),
            record(&source).replace(SESSION, "different-session"),
            record(&source).replace("https://claude.ai/", "https://example.com/"),
            record(&source).replace(URL, &format!("{URL}/extra")),
        ] {
            fs::write(&transcript, text).unwrap();
            preserve(&paths);
            assert!(copies(&paths).is_empty());
        }
        fs::write(&transcript, record(&source)).unwrap();
        fs::remove_dir_all(paths.sessions_root()).unwrap();
        preserve(&paths);
        assert!(copies(&paths).is_empty());
    }

    #[test]
    fn scans_recent_frames_in_large_transcripts_and_continues_after_missing_sources() {
        let (_temp, paths, source, transcript) = fixture();
        let mut bytes = vec![b'x'; TAIL_LIMIT as usize + 100];
        bytes.push(b'\n');
        bytes.extend_from_slice(record(&source.with_file_name("missing.html")).as_bytes());
        bytes.extend_from_slice(record(&source).as_bytes());
        fs::write(transcript, bytes).unwrap();
        preserve(&paths);
        assert_eq!(copies(&paths).len(), 1);
    }

    #[test]
    fn oversized_and_binary_sources_are_skipped() {
        let (_temp, paths, source, _transcript) = fixture();
        File::create(&source)
            .unwrap()
            .set_len(FILE_LIMIT + 1)
            .unwrap();
        preserve(&paths);
        assert!(copies(&paths).is_empty());
        fs::write(&source, b"binary\0data").unwrap();
        preserve(&paths);
        assert!(copies(&paths).is_empty());
    }

    #[test]
    fn unavailable_store_is_nonfatal_and_quota_preserves_existing_copies() {
        let (_temp, paths, source, _transcript) = fixture();
        let root = paths.backups_dir.join("artifact-copies");
        fs::write(&root, "occupied").unwrap();
        preserve(&paths);
        assert_eq!(
            fs::read_to_string(&source).unwrap(),
            "<html>Saved plan</html>"
        );
        fs::remove_file(&root).unwrap();
        preserve(&paths);
        let frame: Frame = serde_json::from_str(record(&source).trim()).unwrap();
        fs::write(&source, "New revision").unwrap();
        let mut used = STORE_LIMIT;
        snapshot(&root, &frame, &mut used).unwrap();
        assert_eq!(copies(&paths).len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn source_and_store_symlinks_are_not_followed() {
        use std::os::unix::fs::symlink;
        let (temp, paths, source, transcript) = fixture();
        let link = temp.path().join("linked.html");
        symlink(&source, &link).unwrap();
        fs::write(&transcript, record(&link)).unwrap();
        preserve(&paths);
        assert!(copies(&paths).is_empty());
        fs::write(&transcript, record(&source)).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, paths.backups_dir.join("artifact-copies")).unwrap();
        preserve(&paths);
        assert!(children(&outside).is_empty());
    }
}
