use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

const DEFAULT_OUTPUT_SAMPLE_RATE: u32 = 48_000;

#[derive(Clone)]
pub struct AppConfig {
    pub deepgram_api_key: String,
    pub deepgram_model: String,
    pub deepgram_language: String,
    pub deepgram_keyterms: Vec<String>,
    pub output_sample_rate: u32,
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
            "model={} language={} keyterms={} output_sample_rate={} api_key_present={}",
            self.deepgram_model,
            self.deepgram_language,
            self.deepgram_keyterms.len(),
            self.output_sample_rate,
            !self.deepgram_api_key.trim().is_empty()
        )
    }
}

fn read_output_sample_rate() -> u32 {
    std::env::var("ASHE_OUTPUT_SAMPLE_RATE")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_OUTPUT_SAMPLE_RATE)
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
