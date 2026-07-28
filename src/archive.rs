use crate::config::AppConfig;
use crate::daily_report;
use crate::logger;
use anyhow::{Context, Result, ensure};
use ashe_archive_crypto::{
    ARCHIVE_FILENAME, RecipientMaterial, encrypt_archive_file, inspect_archive, pack_day,
    sha256_file,
};
use chrono::{Days, Local, NaiveDate};
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const UPLOAD_STATE_FILE: &str = "archive-upload-state.json";

pub struct ArchiveHandle {
    tx: Option<Sender<()>>,
}

impl ArchiveHandle {
    pub fn spawn(config: AppConfig) -> Self {
        if config.archive_recipient_json.trim().is_empty() {
            logger::info("Archive sealing disabled: no compiled or configured recipient");
            return Self { tx: None };
        }
        let recipient =
            match serde_json::from_str::<RecipientMaterial>(config.archive_recipient_json.trim())
                .context("archive recipient JSON is invalid")
                .and_then(|recipient| {
                    recipient.validate_public_material()?;
                    Ok(recipient)
                }) {
                Ok(recipient) => recipient,
                Err(error) => {
                    logger::info(format!("Archive sealing disabled: {error:#}"));
                    return Self { tx: None };
                }
            };
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || run(config, recipient, rx));
        Self { tx: Some(tx) }
    }
}

impl Drop for ArchiveHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
struct UploadState {
    #[serde(default)]
    uploaded: BTreeMap<String, UploadedArchive>,
}

#[derive(Deserialize, Serialize)]
struct UploadedArchive {
    day: String,
    acknowledged_at: u64,
}

fn run(config: AppConfig, recipient: RecipientMaterial, stop: Receiver<()>) {
    let interval = Duration::from_secs(config.archive_scan_minutes.saturating_mul(60));
    loop {
        if let Err(error) = scan_and_seal(&config, &recipient) {
            logger::info(format!("Archive sealing scan failed: {error:#}"));
        }
        if let Err(error) = upload_archives(&config) {
            logger::info(format!("Archive upload scan failed: {error:#}"));
        }
        match stop.recv_timeout(interval) {
            Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn scan_and_seal(config: &AppConfig, recipient: &RecipientMaterial) -> Result<()> {
    let today = Local::now().date_naive();
    let oldest_plaintext = today
        .checked_sub_days(Days::new(config.archive_plaintext_days - 1))
        .context("archive plaintext-day threshold overflow")?;
    let mut days = dated_directories(&config.journal_artifacts_dir)?;
    days.sort_by_key(|(day, _)| *day);
    for (day, directory) in days {
        if day >= oldest_plaintext {
            continue;
        }
        let _write_guard = crate::artifact_store::lock();
        let archive = directory.join(ARCHIVE_FILENAME);
        if archive.is_file() {
            cleanup_sealed_day(&directory)?;
            continue;
        }
        let day_text = day.format("%Y-%m-%d").to_string();
        if has_pending_block(&config.journal_artifacts_dir, &day_text) {
            logger::info(format!(
                "Archive sealing deferred for {day_text}: pending block"
            ));
            continue;
        }
        if config.daily_report_enabled && !directory.join("daily.md").is_file() {
            logger::info(format!(
                "Archive sealing deferred for {day_text}: daily.md missing"
            ));
            continue;
        }
        if config.daily_report_enabled && !daily_report::report_is_current(config, &day_text) {
            logger::info(format!(
                "Archive sealing deferred for {day_text}: daily.md is stale"
            ));
            continue;
        }
        let bundle = match pack_day(&directory, &day_text) {
            Ok(bundle) => bundle,
            Err(error) => {
                logger::info(format!(
                    "Archive sealing deferred for {day_text}: {error:#}"
                ));
                continue;
            }
        };
        let temporary = directory.join(format!("{ARCHIVE_FILENAME}.tmp"));
        if temporary.exists() {
            fs::remove_file(&temporary).with_context(|| {
                format!(
                    "failed to remove stale archive temporary {}",
                    temporary.display()
                )
            })?;
        }
        let metadata = encrypt_archive_file(&day_text, &bundle.payload, recipient, &temporary)?;
        fs::rename(&temporary, &archive)
            .with_context(|| format!("failed to commit {}", archive.display()))?;
        cleanup_paths(&bundle.source_paths)?;
        cleanup_sealed_day(&directory)?;
        logger::info(format!(
            "Sealed archive day={} path={} bytes={} sha256={}",
            day_text,
            archive.display(),
            metadata.size,
            metadata.sha256
        ));
    }
    Ok(())
}

fn upload_archives(config: &AppConfig) -> Result<()> {
    let upload_url = config.archive_upload_url.trim().trim_end_matches('/');
    let upload_token = config.archive_upload_token.trim();
    if upload_url.is_empty() || upload_token.is_empty() {
        return Ok(());
    }
    let parsed_upload_url =
        reqwest::Url::parse(upload_url).context("ASHE_ARCHIVE_UPLOAD_URL is not a valid URL")?;
    ensure!(
        parsed_upload_url.scheme() == "https",
        "ASHE_ARCHIVE_UPLOAD_URL must use HTTPS"
    );
    let mut state = read_upload_state(&config.journal_artifacts_dir);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build archive-upload runtime")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2 * 60))
        .build()
        .context("failed to build archive-upload client")?;
    let mut changed = false;
    for (_, directory) in dated_directories(&config.journal_artifacts_dir)? {
        let path = directory.join(ARCHIVE_FILENAME);
        if !path.is_file() {
            continue;
        }
        let sha256 = sha256_file(&path)?;
        if state.uploaded.contains_key(&sha256) {
            continue;
        }
        let header = inspect_archive(&path)?;
        let bytes = fs::read(&path)?;
        let endpoint = format!("{upload_url}/{sha256}");
        let response = runtime.block_on(
            client
                .put(endpoint)
                .bearer_auth(upload_token)
                .header("content-type", "application/vnd.ashe.archive")
                .header("x-ashe-day", &header.day)
                .body(bytes)
                .send(),
        )?;
        ensure!(
            response.status().is_success(),
            "archive receiver returned HTTP {} for {}",
            response.status(),
            header.day
        );
        state.uploaded.insert(
            sha256.clone(),
            UploadedArchive {
                day: header.day.clone(),
                acknowledged_at: now(),
            },
        );
        changed = true;
        logger::info(format!(
            "Archive receiver acknowledged day={} sha256={sha256}",
            header.day
        ));
    }
    if changed {
        write_upload_state(&config.journal_artifacts_dir, &state)?;
    }
    Ok(())
}

fn dated_directories(root: &Path) -> Result<Vec<(NaiveDate, PathBuf)>> {
    let mut output = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(day) = NaiveDate::parse_from_str(&name, "%Y-%m-%d") {
            output.push((day, entry.path()));
        }
    }
    Ok(output)
}

fn has_pending_block(root: &Path, day: &str) -> bool {
    let Ok(entries) = fs::read_dir(root.join("pending")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        fs::read(entry.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value.get("day").and_then(Value::as_str).map(str::to_owned))
            .as_deref()
            == Some(day)
    })
}

fn cleanup_paths(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("failed to remove sealed artifact {}", path.display()))?;
        }
    }
    Ok(())
}

