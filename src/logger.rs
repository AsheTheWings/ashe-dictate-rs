use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn init() {
    let path = log_path();
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "\r\n{} === Ashe Worker started ===", timestamp()));
}

pub fn info(message: impl AsRef<str>) {
    let path = log_path();
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "{} {}", timestamp(), message.as_ref()));
}

pub fn log_path() -> PathBuf {
    LOG_PATH
        .get_or_init(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("ashe-worker.log")))
                .unwrap_or_else(|| PathBuf::from("ashe-worker.log"))
        })
        .clone()
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{}", now)
}
