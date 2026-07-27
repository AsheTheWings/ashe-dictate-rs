use crate::block_artifact::{BlockArtifact, BlockDocumentInput};
use crate::config::AppConfig;
use crate::llm_client;
use crate::logger;
use anyhow::{Context, Result};
use chrono::{Local, NaiveDate, TimeZone};
use crossbeam_channel::{Receiver, Sender};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAILY_SCAN_INTERVAL: Duration = Duration::from_secs(5 * 60);
const DAILY_PROMPT_VERSION: &str = "ashe-worker-daily-v3";

pub struct DailyReportHandle {
    tx: Option<Sender<()>>,
}

impl DailyReportHandle {
    pub fn spawn(config: AppConfig) -> Self {
        if !config.daily_report_enabled {
            return Self { tx: None };
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || run(config, rx));
        Self { tx: Some(tx) }
    }
}

impl Drop for DailyReportHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Serialize)]
struct DailyTotals {
    active_seconds: u64,
    idle_seconds: u64,
    app_seconds: BTreeMap<String, u64>,
    outcomes: BTreeMap<String, usize>,
}

#[derive(Serialize)]
struct CoverageGap {
    start: String,
    end: String,
    status: &'static str,
}

#[derive(Serialize)]
struct DailyCoverage {
    complete: bool,
    gaps: Vec<CoverageGap>,
}

struct DailySource {
    blocks: Vec<BlockDocumentInput>,
    pending_ids: HashSet<String>,
    totals: DailyTotals,
    coverage: DailyCoverage,
    hash: String,
}

impl DailySource {
    fn document_input(&self, day: &str, timezone: &str) -> Value {
        let mut pending_block_ids = self.pending_ids.iter().cloned().collect::<Vec<_>>();
        pending_block_ids.sort();
        json!({
            "day": day,
            "timezone": timezone,
            "aggregate": {
                "measured_totals": &self.totals,
                "coverage": &self.coverage,
                "pending_block_ids": pending_block_ids,
                "blocks": &self.blocks,
            },
        })
    }
}

