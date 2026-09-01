use crate::config::AppConfig;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Clone)]
pub struct PasteUploader {
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct PasteResponse {
    path: String,
    sha256: String,
    bytes: usize,
}

impl Default for PasteUploader {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl PasteUploader {
    pub async fn upload_png(
        &self,
        config: AppConfig,
        png: Vec<u8>,
        captured_at: DateTime<Utc>,
    ) -> Result<String> {
        config.validate_for_paste()?;
        ensure!(!png.is_empty(), "clipboard PNG is empty");
        let sha256 = format!("{:x}", Sha256::digest(&png));
        let captured_at = captured_at.to_rfc3339_opts(SecondsFormat::Millis, true);
        let filename = canonical_filename(&captured_at, &sha256)?;
        let endpoint = format!("{}/{}", config.worker_endpoint("/v1/pastes")?, sha256);
        let response = self
            .client
            .put(endpoint)
            .timeout(Duration::from_secs(20))
            .bearer_auth(config.paste_upload_token.trim())
            .header("content-type", "image/png")
            .header("x-ashe-captured-at", &captured_at)
            .body(png.clone())
            .send()
            .await
            .context("paste upload request failed")?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .context("failed to read paste upload response")?;
        ensure!(status.is_success(), "paste receiver returned HTTP {status}");
        let value: PasteResponse =
            serde_json::from_slice(&bytes).context("paste receiver returned invalid JSON")?;
        ensure!(
            value.sha256 == sha256,
            "paste receiver returned a different digest"
        );
        ensure!(
            value.bytes == png.len(),
            "paste receiver returned a different byte count"
        );
        validate_remote_path(&value.path, &filename)?;
        Ok(value.path)
    }
}

fn validate_remote_path(path: &str, expected_filename: &str) -> Result<()> {
    ensure!(
        path == path.trim(),
        "paste receiver returned an unsafe path"
    );
    ensure!(
        !path.contains('\\'),
        "paste receiver returned an unsafe path"
    );
    let (directory, filename) = path
        .rsplit_once('/')
        .ok_or_else(|| anyhow::anyhow!("paste receiver returned a relative path"))?;
    ensure!(
        directory.starts_with('/') && directory.len() > 1,
        "paste receiver returned a relative or root-level path"
    );
    ensure!(
        directory.split('/').skip(1).all(safe_remote_component),
        "paste receiver returned an unsafe path"
    );
    ensure!(
        filename == expected_filename,
        "paste receiver returned an unexpected filename"
    );
    Ok(())
}

fn safe_remote_component(component: &str) -> bool {
    !component.is_empty()
        && !matches!(component, "." | "..")
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn canonical_filename(captured_at: &str, sha256: &str) -> Result<String> {
    ensure!(sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
    ensure!(captured_at.len() == 24 && captured_at.ends_with('Z'));
    let compact = captured_at.replace(['-', ':'], "");
    let filename = format!("{}-{}.png", compact, &sha256[..8]);
    ensure!(
        filename
            .bytes()
            .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') }),
        "generated paste filename is unsafe"
    );
    Ok(filename)
}

#[cfg(test)]
mod tests {
    use super::{canonical_filename, validate_remote_path};

    #[test]
    fn canonical_name_uses_utc_milliseconds_and_digest_prefix() {
        let digest = "4fa8c2d1aabbccdd00112233445566778899aabbccddeeff0011223344556677";
        assert_eq!(
            canonical_filename("2026-08-18T20:31:45.217Z", digest).unwrap(),
            "20260818T203145.217Z-4fa8c2d1.png"
        );
    }

    #[test]
    fn remote_path_uses_the_server_owned_directory() {
        let filename = "20260818T203145.217Z-4fa8c2d1.png";
        assert!(validate_remote_path(&format!("/root/Desktop/paste/{filename}"), filename).is_ok());
        assert!(validate_remote_path(&format!("relative/paste/{filename}"), filename).is_err());
        assert!(validate_remote_path(&format!("/paste/../{filename}"), filename).is_err());
        assert!(validate_remote_path("/root/Desktop/paste/other.png", filename).is_err());
    }
}
