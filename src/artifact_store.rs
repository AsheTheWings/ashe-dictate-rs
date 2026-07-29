use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

static ARTIFACT_WRITE_LOCK: Mutex<()> = Mutex::new(());
const ARTIFACT_LOCK_FILE: &str = ".ashe-artifacts.lock";

pub struct ArtifactWriteGuard {
    _process_guard: MutexGuard<'static, ()>,
    file: File,
}

impl Drop for ArtifactWriteGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn lock(root: &Path) -> Result<ArtifactWriteGuard> {
    let process_guard = ARTIFACT_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::fs::create_dir_all(root)
        .with_context(|| format!("failed to create artifact root {}", root.display()))?;
    let path = root.join(ARTIFACT_LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open artifact lock {}", path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("failed to lock artifact store {}", root.display()))?;
    Ok(ArtifactWriteGuard {
        _process_guard: process_guard,
        file,
    })
}
