use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static LOG_WRITE_LOCK: Mutex<()> = Mutex::new(());

pub fn init() {
    append(format_args!(
        "\r\n{} === Ashe Worker started ===",
        timestamp()
    ));
}

pub fn info(message: impl AsRef<str>) {
    append(format_args!("{} {}", timestamp(), message.as_ref()));
}

fn append(arguments: std::fmt::Arguments<'_>) {
    let _guard = LOG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
        .and_then(|mut file| writeln!(file, "{arguments}"));
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
