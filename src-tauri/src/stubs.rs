//! Shared "not implemented" helper for stubbed commands.
//!
//! Every command in the v1 stub layer funnels through here so the
//! warning log + error message stay consistent and the call site
//! stays one line.

pub fn not_implemented<T>(name: &str) -> Result<T, String> {
    tracing::warn!(command = %name, "stubbed command invoked (not in v1)");
    Err(format!(
        "{name}: not implemented in grok-build-desktop-tauri v1. \
         See docs/MIGRATION_STATUS.md."
    ))
}