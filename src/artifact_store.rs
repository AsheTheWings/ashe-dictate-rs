use std::sync::{Mutex, MutexGuard};

static ARTIFACT_WRITE_LOCK: Mutex<()> = Mutex::new(());

pub fn lock() -> MutexGuard<'static, ()> {
    ARTIFACT_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
