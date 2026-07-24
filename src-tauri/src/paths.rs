//! Helpers for resolving on-disk locations shared with `grok` / `agent`.
//!
//! Mirrors `crates/codegen/xai-grok-config/src/paths.rs:28-110` from
//! the Rust workspace but inlines only the bits we need, so the Tauri
//! crate has zero workspace dependencies.

use std::path::PathBuf;

/// `$HOME/.grok`, override with `$GROK_HOME`. Always created if missing.
pub fn grok_home() -> PathBuf {
    if let Ok(env) = std::env::var("GROK_HOME") {
        let p = PathBuf::from(env);
        let _ = std::fs::create_dir_all(&p);
        return p;
    }
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let p = home.join(".grok");
    let _ = std::fs::create_dir_all(&p);
    p
}

/// `$GROK_HOME/bin/grok` (or `grok.exe` on Windows).
pub fn managed_grok() -> PathBuf {
    grok_home().join("bin").join(if cfg!(windows) { "grok.exe" } else { "grok" })
}

/// `$GROK_HOME/bin/agent` (or `agent.exe` on Windows).
pub fn managed_agent() -> PathBuf {
    grok_home().join("bin").join(if cfg!(windows) { "agent.exe" } else { "agent" })
}