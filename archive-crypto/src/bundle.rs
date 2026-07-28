use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

const BUNDLE_SCHEMA_VERSION: u32 = 1;
const MANIFEST_PATH: &str = "ASHE-MANIFEST.json";
const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_UNPACKED_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug)]
pub struct Bundle {
    pub payload: Vec<u8>,
    pub source_paths: Vec<PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BundleManifest {
    schema_version: u32,
    day: String,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    path: String,
    size: usize,
    sha256: String,
}

pub fn pack_day(day_dir: &Path, day: &str) -> Result<Bundle> {
    let mut paths = Vec::new();
    collect_files(day_dir, day_dir, &mut paths)?;
    paths.sort_by(|left, right| left.0.cmp(&right.0));
    ensure!(
        !paths.is_empty(),
        "day has no non-image artifacts to archive"
    );
    ensure!(
        paths.iter().all(|(relative, _)| relative != MANIFEST_PATH),
        "day contains the reserved archive manifest path"
    );

    let mut total = 0usize;
    let mut files = Vec::with_capacity(paths.len());
    for (relative, absolute) in &paths {
        let bytes =
            fs::read(absolute).with_context(|| format!("failed to read {}", absolute.display()))?;
        total = total
            .checked_add(bytes.len())
            .context("day artifact size overflow")?;
        ensure!(
            total <= MAX_SOURCE_BYTES,
            "day artifacts exceed the supported size"
        );
        files.push((relative.clone(), bytes));
    }

    let manifest = BundleManifest {
        schema_version: BUNDLE_SCHEMA_VERSION,
        day: day.to_string(),
        files: files
            .iter()
            .map(|(path, bytes)| ManifestFile {
                path: path.clone(),
                size: bytes.len(),
                sha256: sha256(bytes),
            })
            .collect(),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        for (path, bytes) in &files {
            append_tar_file(&mut builder, path, bytes)?;
        }
        append_tar_file(&mut builder, MANIFEST_PATH, &manifest_bytes)?;
        builder.finish()?;
    }
    let payload = zstd::stream::encode_all(Cursor::new(tar_bytes), 3)
        .context("failed to compress day archive")?;
    Ok(Bundle {
        payload,
        source_paths: paths.into_iter().map(|(_, absolute)| absolute).collect(),
    })
}

pub fn extract_bundle(payload: &[u8], output: &Path) -> Result<String> {
    ensure!(
        !output.exists(),
        "refusing to extract over an existing path"
    );
    let mut decoder = zstd::stream::read::Decoder::new(Cursor::new(payload))
        .context("archive payload is not valid zstd")?;
    let mut tar_bytes = Vec::new();
    decoder
        .by_ref()
        .take((MAX_UNPACKED_BYTES + 1) as u64)
        .read_to_end(&mut tar_bytes)?;
    ensure!(
        tar_bytes.len() <= MAX_UNPACKED_BYTES,
        "archive expands beyond the supported size"
    );

    let mut stored = BTreeMap::new();
    let mut archive = tar::Archive::new(Cursor::new(tar_bytes));
    for entry in archive
        .entries()
        .context("archive tar payload is invalid")?
    {
        let mut entry = entry?;
        ensure!(
            entry.header().entry_type().is_file(),
            "archive contains a non-file entry"
        );
        let path = safe_relative_path(&entry.path()?)?;
        ensure!(
            stored.insert(path.clone(), Vec::new()).is_none(),
            "archive contains duplicate paths"
        );
        let bytes = stored.get_mut(&path).expect("inserted archive entry");
        entry
            .by_ref()
            .take((MAX_SOURCE_BYTES + 1) as u64)
            .read_to_end(bytes)?;
        ensure!(
            bytes.len() <= MAX_SOURCE_BYTES,
            "archive entry is too large"
        );
    }
    let manifest_bytes = stored
        .remove(MANIFEST_PATH)
        .context("archive manifest is missing")?;
    let manifest: BundleManifest =
        serde_json::from_slice(&manifest_bytes).context("archive manifest is invalid")?;
    ensure!(
        manifest.schema_version == BUNDLE_SCHEMA_VERSION,
        "unsupported bundle manifest version"
    );
    let expected = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    ensure!(
        expected.len() == manifest.files.len(),
        "archive manifest has duplicate paths"
    );
    ensure!(
        expected == stored.keys().cloned().collect(),
        "archive manifest does not match its files"
    );
    for file in &manifest.files {
        safe_relative_string(&file.path)?;
        let bytes = stored
            .get(&file.path)
            .expect("manifest set matches stored paths");
        ensure!(bytes.len() == file.size, "archive file size mismatch");
        ensure!(
            sha256(bytes) == file.sha256,
            "archive file checksum mismatch"
        );
    }

    fs::create_dir(output).with_context(|| format!("failed to create {}", output.display()))?;
    for (path, bytes) in stored {
        let target = output.join(Path::new(&path));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, bytes)
            .with_context(|| format!("failed to write {}", target.display()))?;
    }
    Ok(manifest.day)
}

