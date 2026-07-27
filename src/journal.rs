use crate::activity::{self, ActivitySample};
use crate::config::AppConfig;
use crate::daily_report::DailyReportHandle;
use crate::llm_client;
use crate::logger;
use crate::screen_capture;
use anyhow::{Context, Result};
use chrono::{Local, NaiveDateTime, TimeZone};
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct JournalStatus {
    pub running: bool,
    pub summary: String,
    pub current_frames: usize,
}

#[derive(Clone)]
pub struct JournalHandle {
    tx: Sender<JournalCommand>,
    status: Arc<Mutex<JournalStatus>>,
    artifacts: PathBuf,
}

enum JournalCommand {
    Toggle,
    Shutdown,
}

impl JournalHandle {
    pub fn spawn(config: AppConfig) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let status = Arc::new(Mutex::new(JournalStatus {
            running: config.journal_enabled,
            summary: if config.journal_enabled {
                "activity journal starting".to_string()
            } else {
                "activity journal paused".to_string()
            },
            current_frames: 0,
        }));
        let worker_status = Arc::clone(&status);
        let artifacts = config.journal_artifacts_dir.clone();
        std::thread::spawn(move || {
            if let Err(error) = run(config, rx, &worker_status) {
                logger::info(format!("Activity journal stopped with error: {error:#}"));
                set_status(&worker_status, false, format!("journal error: {error}"), 0);
            }
        });
        Self {
            tx,
            status,
            artifacts,
        }
    }

    pub fn toggle(&self) {
        let _ = self.tx.send(JournalCommand::Toggle);
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(JournalCommand::Shutdown);
    }

    pub fn status(&self) -> JournalStatus {
        self.status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn artifacts_dir(&self) -> &Path {
        &self.artifacts
    }

    pub fn today_journal(&self) -> PathBuf {
        self.artifacts
            .join(Local::now().format("%Y-%m-%d").to_string())
            .join("journal.md")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct FrameRecord {
    path: String,
    ts: i64,
    monitor: i32,
    width: u32,
    height: u32,
    delta: f32,
    window: String,
    #[serde(default)]
    sent: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Block {
    start: i64,
    end: i64,
    day: String,
    frames: Vec<FrameRecord>,
    samples: Vec<ActivitySample>,
    captured: usize,
    attempts: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ContextState {
    #[serde(default)]
    summary_through: String,
}

#[derive(Clone, Debug)]
struct StoredReport {
    id: String,
    start: i64,
    content: String,
}

struct BlockMetrics {
    timeline: Vec<String>,
    totals: String,
    app_seconds: BTreeMap<String, u64>,
    active_seconds: u64,
    idle_seconds: u64,
}

#[derive(Default)]
struct DedupState {
    fingerprint: Vec<u8>,
    retained_ts: Option<i64>,
}

impl DedupState {
    fn clear(&mut self) {
        self.fingerprint.clear();
        self.retained_ts = None;
    }

    fn observe(&mut self, fingerprint: Vec<u8>, ts: i64, threshold: f32, max_gap_s: u64) -> f32 {
        let first = self.fingerprint.is_empty();
        let delta = screen_capture::changed_percent(&self.fingerprint, &fingerprint);
        let gap_elapsed = self
            .retained_ts
            .is_some_and(|previous| ts - previous >= max_gap_s as i64);
        if first || delta >= threshold || gap_elapsed {
            self.fingerprint = fingerprint;
            self.retained_ts = Some(ts);
        }
        delta
    }
}

impl Block {
    fn new(start: i64, seconds: i64) -> Self {
        Self {
            start,
            end: start + seconds,
            day: day_key(start),
            frames: Vec::new(),
            samples: Vec::new(),
            captured: 0,
            attempts: 0,
        }
    }

    fn id(&self) -> String {
        format!("{}T{}", self.day, clock(self.start, "%H%M"))
    }

    fn slug(&self) -> String {
        format!("{}-{}", clock(self.start, "%H%M"), clock(self.end, "%H%M"))
    }

    fn label(&self) -> String {
        format!(
            "{}-{}",
            clock(self.start, "%H:%M"),
            clock(self.end, "%H:%M")
        )
    }
}

fn run(
    config: AppConfig,
    commands: Receiver<JournalCommand>,
    status: &Arc<Mutex<JournalStatus>>,
) -> Result<()> {
    prepare_store(&config.journal_artifacts_dir)?;
    let _daily_reports = DailyReportHandle::spawn(config.clone());
    let mut running = config.journal_enabled;
    let block_seconds = (config.journal_block_minutes * 60) as i64;
    let mut block: Option<Block> = None;
    let mut dedup_state = DedupState::default();
    let mut next_capture = now();
    let mut was_inactive = false;

    if running && config.tera_api_key.trim().is_empty() {
        running = false;
        set_status(
            status,
            false,
            "journal paused: TERA_API_KEY/ASHE_API_KEY is missing".to_string(),
            0,
        );
    }
    if running {
        block = recover_pending(&config, block_seconds, status)?;
        logger::info(format!(
            "Activity journal started: capture={}s idle_capture={}s block={}m dedup={}pct idle={}s min_active={}s artifacts={}",
            config.journal_capture_interval,
            config.journal_idle_capture_interval,
            config.journal_block_minutes,
            config.journal_dedup_threshold,
            config.journal_idle_threshold_s,
            config.journal_min_active_seconds,
            config.journal_artifacts_dir.display()
        ));
    }

    loop {
        match commands.recv_timeout(Duration::from_millis(config.journal_telemetry_interval_ms)) {
            Ok(JournalCommand::Shutdown) => {
                if let Some(current) = block.as_ref() {
                    save_pending(&config.journal_artifacts_dir, current)?;
                }
                set_status(status, false, "activity journal stopped".to_string(), 0);
                return Ok(());
            }
            Ok(JournalCommand::Toggle) => {
                if running {
                    if let Some(current) = block.as_ref() {
                        save_pending(&config.journal_artifacts_dir, current)?;
                    }
                    dedup_state.clear();
                    running = false;
                    set_status(status, false, "activity journal paused".to_string(), 0);
                } else if config.tera_api_key.trim().is_empty() {
                    set_status(
                        status,
                        false,
                        "journal needs TERA_API_KEY/ASHE_API_KEY".to_string(),
                        0,
                    );
                } else {
                    running = true;
                    block = recover_pending(&config, block_seconds, status)?;
                    dedup_state.clear();
                    next_capture = now();
                    was_inactive = false;
                    set_status(status, true, "activity journal resumed".to_string(), 0);
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }
        if !running {
            continue;
        }

        let stamp = now();
        let boundary = stamp - stamp.rem_euclid(block_seconds);
        if block
            .as_ref()
            .is_some_and(|current| boundary >= current.end)
        {
            let finished = block.take().expect("checked above");
            describe_and_store(&config, finished, status);
            dedup_state.clear();
        }
        let current = block.get_or_insert_with(|| Block::new(boundary, block_seconds));
        let sample = activity::sample(stamp);
        current.samples.push(sample.clone());

        let inactive = sample.locked || sample.idle_s >= config.journal_idle_threshold_s;
        if was_inactive && !inactive {
            // Do not make the user wait for a stale idle/locked schedule after input resumes.
            next_capture = stamp;
        }
        was_inactive = inactive;

        let Some(capture_interval) = capture_interval_for_sample(
            &sample,
            config.journal_idle_threshold_s,
            config.journal_capture_interval,
            config.journal_idle_capture_interval,
        ) else {
            // Sampling continues for accurate coverage, but locked desktops are never captured.
            next_capture = stamp + config.journal_capture_interval as i64;
            continue;
        };

        if stamp >= next_capture {
            next_capture = stamp + capture_interval as i64;
            if !denied(&config, &sample) {
                match capture(&config, current, &mut dedup_state, &sample) {
                    Ok(()) => {
                        save_pending(&config.journal_artifacts_dir, current)?;
                        set_status(
                            status,
                            true,
                            format!("recording {}", current.label()),
                            current.frames.len(),
                        );
                    }
                    Err(error) => {
                        logger::info(format!("Journal capture failed: {error:#}"));
                        set_status(
                            status,
                            true,
                            format!("capture failed: {error}"),
                            current.frames.len(),
                        );
                    }
                }
            }
            purge_expired_frames(&config)?;
        }
    }
}

fn capture(
    config: &AppConfig,
    block: &mut Block,
    dedup_state: &mut DedupState,
    sample: &ActivitySample,
) -> Result<()> {
    let frame = screen_capture::capture_screen(config.journal_monitor == 0)?;
    let stamp = now();
    let delta = dedup_state.observe(
        frame.fingerprint,
        stamp,
        config.journal_dedup_threshold,
        config.journal_max_frame_gap_s,
    );
    let directory = day_dir(&config.journal_artifacts_dir, &block.day).join("frames");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}_m{}.webp", clock(stamp, "%H%M%S"), 1));
    fs::write(&path, frame.webp).context("failed to save journal frame")?;
    block.frames.push(FrameRecord {
        path: path.display().to_string(),
        ts: stamp,
        monitor: config.journal_monitor,
        width: frame.width,
        height: frame.height,
        delta,
        window: sample.label(),
        sent: false,
    });
    block.captured += 1;
    Ok(())
}

fn denied(config: &AppConfig, sample: &ActivitySample) -> bool {
    if config.journal_denylist.is_empty() {
        return false;
    }
    let text = format!("{} {}", sample.exe, sample.title).to_ascii_lowercase();
    config
        .journal_denylist
        .iter()
        .any(|term| text.contains(term))
}

fn capture_interval_for_sample(
    sample: &ActivitySample,
    idle_threshold_s: f64,
    active_interval_s: u64,
    idle_interval_s: u64,
) -> Option<u64> {
    if sample.locked {
        None
    } else if sample.idle_s >= idle_threshold_s {
        Some(idle_interval_s)
    } else {
        Some(active_interval_s)
    }
}

fn should_suppress_description(
    active_seconds: u64,
    distinct_frames: usize,
    min_active_seconds: u64,
) -> bool {
    active_seconds < min_active_seconds && distinct_frames <= 1
}

fn describe_and_store(config: &AppConfig, mut block: Block, status: &Arc<Mutex<JournalStatus>>) {
    let metrics = block_metrics(config, &block);
    if block.frames.is_empty() {
        let outcome = terminal_outcome(&block);
        let reason = if outcome == "locked" {
            "workstation locked"
        } else {
            "no captured activity"
        };
        let _ = write_terminal_block(config, &block, outcome, reason, &metrics);
        let _ = clear_pending(&config.journal_artifacts_dir, &block);
        return;
    }
    let distinct_frames = block
        .frames
        .iter()
        .filter(|frame| frame.delta >= config.journal_dedup_threshold)
        .count();
    if should_suppress_description(
        metrics.active_seconds,
        distinct_frames,
        config.journal_min_active_seconds,
    ) {
        let outcome = terminal_outcome(&block);
        let reason = if outcome == "locked" {
            "workstation locked"
        } else {
            "idle or unchanged"
        };
        let _ = write_terminal_block(config, &block, outcome, reason, &metrics);
        let _ = clear_pending(&config.journal_artifacts_dir, &block);
        return;
    }

    set_status(
        status,
        true,
        format!("describing {}", block.label()),
        block.frames.len(),
    );
    let selected = select_frames(&block.frames, config);
    let selected_paths: HashSet<String> = selected.iter().map(|frame| frame.path.clone()).collect();
    for frame in &mut block.frames {
        frame.sent = selected_paths.contains(&frame.path);
    }
    let images = block
        .frames
        .iter()
        .filter(|frame| frame.sent)
        .filter_map(|frame| {
            fs::read(&frame.path).ok().map(|bytes| {
                (
                    format!(
                        "{} · monitor {} · {}x{} · foreground: {}",
                        clock(frame.ts, "%H:%M:%S"),
                        frame.monitor,
                        frame.width,
                        frame.height,
                        frame.window
                    ),
                    bytes,
                )
            })
        })
        .collect::<Vec<_>>();
    if images.is_empty() {
        logger::info(format!(
            "No readable frames for journal block {}",
            block.id()
        ));
        return;
    }
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build journal runtime")
        .and_then(|runtime| {
            runtime.block_on(async {
                let earlier_context = build_earlier_context(config, block.start).await?;
                let context = format!(
                    "## Block {} on {}\nCaptured {} screenshots every {} seconds. {} visually distinct images are attached in chronological order.\nMeasured app time: {}.\n\n### Measured focus timeline (ground truth)\n{}\n\n{}\n\nWrite the report for this block only.",
                    block.label(),
                    block.day,
                    block.captured,
                    config.journal_capture_interval,
                    images.len(),
                    if metrics.totals.is_empty() { "unavailable" } else { &metrics.totals },
                    metrics.timeline
                        .iter()
                        .map(|line| format!("- {line}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    earlier_context,
                );
                llm_client::describe_activity_block(config.clone(), context, images).await
            })
        });
    match result {
        Ok((title, body)) => {
            if let Err(error) = write_report(config, &block, &title, &body, &metrics) {
                logger::info(format!("Writing journal block failed: {error:#}"));
                return;
            }
            let _ = clear_pending(&config.journal_artifacts_dir, &block);
            set_status(status, true, format!("journaled: {title}"), 0);
        }
        Err(error) => {
            block.attempts += 1;
            let _ = save_pending(&config.journal_artifacts_dir, &block);
            logger::info(format!(
                "Description of {} failed (attempt {}): {error:#}",
                block.id(),
                block.attempts
            ));
            set_status(
                status,
                true,
                format!("description pending: {error}"),
                block.frames.len(),
            );
        }
    }
}

fn select_frames<'a>(frames: &'a [FrameRecord], config: &AppConfig) -> Vec<&'a FrameRecord> {
    let mut kept = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let keep_for_gap = kept.last().is_some_and(|previous: &&FrameRecord| {
            frame.ts - previous.ts >= config.journal_max_frame_gap_s as i64
        });
        if index == 0 || frame.delta >= config.journal_dedup_threshold || keep_for_gap {
            kept.push(frame);
        }
    }
    if let Some(last) = frames.last()
        && kept.last().is_none_or(|frame| frame.path != last.path)
    {
        kept.push(last);
    }
    while kept.len() > config.journal_max_frames_per_call {
        let index = kept[1..kept.len() - 1]
            .iter()
            .enumerate()
            .min_by(|left, right| left.1.delta.total_cmp(&right.1.delta))
            .map(|(index, _)| index + 1)
            .unwrap_or(kept.len() - 1);
        kept.remove(index);
    }
    let budget = (config.journal_max_payload_mb * 1_000_000.0 * 0.72) as u64;
    while kept.len() > 2
        && kept
            .iter()
            .filter_map(|frame| fs::metadata(&frame.path).ok())
            .map(|meta| meta.len())
            .sum::<u64>()
            > budget
    {
        let index = kept[1..kept.len() - 1]
            .iter()
            .enumerate()
            .min_by(|left, right| left.1.delta.total_cmp(&right.1.delta))
            .map(|(index, _)| index + 1)
            .unwrap_or(1);
        kept.remove(index);
    }
    kept
}

fn recover_pending(
    config: &AppConfig,
    block_seconds: i64,
    status: &Arc<Mutex<JournalStatus>>,
) -> Result<Option<Block>> {
    let pending = config.journal_artifacts_dir.join("pending");
    let stamp = now();
    let boundary = stamp - stamp.rem_euclid(block_seconds);
    let mut current = None;
    for entry in fs::read_dir(pending)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(block) = serde_json::from_slice::<Block>(&bytes) else {
            continue;
        };
        if block.start == boundary {
            current = Some(block);
        } else if !report_path(&config.journal_artifacts_dir, &block).is_file() {
            describe_and_store(config, block, status);
        } else {
            let _ = fs::remove_file(path);
        }
    }
    Ok(current)
}

fn prepare_store(root: &Path) -> Result<()> {
    fs::create_dir_all(root.join("pending"))?;
    fs::create_dir_all(root.join("logs"))?;
    Ok(())
}

fn day_dir(root: &Path, day: &str) -> PathBuf {
    root.join(day)
}

fn pending_path(root: &Path, block: &Block) -> PathBuf {
    root.join("pending").join(format!("{}.json", block.id()))
}

fn report_path(root: &Path, block: &Block) -> PathBuf {
    day_dir(root, &block.day)
        .join("blocks")
        .join(format!("{}.md", block.slug()))
}

fn save_pending(root: &Path, block: &Block) -> Result<()> {
    let path = pending_path(root, block);
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(block)?)?;
    for attempt in 0..5 {
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        match fs::rename(&temporary, &path) {
            Ok(()) => return Ok(()),
            Err(error) if attempt < 4 => {
                logger::info(format!(
                    "Pending manifest replace retry {}: {error}",
                    attempt + 1
                ));
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("pending manifest retry loop always returns")
}

fn clear_pending(root: &Path, block: &Block) -> Result<()> {
    let path = pending_path(root, block);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn write_report(
    config: &AppConfig,
    block: &Block,
    title: &str,
    body: &str,
    metrics: &BlockMetrics,
) -> Result<()> {
    let directory = day_dir(&config.journal_artifacts_dir, &block.day);
    fs::create_dir_all(directory.join("blocks"))?;
    fs::create_dir_all(directory.join("keyframes"))?;
    let keyframe = block
        .frames
        .iter()
        .find(|frame| frame.sent)
        .or_else(|| block.frames.first());
    let keyframe_relative = if let Some(frame) = keyframe {
        let target = directory.join("keyframes").join(format!(
            "{}_{}.webp",
            block.slug(),
            clock(frame.ts, "%H%M%S")
        ));
        fs::copy(&frame.path, &target)?;
        format!(
            "keyframes/{}",
            target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        )
    } else {
        String::new()
    };
    let report = format!(
        "---\nblock: {}\nwindow: {} - {}\noutcome: described\nactive_seconds: {}\nidle_seconds: {}\nframes_captured: {}\nframes_sent: {}\napps: {}\napp_seconds_json: {}\nkeyframe: {}\nmodel: {}\n---\n\n# {} — {}\n\n{}\n\n## Measured timeline\n\n{}{}\n",
        block.id(),
        clock(block.start, "%H:%M:%S"),
        clock(block.end, "%H:%M:%S"),
        metrics.active_seconds,
        metrics.idle_seconds,
        block.captured,
        block.frames.iter().filter(|frame| frame.sent).count(),
        metrics.totals,
        serde_json::to_string(&metrics.app_seconds)?,
        if keyframe_relative.is_empty() {
            "-"
        } else {
            &keyframe_relative
        },
        config.tera_model,
        block.label(),
        title,
        body.trim(),
        metrics
            .timeline
            .iter()
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        if keyframe_relative.is_empty() {
            String::new()
        } else {
            format!("\n\n![keyframe](../{keyframe_relative})")
        },
    );
    fs::write(report_path(&config.journal_artifacts_dir, block), report)?;
    let journal_path = directory.join("journal.md");
    let mut journal = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path)?;
    writeln!(
        journal,
        "## {} — {}\n\n{}\n",
        block.label(),
        title,
        body.trim()
    )?;
    Ok(())
}

fn write_terminal_block(
    config: &AppConfig,
    block: &Block,
    outcome: &str,
    reason: &str,
    metrics: &BlockMetrics,
) -> Result<()> {
    let directory = day_dir(&config.journal_artifacts_dir, &block.day);
    fs::create_dir_all(directory.join("blocks"))?;
    let path = report_path(&config.journal_artifacts_dir, block);
    fs::write(
        path,
        format!(
            "---\nblock: {}\nwindow: {} - {}\noutcome: {}\nactive_seconds: {}\nidle_seconds: {}\napps: {}\napp_seconds_json: {}\n---\n\n# {} — {}\n\nNo block-description LLM report is available for this interval.\n\n## Measured timeline\n\n{}\n",
            block.id(),
            clock(block.start, "%H:%M:%S"),
            clock(block.end, "%H:%M:%S"),
            outcome,
            metrics.active_seconds,
            metrics.idle_seconds,
            metrics.totals,
            serde_json::to_string(&metrics.app_seconds)?,
            block.label(),
            reason,
            metrics
                .timeline
                .iter()
                .map(|line| format!("- {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    )?;
    Ok(())
}

fn block_metrics(config: &AppConfig, block: &Block) -> BlockMetrics {
    let interval = config.journal_telemetry_interval_ms as f64 / 1000.0;
    let timeline = activity::timeline(&block.samples, interval, config.journal_idle_threshold_s);
    let totals = activity::app_totals(&block.samples, interval);
    let app_seconds = activity::app_total_values(&block.samples, interval)
        .into_iter()
        .map(|(name, seconds)| (name, seconds.round().max(0.0) as u64))
        .collect();
    let active_samples = block
        .samples
        .iter()
        .filter(|sample| !sample.locked && sample.idle_s < config.journal_idle_threshold_s)
        .count() as f64;
    let total_seconds = block.samples.len() as f64 * interval;
    let active_seconds = (active_samples * interval).round().max(0.0) as u64;
    BlockMetrics {
        timeline,
        totals,
        app_seconds,
        active_seconds,
        idle_seconds: total_seconds.round().max(0.0) as u64 - active_seconds,
    }
}

fn terminal_outcome(block: &Block) -> &'static str {
    if !block.samples.is_empty() && block.samples.iter().all(|sample| sample.locked) {
        "locked"
    } else {
        "idle"
    }
}

async fn build_earlier_context(config: &AppConfig, before: i64) -> Result<String> {
    const SUMMARY_BATCH_SIZE: usize = 24;

    let root = &config.journal_artifacts_dir;
    let reports = load_successful_reports(root, before)?;
    let mut state = read_context_state(root);
    let unsummarized = reports
        .iter()
        .filter(|report| report.id.as_str() > state.summary_through.as_str())
        .collect::<Vec<_>>();
    let cutoff = before - (config.journal_context_summary_hours * 3600.0) as i64;
    let eligible_start = unsummarized.partition_point(|report| report.start < cutoff);
    let eligible = &unsummarized[eligible_start..];
    let detailed_offset = eligible.len().saturating_sub(config.journal_context_blocs);
    let aged_count = eligible_start + detailed_offset;
    let aged = &unsummarized[..aged_count];
    let detailed = &eligible[detailed_offset..];

    let stored_summary = read_rolling_summary(root);
    let mut summary = truncate_chars(&stored_summary, config.journal_context_summary_max_chars);
    if summary != stored_summary {
        write_rolling_summary(root, &summary, &state.summary_through)?;
    }
    for batch in aged.chunks(SUMMARY_BATCH_SIZE) {
        let input = batch
            .iter()
            .map(|report| report.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");
        summary = llm_client::refresh_activity_summary(
            config.clone(),
            summary,
            input,
            config.journal_context_summary_max_chars,
        )
        .await?;
        state.summary_through = batch
            .last()
            .map(|report| report.id.clone())
            .unwrap_or_else(|| state.summary_through.clone());
        write_rolling_summary(root, &summary, &state.summary_through)?;
        write_context_state(root, &state)?;
    }

    let mut sections = Vec::new();
    if !summary.trim().is_empty() {
        sections.push(format!(
            "### Earlier history (compressed)\nThis continuous summary ends at block {} and does not overlap the complete reports below.\n\n{}",
            state.summary_through,
            summary.trim(),
        ));
    }
    if !detailed.is_empty() {
        sections.push(format!(
            "### {} most recent eligible blocks (complete, oldest first)\nThese reports are passed in full and are not covered by the summary above.\n\n{}",
            detailed.len(),
            detailed
                .iter()
                .map(|report| report.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n---\n\n"),
        ));
    }
    if sections.is_empty() {
        Ok("### Earlier context\n(none yet)".to_string())
    } else {
        Ok(sections.join("\n\n"))
    }
}

fn load_successful_reports(root: &Path, before: i64) -> Result<Vec<StoredReport>> {
    let mut reports = Vec::new();
    for day in fs::read_dir(root)? {
        let day = day?;
        if !day.path().is_dir() {
            continue;
        }
        let blocks = day.path().join("blocks");
        let Ok(entries) = fs::read_dir(blocks) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("md") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            if !content.starts_with("---\n")
                || !content.lines().any(|line| line.starts_with("model: "))
            {
                continue;
            }
            let Some(id) = content
                .lines()
                .find_map(|line| line.strip_prefix("block: "))
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let Some(start) = block_timestamp(id) else {
                continue;
            };
            if start < before {
                reports.push(StoredReport {
                    id: id.to_string(),
                    start,
                    content,
                });
            }
        }
    }
    reports.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(reports)
}

fn block_timestamp(id: &str) -> Option<i64> {
    let naive = NaiveDateTime::parse_from_str(id, "%Y-%m-%dT%H%M").ok()?;
    Local
        .from_local_datetime(&naive)
        .single()
        .or_else(|| Local.from_local_datetime(&naive).earliest())
        .map(|stamp| stamp.timestamp())
}

fn context_state_path(root: &Path) -> PathBuf {
    root.join("context-state.json")
}

fn read_context_state(root: &Path) -> ContextState {
    fs::read(context_state_path(root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_context_state(root: &Path, state: &ContextState) -> Result<()> {
    let path = context_state_path(root);
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
    replace_file(&temporary, &path)
}

fn read_rolling_summary(root: &Path) -> String {
    let Ok(content) = fs::read_to_string(root.join("summary.md")) else {
        return String::new();
    };
    content
        .split_once("\n\n")
        .map(|(_, body)| body.trim().to_string())
        .unwrap_or_default()
}

fn write_rolling_summary(root: &Path, summary: &str, through: &str) -> Result<()> {
    let path = root.join("summary.md");
    let temporary = path.with_extension("md.tmp");
    fs::write(
        &temporary,
        format!(
            "# Rolling activity summary (through {through})\n\n{}\n",
            summary.trim()
        ),
    )?;
    replace_file(&temporary, &path)
}

fn replace_file(temporary: &Path, target: &Path) -> Result<()> {
    for attempt in 0..5 {
        if target.exists() {
            let _ = fs::remove_file(target);
        }
        match fs::rename(temporary, target) {
            Ok(()) => return Ok(()),
            Err(_error) if attempt < 4 => {
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("file replacement retry loop always returns")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn purge_expired_frames(config: &AppConfig) -> Result<()> {
    let pending_paths = fs::read_dir(config.journal_artifacts_dir.join("pending"))?
        .flatten()
        .filter_map(|entry| fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<Block>(&bytes).ok())
        .flat_map(|block| block.frames.into_iter().map(|frame| frame.path))
        .collect::<HashSet<_>>();
    let retention = Duration::from_secs(config.journal_frame_retention_minutes * 60);
    for day in fs::read_dir(&config.journal_artifacts_dir)?.flatten() {
        let frames = day.path().join("frames");
        let Ok(entries) = fs::read_dir(frames) else {
            continue;
        };
        for entry in entries.flatten() {
            if pending_paths.contains(&entry.path().display().to_string()) {
                continue;
            }
            let old_enough = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .and_then(|modified| {
                    SystemTime::now()
                        .duration_since(modified)
                        .map_err(std::io::Error::other)
                })
                .is_ok_and(|age| age >= retention);
            if old_enough {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

fn set_status(
    status: &Arc<Mutex<JournalStatus>>,
    running: bool,
    summary: String,
    current_frames: usize,
) {
    *status.lock().unwrap_or_else(|error| error.into_inner()) = JournalStatus {
        running,
        summary,
        current_frames,
    };
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn day_key(ts: i64) -> String {
    clock(ts, "%Y-%m-%d")
}

fn clock(ts: i64, format: &str) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|stamp| stamp.format(format).to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        ActivitySample, DedupState, capture_interval_for_sample, should_suppress_description,
    };

    #[test]
    fn capture_stops_while_locked_and_slows_while_idle() {
        let mut sample = ActivitySample::default();
        assert_eq!(
            capture_interval_for_sample(&sample, 120.0, 20, 120),
            Some(20)
        );

        sample.idle_s = 120.0;
        assert_eq!(
            capture_interval_for_sample(&sample, 120.0, 20, 120),
            Some(120)
        );

        sample.locked = true;
        assert_eq!(capture_interval_for_sample(&sample, 120.0, 20, 120), None);
    }

    #[test]
    fn description_is_suppressed_only_for_low_activity_and_static_visuals() {
        assert!(should_suppress_description(29, 1, 30));
        assert!(!should_suppress_description(30, 1, 30));
        assert!(!should_suppress_description(0, 2, 30));
        assert!(!should_suppress_description(0, 1, 0));
    }

    #[test]
    fn dedup_compares_against_the_last_retained_fingerprint() {
        let mut state = DedupState::default();
        assert_eq!(state.observe(vec![0; 100], 0, 0.5, 120), 100.0);

        let mut gradual = vec![0; 100];
        gradual[0] = 5;
        assert_eq!(state.observe(gradual, 20, 0.5, 120), 0.0);

        let mut accumulated = vec![0; 100];
        accumulated[0] = 10;
        assert_eq!(state.observe(accumulated, 40, 0.5, 120), 1.0);
        assert_eq!(state.retained_ts, Some(40));
    }

    #[test]
    fn maximum_gap_advances_the_visual_baseline() {
        let mut state = DedupState::default();
        state.observe(vec![0; 100], 0, 2.0, 120);

        let mut low_delta = vec![0; 100];
        low_delta[0] = 10;
        assert_eq!(state.observe(low_delta.clone(), 120, 2.0, 120), 1.0);
        assert_eq!(state.retained_ts, Some(120));
        assert_eq!(state.observe(low_delta, 140, 2.0, 120), 0.0);
    }
}