fn cleanup_sealed_day(directory: &Path) -> Result<()> {
    ensure!(
        directory.join(ARCHIVE_FILENAME).is_file(),
        "refusing to clean an unsealed day"
    );
    remove_plaintext_tree(directory, directory)?;
    for images in ["frames", "keyframes"] {
        let path = directory.join(images);
        if path.exists() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("failed to purge {}", path.display()))?;
        }
    }
    remove_empty_subdirectories(directory, directory)?;
    Ok(())
}

fn remove_plaintext_tree(root: &Path, directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .context("sealed artifact escaped its day")?;
        let top = relative
            .components()
            .next()
            .and_then(|part| part.as_os_str().to_str());
        if matches!(top, Some("frames" | "keyframes")) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "sealed day contains a symlink"
        );
        if metadata.is_dir() {
            remove_plaintext_tree(root, &path)?;
        } else if metadata.is_file()
            && path.file_name().and_then(|name| name.to_str()) != Some(ARCHIVE_FILENAME)
        {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn remove_empty_subdirectories(root: &Path, directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            remove_empty_subdirectories(root, &path)?;
            if path != root && fs::read_dir(&path)?.next().is_none() {
                fs::remove_dir(&path)?;
            }
        }
    }
    Ok(())
}

fn read_upload_state(root: &Path) -> UploadState {
    fs::read(root.join(UPLOAD_STATE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_upload_state(root: &Path, state: &UploadState) -> Result<()> {
    let path = root.join(UPLOAD_STATE_FILE);
    let temporary = root.join(format!("{UPLOAD_STATE_FILE}.tmp"));
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&temporary, &path)?;
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::cleanup_sealed_day;
    use ashe_archive_crypto::ARCHIVE_FILENAME;
    use std::fs;

    #[test]
    fn sealed_day_cleanup_removes_plaintext_and_images_but_keeps_archive() {
        let root = tempfile_directory("sealed-cleanup");
        fs::create_dir_all(root.join("blocks")).unwrap();
        fs::create_dir_all(root.join("learning")).unwrap();
        fs::create_dir_all(root.join("keyframes")).unwrap();
        fs::write(root.join(ARCHIVE_FILENAME), "ciphertext").unwrap();
        fs::write(root.join("journal.md"), "plaintext").unwrap();
        fs::write(root.join("blocks/1200.json"), "plaintext").unwrap();
        fs::write(root.join("learning/1200.json"), "plaintext").unwrap();
        fs::write(root.join("keyframes/1200.webp"), "image").unwrap();
        cleanup_sealed_day(&root).unwrap();
        assert!(root.join(ARCHIVE_FILENAME).is_file());
        assert!(!root.join("journal.md").exists());
        assert!(!root.join("blocks").exists());
        assert!(!root.join("learning").exists());
        assert!(!root.join("keyframes").exists());
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