fn collect_files(root: &Path, directory: &Path, output: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "day artifacts may not contain symlinks"
        );
        let relative_path = path
            .strip_prefix(root)
            .context("artifact escaped its day directory")?;
        let relative = safe_relative_path(relative_path)?;
        let top = relative.split('/').next().unwrap_or_default();
        if matches!(top, "frames" | "keyframes") {
            continue;
        }
        if metadata.is_dir() {
            collect_files(root, &path, output)?;
        } else if metadata.is_file()
            && relative != super::ARCHIVE_FILENAME
            && !relative.ends_with(".tmp")
        {
            output.push((relative, path));
        }
    }
    Ok(())
}

fn append_tar_file(
    builder: &mut tar::Builder<&mut Vec<u8>>,
    path: &str,
    bytes: &[u8],
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_cksum();
    builder.append_data(&mut header, path, Cursor::new(bytes))?;
    Ok(())
}

fn safe_relative_path(path: &Path) -> Result<String> {
    let mut pieces = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(piece) => {
                let piece = piece
                    .to_str()
                    .ok_or_else(|| anyhow!("archive path is not valid UTF-8"))?;
                ensure!(
                    !piece.is_empty(),
                    "archive path contains an empty component"
                );
                pieces.push(piece);
            }
            _ => return Err(anyhow!("archive path is not safely relative")),
        }
    }
    ensure!(!pieces.is_empty(), "archive path is empty");
    Ok(pieces.join("/"))
}

fn safe_relative_string(path: &str) -> Result<()> {
    ensure!(
        safe_relative_path(Path::new(path))? == path,
        "archive path is not canonical"
    );
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{extract_bundle, pack_day};
    use std::fs;

    #[test]
    fn bundle_excludes_images_and_round_trips_text_artifacts() {
        let root = tempfile_directory("bundle");
        let day = root.join("2026-07-25");
        fs::create_dir_all(day.join("blocks")).unwrap();
        fs::create_dir_all(day.join("learning")).unwrap();
        fs::create_dir_all(day.join("keyframes")).unwrap();
        fs::write(day.join("journal.md"), "journal").unwrap();
        fs::write(day.join("blocks/1200-1210.json"), "{}").unwrap();
        fs::write(day.join("learning/1200-1210.json"), "learning").unwrap();
        fs::write(day.join("keyframes/1200.webp"), "image").unwrap();
        let bundle = pack_day(&day, "2026-07-25").unwrap();
        assert_eq!(bundle.source_paths.len(), 3);
        let restored = root.join("restored");
        let restored_day = extract_bundle(&bundle.payload, &restored).unwrap();
        assert_eq!(restored_day, "2026-07-25");
        assert_eq!(
            fs::read_to_string(restored.join("journal.md")).unwrap(),
            "journal"
        );
        assert_eq!(
            fs::read_to_string(restored.join("learning/1200-1210.json")).unwrap(),
            "learning"
        );
        assert!(!restored.join("keyframes").exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn tempfile_directory(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ashe-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        path
    }
}