fn run(config: AppConfig, stop: Receiver<()>) {
    loop {
        if !config.tera_api_key.trim().is_empty()
            && let Err(error) = scan_closed_days(&config)
        {
            logger::info(format!("Daily report scan failed: {error:#}"));
        }
        match stop.recv_timeout(DAILY_SCAN_INTERVAL) {
            Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn scan_closed_days(config: &AppConfig) -> Result<()> {
    let root = &config.journal_artifacts_dir;
    let today = Local::now().format("%Y-%m-%d").to_string();
    let mut days = fs::read_dir(root)?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            (name.len() == 10 && name.as_str() < today.as_str()).then_some(name)
        })
        .collect::<Vec<_>>();
    days.sort();
    for day in days {
        if root
            .join(&day)
            .join(ashe_archive_crypto::ARCHIVE_FILENAME)
            .is_file()
        {
            continue;
        }
        if !grace_period_elapsed(&day, config.daily_report_grace_minutes) {
            continue;
        }
        if let Err(error) = generate_day(config, &day) {
            logger::info(format!("Daily report for {day} failed: {error:#}"));
        }
    }
    Ok(())
}

fn grace_period_elapsed(day: &str, grace_minutes: u64) -> bool {
    day_bounds(day).is_some_and(|(_, end)| now() >= end.saturating_add((grace_minutes * 60) as i64))
}

fn generate_day(config: &AppConfig, day: &str) -> Result<()> {
    let source = build_daily_source(config, day)?;
    let output = config.journal_artifacts_dir.join(day).join("daily.md");
    if read_frontmatter_value(&output, "source_hash").as_deref() == Some(&source.hash) {
        return Ok(());
    }

    let timezone = Local::now().offset().to_string();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build daily-report runtime")?;
    let report = runtime.block_on(llm_client::generate_daily_activity_report(
        config.clone(),
        source.document_input(day, &timezone),
    ))?;
    let document = format!(
        "---\ndate: {day}\ntimezone: {timezone}\ngenerated_at: {}\nsource_hash: {}\nprompt_version: {}\nsource_blocks: {}\npending_blocks: {}\ncoverage_complete: {}\nmodel: {}\n---\n\n# Activity report — {day}\n\n{}\n",
        Local::now().to_rfc3339(),
        source.hash,
        DAILY_PROMPT_VERSION,
        source.blocks.len(),
        source.pending_ids.len(),
        source.coverage.complete,
        config.tera_model,
        report.trim(),
    );
    let _write_guard = crate::artifact_store::lock();
    if output.parent().is_some_and(|directory| {
        directory
            .join(ashe_archive_crypto::ARCHIVE_FILENAME)
            .is_file()
    }) {
        return Ok(());
    }
    write_atomic(&output, document.as_bytes())?;
    logger::info(format!(
        "Daily report written: {} ({} blocks, complete={})",
        output.display(),
        source.blocks.len(),
        source.coverage.complete
    ));
    Ok(())
}

pub(crate) fn report_is_current(config: &AppConfig, day: &str) -> bool {
    let output = config.journal_artifacts_dir.join(day).join("daily.md");
    build_daily_source(config, day).is_ok_and(|source| {
        read_frontmatter_value(&output, "source_hash").as_deref() == Some(&source.hash)
    })
}

fn build_daily_source(config: &AppConfig, day: &str) -> Result<DailySource> {
    let mut blocks = load_day_reports(config, day)?;
    blocks.sort_by(|left, right| left.block.cmp(&right.block));
    let pending_ids = load_pending_ids(&config.journal_artifacts_dir, day);
    let report_ids = blocks
        .iter()
        .map(|block| block.block.clone())
        .collect::<HashSet<_>>();
    let coverage = coverage_input(day, config.journal_block_minutes, &report_ids, &pending_ids);
    let totals = measured_totals(&blocks);
    let mut pending_for_hash = pending_ids.iter().cloned().collect::<Vec<_>>();
    pending_for_hash.sort();
    let hash_source = json!({
        "prompt_version": DAILY_PROMPT_VERSION,
        "day": day,
        "model": config.tera_model,
        "block_minutes": config.journal_block_minutes,
        "pending_block_ids": pending_for_hash,
        "measured_totals": &totals,
        "coverage": &coverage,
        "blocks": &blocks,
    });
    Ok(DailySource {
        blocks,
        pending_ids,
        totals,
        coverage,
        hash: stable_hash(&serde_json::to_vec(&hash_source)?),
    })
}

fn load_day_reports(config: &AppConfig, day: &str) -> Result<Vec<BlockDocumentInput>> {
    let directory = config.journal_artifacts_dir.join(day).join("blocks");
    let Ok(entries) = fs::read_dir(directory) else {
        return Ok(Vec::new());
    };
    let mut reports = BTreeMap::new();
    for path in entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
    {
        let Ok(bytes) = fs::read(path) else { continue };
        let Ok(artifact) = serde_json::from_slice::<BlockArtifact>(&bytes) else {
            continue;
        };
        if !artifact.is_supported() {
            continue;
        }
        reports.insert(artifact.block.clone(), artifact.document_input());
    }
    Ok(reports.into_values().collect())
}

fn load_pending_ids(root: &Path, day: &str) -> HashSet<String> {
    let Ok(entries) = fs::read_dir(root.join("pending")) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter_map(|entry| fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|value| value.get("day").and_then(Value::as_str) == Some(day))
        .filter_map(|value| value.get("start").and_then(Value::as_i64))
        .map(|start| format!("{}T{}", day, local_clock(start, "%H%M")))
        .collect()
}

fn coverage_input(
    day: &str,
    block_minutes: u64,
    reports: &HashSet<String>,
    pending: &HashSet<String>,
) -> DailyCoverage {
    let Some((start, end)) = day_bounds(day) else {
        return DailyCoverage {
            complete: false,
            gaps: vec![CoverageGap {
                start: day.to_string(),
                end: day.to_string(),
                status: "invalid local date",
            }],
        };
    };
    let span = (block_minutes * 60) as i64;
    let mut cursor = start;
    let remainder = cursor.rem_euclid(span);
    if remainder != 0 {
        cursor += span - remainder;
    }
    let mut gaps: Vec<(i64, i64, &'static str)> = Vec::new();
    if cursor > start {
        gaps.push((start, cursor.min(end), "missing/unknown"));
    }
    while cursor < end {
        let block_end = (cursor + span).min(end);
        let id = format!("{}T{}", day, local_clock(cursor, "%H%M"));
        let status = if reports.contains(&id) {
            None
        } else if pending.contains(&id) {
            Some("pending description")
        } else {
            Some("missing/unknown")
        };
        if let Some(status) = status {
            if let Some(last) = gaps.last_mut()
                && last.1 == cursor
                && last.2 == status
            {
                last.1 = block_end;
            } else {
                gaps.push((cursor, block_end, status));
            }
        }
        cursor += span;
    }
    let gaps = gaps
        .into_iter()
        .map(|(start, end, status)| CoverageGap {
            start: local_clock(start, "%H:%M"),
            end: local_clock(end, "%H:%M"),
            status,
        })
        .collect::<Vec<_>>();
    DailyCoverage {
        complete: gaps.is_empty(),
        gaps,
    }
}

fn measured_totals(blocks: &[BlockDocumentInput]) -> DailyTotals {
    let active_seconds = blocks.iter().map(|block| block.active_seconds).sum();
    let idle_seconds = blocks.iter().map(|block| block.idle_seconds).sum();
    let mut app_seconds = BTreeMap::new();
    let mut outcomes = BTreeMap::new();
    for block in blocks {
        *outcomes.entry(block.outcome.clone()).or_default() += 1;
        for (app, seconds) in &block.app_seconds {
            *app_seconds.entry(app.clone()).or_default() += seconds;
        }
    }
    DailyTotals {
        active_seconds,
        idle_seconds,
        app_seconds,
        outcomes,
    }
}

fn frontmatter_value(content: &str, name: &str) -> Option<String> {
    if !content.starts_with("---\n") {
        return None;
    }
    let prefix = format!("{name}: ");
    content
        .lines()
        .skip(1)
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn read_frontmatter_value(path: &Path, name: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| frontmatter_value(&content, name))
}

fn day_bounds(day: &str) -> Option<(i64, i64)> {
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let next = date.succ_opt()?;
    let start = Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .earliest()?
        .timestamp();
    let end = Local
        .from_local_datetime(&next.and_hms_opt(0, 0, 0)?)
        .earliest()?
        .timestamp();
    Some((start, end))
}

fn local_clock(ts: i64, format: &str) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|stamp| stamp.format(format).to_string())
        .unwrap_or_default()
}

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let temporary = PathBuf::from(format!("{}.tmp", path.display()));
    fs::write(&temporary, content)?;
    for attempt in 0..5 {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
        match fs::rename(&temporary, path) {
            Ok(()) => return Ok(()),
            Err(_error) if attempt < 4 => {
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("daily report replacement retry loop always returns")
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
