use crate::config::AppConfig;
use crate::daily_report;
use crate::logger;
use anyhow::{Context, Result, ensure};
use ashe_archive_crypto::{
    ARCHIVE_FILENAME, ArchiveMetadata, RecipientMaterial, encrypt_archive_file, inspect_archive,
    pack_day, sha256_file,
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
        let recipient = match archive_recipient(&config) {
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

#[derive(Debug, Eq, PartialEq)]
pub struct UploadArchiveOutcome {
    pub day: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub enum SealDayOutcome {
    Sealed {
        path: PathBuf,
        metadata: ArchiveMetadata,
    },
    AlreadySealed {
        path: PathBuf,
        sha256: String,
        size: u64,
    },
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
    let mut days = dated_directories(&config.activity_artifacts_dir)?;
    days.sort_by_key(|(day, _)| *day);
    for (day, directory) in days {
        if day >= oldest_plaintext {
            continue;
        }
        let _write_guard = crate::artifact_store::lock(&config.activity_artifacts_dir)?;
        let archive = directory.join(ARCHIVE_FILENAME);
        if archive.is_file() {
            cleanup_sealed_day(&directory)?;
            continue;
        }
        let day_text = day.format("%Y-%m-%d").to_string();
        if has_pending_block(&config.activity_artifacts_dir, &day_text) {
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
        let result = match seal_day_locked(&directory, &day_text, recipient) {
            Ok(result) => result,
            Err(error) => {
                logger::info(format!(
                    "Archive sealing deferred for {day_text}: {error:#}"
                ));
                continue;
            }
        };
        log_seal_outcome(&day_text, &result);
    }
    Ok(())
}

pub fn seal_day_manually(config: &AppConfig, day_text: &str) -> Result<SealDayOutcome> {
    let day = parse_day(day_text)?;
    ensure!(
        day < Local::now().date_naive(),
        "refusing to archive the current or a future day"
    );
    let directory = config.activity_artifacts_dir.join(day_text);
    ensure!(
        directory.is_dir(),
        "artifact day does not exist: {day_text}"
    );
    if directory.join(ARCHIVE_FILENAME).is_file() {
        let _write_guard = crate::artifact_store::lock(&config.activity_artifacts_dir)?;
        return seal_day_locked(&directory, day_text, &archive_recipient(config)?);
    }
    ensure!(
        !has_pending_block(&config.activity_artifacts_dir, day_text),
        "day has a pending activity block"
    );
    if config.daily_report_enabled {
        daily_report::generate_day(config, day_text)
            .with_context(|| format!("failed to generate daily report for {day_text}"))?;
    }
    let recipient = archive_recipient(config)?;
    let _write_guard = crate::artifact_store::lock(&config.activity_artifacts_dir)?;
    if directory.join(ARCHIVE_FILENAME).is_file() {
        return seal_day_locked(&directory, day_text, &recipient);
    }
    ensure!(
        !has_pending_block(&config.activity_artifacts_dir, day_text),
        "day gained a pending activity block while preparing the archive"
    );
    if config.daily_report_enabled {
        ensure!(directory.join("daily.md").is_file(), "daily.md is missing");
        ensure!(
            daily_report::report_is_current(config, day_text),
            "daily.md is stale"
        );
    }
    seal_day_locked(&directory, day_text, &recipient)
}

pub fn upload_archive_manually(config: &AppConfig, path: &Path) -> Result<UploadArchiveOutcome> {
    ensure!(path.is_file(), "archive does not exist: {}", path.display());
    let header = inspect_archive(path)?;
    let sha256 = sha256_file(path)?;
    let size = fs::metadata(path)?.len();
    let (upload_url, upload_token) = receiver_config(config)?;
    let runtime = upload_runtime()?;
    let client = upload_client()?;
    runtime.block_on(send_archive(
        &client,
        &upload_url,
        &upload_token,
        path,
        &header.day,
        &sha256,
    ))?;
    record_uploaded_archive(&config.activity_artifacts_dir, &sha256, &header.day)?;
    Ok(UploadArchiveOutcome {
        day: header.day,
        sha256,
        size,
    })
}

fn archive_recipient(config: &AppConfig) -> Result<RecipientMaterial> {
    ensure!(
        !config.archive_recipient_json.trim().is_empty(),
        "archive recipient is not configured"
    );
    let recipient = serde_json::from_str::<RecipientMaterial>(config.archive_recipient_json.trim())
        .context("archive recipient JSON is invalid")?;
    recipient.validate_public_material()?;
    Ok(recipient)
}

fn parse_day(day: &str) -> Result<NaiveDate> {
    let parsed = NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .with_context(|| format!("invalid day {day:?}; expected YYYY-MM-DD"))?;
    ensure!(
        parsed.format("%Y-%m-%d").to_string() == day,
        "day must use canonical YYYY-MM-DD form"
    );
    Ok(parsed)
}

fn seal_day_locked(
    directory: &Path,
    day_text: &str,
    recipient: &RecipientMaterial,
) -> Result<SealDayOutcome> {
    let archive = directory.join(ARCHIVE_FILENAME);
    if archive.is_file() {
        let header = inspect_archive(&archive)?;
        ensure!(
            header.day == day_text,
            "existing archive day does not match its directory"
        );
        let sha256 = sha256_file(&archive)?;
        let size = fs::metadata(&archive)?.len();
        cleanup_sealed_day(directory)?;
        return Ok(SealDayOutcome::AlreadySealed {
            path: archive,
            sha256,
            size,
        });
    }
    logger::info(format!(
        "Archive sealing started day={day_text} directory={}",
        directory.display()
    ));
    let bundle = pack_day(directory, day_text)?;
    let temporary = directory.join(format!("{ARCHIVE_FILENAME}.tmp"));
    if temporary.exists() {
        fs::remove_file(&temporary).with_context(|| {
            format!(
                "failed to remove stale archive temporary {}",
                temporary.display()
            )
        })?;
    }
    let metadata = encrypt_archive_file(day_text, &bundle.payload, recipient, &temporary)?;
    fs::rename(&temporary, &archive)
        .with_context(|| format!("failed to commit {}", archive.display()))?;
    cleanup_paths(&bundle.source_paths)?;
    cleanup_sealed_day(directory)?;
    Ok(SealDayOutcome::Sealed {
        path: archive,
        metadata,
    })
}

fn log_seal_outcome(day: &str, outcome: &SealDayOutcome) {
    if let SealDayOutcome::Sealed { path, metadata } = outcome {
        logger::info(format!(
            "Sealed archive day={} path={} bytes={} sha256={}",
            day,
            path.display(),
            metadata.size,
            metadata.sha256
        ));
    }
}

fn upload_archives(config: &AppConfig) -> Result<()> {
    if config.worker_base_url.trim().is_empty() || config.archive_upload_token.trim().is_empty() {
        return Ok(());
    }
    let (upload_url, upload_token) = receiver_config(config)?;
    let state = read_upload_state(&config.activity_artifacts_dir)?;
    let runtime = upload_runtime()?;
    let client = upload_client()?;
    let mut accepted = BTreeMap::new();
    for (_, directory) in dated_directories(&config.activity_artifacts_dir)? {
        let path = directory.join(ARCHIVE_FILENAME);
        if !path.is_file() {
            continue;
        }
        let sha256 = sha256_file(&path)?;
        if state.uploaded.contains_key(&sha256) || accepted.contains_key(&sha256) {
            continue;
        }
        let header = inspect_archive(&path)?;
        runtime.block_on(send_archive(
            &client,
            &upload_url,
            &upload_token,
            &path,
            &header.day,
            &sha256,
        ))?;
        record_uploaded_archive(&config.activity_artifacts_dir, &sha256, &header.day)?;
        accepted.insert(
            sha256.clone(),
            UploadedArchive {
                day: header.day.clone(),
                acknowledged_at: now(),
            },
        );
        logger::info(format!(
            "Archive receiver acknowledged day={} sha256={sha256}",
            header.day
        ));
    }
    Ok(())
}

fn receiver_config(config: &AppConfig) -> Result<(String, String)> {
    let upload_url = config.worker_endpoint("/v1/archives")?;
    let upload_token = config.archive_upload_token.trim().to_string();
    ensure!(
        !upload_token.is_empty(),
        "ASHE_ARCHIVE_UPLOAD_TOKEN is required"
    );
    Ok((upload_url, upload_token))
}

fn upload_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build archive-upload runtime")
}

fn upload_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(2 * 60))
        .build()
        .context("failed to build archive-upload client")
}

async fn send_archive(
    client: &reqwest::Client,
    upload_url: &str,
    upload_token: &str,
    path: &Path,
    day: &str,
    sha256: &str,
) -> Result<()> {
    let size = fs::metadata(path)
        .with_context(|| format!("failed to inspect encrypted archive {}", path.display()))?
        .len();
    logger::info(format!(
        "Archive upload started day={day} bytes={size} sha256={sha256}"
    ));
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read encrypted archive {}", path.display()))?;
    let response = client
        .put(format!("{upload_url}/{sha256}"))
        .bearer_auth(upload_token)
        .header("content-type", "application/vnd.ashe.archive")
        .header("x-ashe-day", day)
        .body(bytes)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "archive receiver returned HTTP {} for {}",
        response.status(),
        day
    );
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

fn record_uploaded_archive(root: &Path, sha256: &str, day: &str) -> Result<()> {
    let mut uploaded = BTreeMap::new();
    uploaded.insert(
        sha256.to_string(),
        UploadedArchive {
            day: day.to_string(),
            acknowledged_at: now(),
        },
    );
    record_uploaded_archives(root, uploaded)
}

fn record_uploaded_archives(
    root: &Path,
    uploaded: BTreeMap<String, UploadedArchive>,
) -> Result<()> {
    let _write_guard = crate::artifact_store::lock(root)?;
    let mut state = read_upload_state(root)?;
    state.uploaded.extend(uploaded);
    write_upload_state_unlocked(root, &state)
}

fn read_upload_state(root: &Path) -> Result<UploadState> {
    let path = root.join(UPLOAD_STATE_FILE);
    if !path.exists() {
        return Ok(UploadState::default());
    }
    let bytes = fs::read(&path)
        .with_context(|| format!("failed to read upload state {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("upload state is invalid: {}", path.display()))
}

fn write_upload_state_unlocked(root: &Path, state: &UploadState) -> Result<()> {
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
    use super::{
        SealDayOutcome, UploadState, UploadedArchive, cleanup_sealed_day, parse_day,
        read_upload_state, record_uploaded_archives, seal_day_locked,
    };
    use ashe_archive_crypto::{ARCHIVE_FILENAME, RecipientMaterial, inspect_archive};
    use libsodium_rs::crypto_pwhash::argon2id;
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

    #[test]
    fn upload_receipts_round_trip() {
        let root = tempfile_directory("upload-receipts");
        let mut state = UploadState::default();
        state.uploaded.insert(
            "abc".to_string(),
            UploadedArchive {
                day: "2026-07-27".to_string(),
                acknowledged_at: 42,
            },
        );
        record_uploaded_archives(&root, state.uploaded).unwrap();
        let restored = read_upload_state(&root).unwrap();
        assert_eq!(restored.uploaded["abc"].day, "2026-07-27");
        assert_eq!(restored.uploaded["abc"].acknowledged_at, 42);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sealing_encrypts_then_removes_plaintext() {
        let root = tempfile_directory("manual-seal");
        let day = root.join("2026-07-25");
        fs::create_dir(&day).unwrap();
        fs::write(day.join("daily.md"), "daily report").unwrap();
        let recipient = RecipientMaterial::create_with_limits(
            "alpha beta gamma delta epsilon",
            argon2id::OPSLIMIT_INTERACTIVE,
            argon2id::MEMLIMIT_INTERACTIVE,
        )
        .unwrap();
        let outcome = seal_day_locked(&day, "2026-07-25", &recipient).unwrap();
        let SealDayOutcome::Sealed { path, metadata } = outcome else {
            panic!("expected newly sealed archive")
        };
        assert_eq!(metadata.day, "2026-07-25");
        assert_eq!(inspect_archive(&path).unwrap().day, "2026-07-25");
        assert!(!day.join("daily.md").exists());
        assert!(day.join(ARCHIVE_FILENAME).is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archive_days_require_canonical_dates() {
        assert!(parse_day("2026-07-25").is_ok());
        assert!(parse_day("2026-7-25").is_err());
        assert!(parse_day("not-a-day").is_err());
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
