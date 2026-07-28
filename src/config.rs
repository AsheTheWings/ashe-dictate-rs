use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

const DEFAULT_OUTPUT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_TERA_API_BASE: &str = "https://tera.asheservices.online/v1";
const DEFAULT_TERA_MODEL: &str = "cloudcode/chat-gemini-3-flash-paid-tier";
const DEFAULT_LLM_TEMPERATURE: f32 = 0.2;
const DEFAULT_ACTIVITY_CAPTURE_INTERVAL: u64 = 20;
const DEFAULT_ACTIVITY_IDLE_CAPTURE_INTERVAL: u64 = 120;
const DEFAULT_LEARNING_CAPTURE_INTERVAL: u64 = 10;
const DEFAULT_LEARNING_MAX_FRAME_GAP_S: u64 = 30;
const DEFAULT_ACTIVITY_BLOCK_MINUTES: u64 = 10;
const DEFAULT_ACTIVITY_DEDUP_THRESHOLD: f32 = 2.0;
const DEFAULT_ACTIVITY_MAX_FRAMES_PER_CALL: usize = 100;
const DEFAULT_ACTIVITY_MIN_ACTIVE_SECONDS: u64 = 30;
const DEFAULT_CONTEXT_BLOCKS: usize = 6;
const DEFAULT_CONTEXT_SUMMARY_MAX_CHARS: usize = 1_500;
const DEFAULT_DAILY_REPORT_GRACE_MINUTES: u64 = 15;
const DEFAULT_ARCHIVE_PLAINTEXT_DAYS: u64 = 2;
const DEFAULT_ARCHIVE_SCAN_MINUTES: u64 = 5;
const COMPILED_ARCHIVE_RECIPIENT_JSON: &str = match option_env!("ASHE_ARCHIVE_RECIPIENT_JSON") {
    Some(value) => value,
    None => "",
};

#[derive(Clone)]
pub struct AppConfig {
    pub deepgram_api_key: String,
    pub deepgram_model: String,
    pub deepgram_language: String,
    pub deepgram_keyterms: Vec<String>,
    pub output_sample_rate: u32,
    pub tera_api_key: String,
    pub tera_api_base: String,
    pub tera_model: String,
    pub llm_temperature: f32,
    pub llm_reasoning_effort: Option<String>,
    pub activity_enabled: bool,
    pub activity_artifacts_dir: PathBuf,
    pub activity_capture_interval: u64,
    pub activity_idle_capture_interval: u64,
    pub learning_enrichment_enabled: bool,
    pub learning_capture_interval: u64,
    pub learning_max_frame_gap_s: u64,
    pub activity_telemetry_interval_ms: u64,
    pub activity_block_minutes: u64,
    pub activity_monitor: i32,
    pub activity_dedup_threshold: f32,
    pub activity_max_frame_gap_s: u64,
    pub activity_max_frames_per_call: usize,
    pub activity_max_payload_mb: f32,
    pub activity_idle_threshold_s: f64,
    pub activity_min_active_seconds: u64,
    pub activity_denylist: Vec<String>,
    pub activity_frame_retention_minutes: u64,
    pub activity_context_blocks: usize,
    pub activity_context_summary_max_chars: usize,
    pub daily_report_enabled: bool,
    pub daily_report_grace_minutes: u64,
    pub archive_recipient_json: String,
    pub archive_plaintext_days: u64,
    pub archive_scan_minutes: u64,
    pub archive_upload_url: String,
    pub archive_upload_token: String,
}

