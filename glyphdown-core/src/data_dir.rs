//! Resolve the glyphdown data directory using the same contract the
//! Python `glyphdown_paths` module honors:
//!   1. `GLYPHDOWN_DATA_DIR` env var wins
//!   2. Windows: `%LOCALAPPDATA%/ultracos`
//!   3. POSIX: `~/.ultracos`
//!
//! NOTE: the on-disk default keeps the legacy `ultracos` name even after the
//! glyphdown brand rename — renaming it would orphan existing audit/cache
//! state. Migration-safe: leave the path, rename only the env-var knob.
//!
//! Read-only here — directory creation is the writers' responsibility;
//! if the dir is missing, that's a "no data yet" state we report.

use std::path::PathBuf;

pub fn resolve() -> PathBuf {
    if let Ok(env_dir) = std::env::var("GLYPHDOWN_DATA_DIR") {
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
