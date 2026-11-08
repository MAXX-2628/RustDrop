//! Default locations shared by the CLI and the platform apps.

use std::{env, path::PathBuf};

/// Identity, keys and trust store. `RUSTDROP_HOME` overrides.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = env::var_os("RUSTDROP_HOME") {
        return dir.into();
    }
    let base = env::var_os("APPDATA")
        .or_else(|| env::var_os("HOME"))
        .unwrap_or_else(|| ".".into());
    let dir = PathBuf::from(&base).join("Ferrydrop");
    // Renamed from RustDrop: move the old folder once so the identity and pairings survive.
    let old = PathBuf::from(base).join("RustDrop");
    // If the old app still has it open the move fails: keep using it, and move it next start.
    if !dir.exists() && old.is_dir() && std::fs::rename(&old, &dir).is_err() {
        return old;
    }
    dir
}

/// Where received files go. `RUSTDROP_DOWNLOADS` overrides.
pub fn downloads_dir() -> PathBuf {
    if let Some(dir) = env::var_os("RUSTDROP_DOWNLOADS") {
        return dir.into();
    }
    let home = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .unwrap_or_else(|| ".".into());
    PathBuf::from(home).join("Downloads").join("Ferrydrop")
}
