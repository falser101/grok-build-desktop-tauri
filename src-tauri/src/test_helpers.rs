//! Test-only utilities shared between modules.
//!
//! `commands.rs` and `config_toml_sync.rs` both mutate `$GROK_HOME`
//! during tests, and `std::env::set_var` is process-global. Cargo runs
//! tests on multiple threads, so without a single shared lock, the
//! `commands::tests::GROK_HOME_LOCK` mutex would only serialise tests
//! inside `commands` — `config_toml_sync::tests` could still race
//! against `commands::tests` and stomp on each other's env override.
//!
//! Centralising the lock here keeps both modules synchronised.

#![cfg(test)]

use std::path::PathBuf;
use std::sync::Mutex;

/// Process-wide lock guarding every `$GROK_HOME` override performed by
/// tests across the crate. Tests that need to set `$GROK_HOME` MUST
/// hold this lock for the duration of the test.
pub static GROK_HOME_LOCK: Mutex<()> = Mutex::new(());

/// Override `$GROK_HOME` to a fresh temp dir for the duration of a
/// test. The returned `MutexGuard` must be held for the whole test —
/// dropping it early releases the lock and lets concurrent tests race
/// against `paths::grok_home()`.
pub fn isolate_grok_home() -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
    let _guard = GROK_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!(
        "grok-tauri-test-{}-{}",
        std::process::id(),
        // Atomic counter + SystemTime nanos so two tests started in
        // the same nanosecond on a fast machine still pick distinct
        // temp dirs.
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            format!(
                "{}{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
                n
            )
        }
    ));
    std::fs::create_dir_all(&dir).expect("create temp GROK_HOME");
    std::env::set_var("GROK_HOME", &dir);
    (dir, _guard)
}