impl AppConfig {
    pub fn load() -> Self {
        load_env_file_near_exe();

        let activity_artifacts_dir = std::env::var("ASHE_ARTIFACTS_DIR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(default_artifacts_dir);
        let activity_capture_interval =
            read_u64("ASHE_CAPTURE_INTERVAL", DEFAULT_ACTIVITY_CAPTURE_INTERVAL).max(1);

        let learning_capture_interval = read_u64(
            "ASHE_LEARNING_CAPTURE_INTERVAL",
            DEFAULT_LEARNING_CAPTURE_INTERVAL,
        )
        .max(1);

        Self {
            deepgram_api_key: std::env::var("DEEPGRAM_API_KEY").unwrap_or_default(),
            deepgram_model: std::env::var("DEEPGRAM_MODEL")
                .unwrap_or_else(|_| "nova-3".to_string()),
            deepgram_language: std::env::var("DEEPGRAM_LANGUAGE")
                .unwrap_or_else(|_| "en-US".to_string()),
            deepgram_keyterms: std::env::var("DEEPGRAM_KEYTERMS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            output_sample_rate: read_output_sample_rate(),
            tera_api_key: std::env::var("TERA_API_KEY")
                .or_else(|_| std::env::var("ASHE_API_KEY"))
                .unwrap_or_default(),
            tera_api_base: std::env::var("TERA_API_BASE")
                .or_else(|_| std::env::var("ASHE_BASE_URL"))
                .unwrap_or_else(|_| DEFAULT_TERA_API_BASE.to_string()),
            tera_model: std::env::var("TERA_MODEL")
                .or_else(|_| std::env::var("ASHE_MODEL"))
                .unwrap_or_else(|_| DEFAULT_TERA_MODEL.to_string()),
            llm_temperature: read_f32("ASHE_LLM_TEMPERATURE", DEFAULT_LLM_TEMPERATURE),
            llm_reasoning_effort: std::env::var("ASHE_LLM_REASONING_EFFORT")
                .ok()
                .filter(|value| !value.is_empty()),
            activity_enabled: read_bool("ASHE_ACTIVITY_ENABLED", true),
            activity_artifacts_dir,
            activity_capture_interval,
            activity_idle_capture_interval: read_u64(
                "ASHE_IDLE_CAPTURE_INTERVAL",
                DEFAULT_ACTIVITY_IDLE_CAPTURE_INTERVAL,
            )
            .max(activity_capture_interval),
            learning_enrichment_enabled: read_bool("ASHE_LEARNING_ENRICHMENT_ENABLED", true),
            learning_capture_interval,
            learning_max_frame_gap_s: read_u64(
                "ASHE_LEARNING_MAX_FRAME_GAP_S",
                DEFAULT_LEARNING_MAX_FRAME_GAP_S,
            )
            .max(learning_capture_interval),
            activity_telemetry_interval_ms: (read_f32("ASHE_TELEMETRY_INTERVAL", 2.0).max(0.5)
                * 1000.0) as u64,
            activity_block_minutes: read_u64("ASHE_BLOCK_MINUTES", DEFAULT_ACTIVITY_BLOCK_MINUTES)
                .max(1),
            activity_monitor: read_i32("ASHE_MONITOR", 1),
            activity_dedup_threshold: read_f32(
                "ASHE_DEDUP_THRESHOLD",
                DEFAULT_ACTIVITY_DEDUP_THRESHOLD,
            )
            .clamp(0.0, 100.0),
            activity_max_frame_gap_s: read_u64("ASHE_MAX_FRAME_GAP_S", 120),
            activity_max_frames_per_call: read_usize(
                "ASHE_MAX_FRAMES_PER_CALL",
                DEFAULT_ACTIVITY_MAX_FRAMES_PER_CALL,
            )
            .max(2),
            activity_max_payload_mb: read_f32("ASHE_MAX_PAYLOAD_MB", 48.0).max(1.0),
            activity_idle_threshold_s: read_f32("ASHE_IDLE_THRESHOLD_S", 120.0).max(10.0) as f64,
            activity_min_active_seconds: read_u64(
                "ASHE_MIN_ACTIVE_SECONDS",
                DEFAULT_ACTIVITY_MIN_ACTIVE_SECONDS,
            ),
            activity_denylist: read_list("ASHE_DENYLIST"),
            activity_frame_retention_minutes: read_u64("ASHE_FRAME_RETENTION_MINUTES", 30),
            activity_context_blocks: read_usize("ASHE_CONTEXT_BLOCKS", DEFAULT_CONTEXT_BLOCKS),
            activity_context_summary_max_chars: read_usize(
                "ASHE_MAX_SUMMARY_CHARS",
                DEFAULT_CONTEXT_SUMMARY_MAX_CHARS,
            )
            .max(1),
            daily_report_enabled: read_bool("ASHE_DAILY_REPORT_ENABLED", true),
            daily_report_grace_minutes: read_u64(
                "ASHE_DAILY_REPORT_GRACE_MINUTES",
                DEFAULT_DAILY_REPORT_GRACE_MINUTES,
            ),
            archive_recipient_json: std::env::var("ASHE_ARCHIVE_RECIPIENT_JSON")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| COMPILED_ARCHIVE_RECIPIENT_JSON.to_string()),
            archive_plaintext_days: read_u64(
                "ASHE_ARCHIVE_PLAINTEXT_DAYS",
                DEFAULT_ARCHIVE_PLAINTEXT_DAYS,
            )
            .max(2),
            archive_scan_minutes: read_u64(
                "ASHE_ARCHIVE_SCAN_MINUTES",
                DEFAULT_ARCHIVE_SCAN_MINUTES,
            )
            .max(1),
            archive_upload_url: std::env::var("ASHE_ARCHIVE_UPLOAD_URL").unwrap_or_default(),
            archive_upload_token: std::env::var("ASHE_ARCHIVE_UPLOAD_TOKEN").unwrap_or_default(),
        }
    }

    pub fn validate_for_dictation(&self) -> Result<()> {
        if self.deepgram_api_key.trim().is_empty() {
            return Err(anyhow!("DEEPGRAM_API_KEY is missing"));
        }
        if self.deepgram_model.trim().is_empty() {
            return Err(anyhow!("DEEPGRAM_MODEL is empty"));
        }
        if self.deepgram_language.trim().is_empty() {
            return Err(anyhow!("DEEPGRAM_LANGUAGE is empty"));
        }
        if !(8_000..=192_000).contains(&self.output_sample_rate) {
            return Err(anyhow!(
                "ASHE_OUTPUT_SAMPLE_RATE must be between 8000 and 192000"
            ));
        }
        if self.tera_api_key.trim().is_empty() {
            return Err(anyhow!("TERA_API_KEY is missing"));
        }
        if self.tera_api_base.trim().is_empty() {
            return Err(anyhow!("TERA_API_BASE is empty"));
        }
        if self.tera_model.trim().is_empty() {
            return Err(anyhow!("TERA_MODEL is empty"));
        }
        Ok(())
    }

    pub fn validate_for_llm(&self) -> Result<()> {
        if self.tera_api_key.trim().is_empty() {
            return Err(anyhow!("TERA_API_KEY is missing"));
        }
        if self.tera_api_base.trim().is_empty() {
            return Err(anyhow!("TERA_API_BASE is empty"));
        }
        if self.tera_model.trim().is_empty() {
            return Err(anyhow!("TERA_MODEL is empty"));
        }
        Ok(())
    }

    pub fn deepgram_query_params(&self) -> Vec<(String, String)> {
        let mut pairs = vec![
            ("model".to_string(), self.deepgram_model.clone()),
            ("language".to_string(), self.deepgram_language.clone()),
            ("smart_format".to_string(), "true".to_string()),
            ("mip_opt_out".to_string(), "true".to_string()),
        ];
        for keyterm in &self.deepgram_keyterms {
            pairs.push(("keyterm".to_string(), keyterm.clone()));
        }
        pairs
    }

    pub fn log_summary(&self) -> String {
        format!(
            "deepgram_model={} language={} keyterms={} output_sample_rate={} deepgram_api_key_present={} tera_model={} tera_api_key_present={} reasoning_effort_present={} activity_enabled={} activity_artifacts={} learning_enrichment_enabled={} learning_capture_interval={}s learning_max_frame_gap={}s context_blocks={} summary_max_chars={} daily_report_enabled={} daily_grace_minutes={} archive_recipient_present={} archive_plaintext_days={} archive_upload_configured={}",
            self.deepgram_model,
            self.deepgram_language,
            self.deepgram_keyterms.len(),
            self.output_sample_rate,
            !self.deepgram_api_key.trim().is_empty(),
            self.tera_model,
            !self.tera_api_key.trim().is_empty(),
            self.llm_reasoning_effort.is_some(),
            self.activity_enabled,
            self.activity_artifacts_dir.display(),
            self.learning_enrichment_enabled,
            self.learning_capture_interval,
            self.learning_max_frame_gap_s,
            self.activity_context_blocks,
            self.activity_context_summary_max_chars,
            self.daily_report_enabled,
            self.daily_report_grace_minutes,
            !self.archive_recipient_json.trim().is_empty(),
            self.archive_plaintext_days,
            !self.archive_upload_url.trim().is_empty()
                && !self.archive_upload_token.trim().is_empty(),
        )
    }
}

fn read_output_sample_rate() -> u32 {
    read_u32("ASHE_OUTPUT_SAMPLE_RATE", DEFAULT_OUTPUT_SAMPLE_RATE)
}

fn read_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

fn read_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn read_i32(name: &str, default: i32) -> i32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .unwrap_or(default)
}

fn read_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn read_bool(name: &str, default: bool) -> bool {
    match std::env::var(name)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn read_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

fn read_f32(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok())
        .unwrap_or(default)
}

fn load_env_file_near_exe() {
    let mut path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    for _ in 0..8 {
        let Some(current) = path.clone() else {
            break;
        };
        let env_path = current.join(".env.local");
        if env_path.exists() {
            let _ = dotenvy::from_path_override(env_path);
            return;
        }
        path = current.parent().map(PathBuf::from);
    }

    let sibling = PathBuf::from(r"e:\Desktop\ashe-worker\.env.local");
    if sibling.exists() {
        let _ = dotenvy::from_path_override(sibling);
    }
}

fn default_artifacts_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("artifacts")))
        .unwrap_or_else(|| PathBuf::from("artifacts"))
}
