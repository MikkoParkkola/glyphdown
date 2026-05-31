//! Resolve the ultracos data directory using the same contract the
//! Python `ultracos_paths` module honors:
//!   1. `ULTRACOS_DATA_DIR` env var wins
//!   2. Windows: `%LOCALAPPDATA%/ultracos`
//!   3. POSIX: `~/.ultracos`
//!
//! Read-only here — directory creation is the writers' responsibility;
//! if the dir is missing, that's a "no data yet" state we report.

use std::path::PathBuf;

pub fn resolve() -> PathBuf {
    if let Ok(env_dir) = std::env::var("ULTRACOS_DATA_DIR") {
        let trimmed = env_dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if cfg!(target_os = "windows") {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let trimmed = local.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed).join("ultracos");
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".ultracos");
    }
    PathBuf::from(".ultracos")
}
