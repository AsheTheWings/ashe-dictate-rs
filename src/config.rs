use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

const DEFAULT_OUTPUT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_FIREWORKS_API_BASE: &str = "https://api.fireworks.ai/inference/v1";
const DEFAULT_FIREWORKS_MODEL: &str = "accounts/fireworks/models/kimi-k2p6";
const DEFAULT_LLM_MAX_TOKENS: u32 = 1536;
const DEFAULT_LLM_TEMPERATURE: f32 = 0.2;

#[derive(Clone)]
pub struct AppConfig {
    pub deepgram_api_key: String,
    pub deepgram_model: String,
    pub deepgram_language: String,
    pub deepgram_keyterms: Vec<String>,
    pub output_sample_rate: u32,
    pub fireworks_api_key: String,
    pub fireworks_api_base: String,
    pub fireworks_model: String,
    pub llm_max_tokens: u32,
    pub llm_temperature: f32,
}

impl AppConfig {
    pub fn load() -> Self {
        let _ = dotenvy::dotenv();
        load_env_file_near_exe();

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
            fireworks_api_key: std::env::var("FIREWORKS_API_KEY").unwrap_or_default(),
            fireworks_api_base: std::env::var("FIREWORKS_API_BASE")
                .unwrap_or_else(|_| DEFAULT_FIREWORKS_API_BASE.to_string()),
            fireworks_model: std::env::var("FIREWORKS_MODEL")
                .unwrap_or_else(|_| DEFAULT_FIREWORKS_MODEL.to_string()),
            llm_max_tokens: read_u32("ASHE_LLM_MAX_TOKENS", DEFAULT_LLM_MAX_TOKENS),
            llm_temperature: read_f32("ASHE_LLM_TEMPERATURE", DEFAULT_LLM_TEMPERATURE),
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
        if self.fireworks_api_key.trim().is_empty() {
            return Err(anyhow!("FIREWORKS_API_KEY is missing"));
        }
        if self.fireworks_api_base.trim().is_empty() {
            return Err(anyhow!("FIREWORKS_API_BASE is empty"));
        }
        if self.fireworks_model.trim().is_empty() {
            return Err(anyhow!("FIREWORKS_MODEL is empty"));
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
            "deepgram_model={} language={} keyterms={} output_sample_rate={} deepgram_api_key_present={} fireworks_model={} fireworks_api_key_present={}",
            self.deepgram_model,
            self.deepgram_language,
            self.deepgram_keyterms.len(),
            self.output_sample_rate,
            !self.deepgram_api_key.trim().is_empty(),
            self.fireworks_model,
            !self.fireworks_api_key.trim().is_empty()
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

    let sibling = PathBuf::from(r"e:\Desktop\ashe-dictate\.env.local");
    if sibling.exists() {
        let _ = dotenvy::from_path_override(sibling);
    }
}
