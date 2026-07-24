//! Locate the `grok` binary on the local system.
//!
//! Mirrors `src/main/agent-installer.ts:198-289` from the Electron
//! build: try `$GROK_BINARY`, then `~/.grok/bin/grok`, then the
//! bundled resource path, then well-known paths, then `which grok`.
//!
//! Returns the first match. `None` means "grok is not installed";
//! the renderer should surface this via the connection-error state.

use std::path::{Path, PathBuf};

use crate::paths;

#[derive(Debug, thiserror::Error)]
pub enum BinaryError {
    #[error("`grok` binary not found. Tried: {0}")]
    NotFound(String),
}

/// One slot in the resolution chain, for error messages.
fn describe(p: &Path) -> String {
    p.display().to_string()
}

pub fn resolve_grok() -> Result<PathBuf, BinaryError> {
    let mut tried: Vec<String> = Vec::new();

    // 1. $GROK_BINARY
    if let Ok(env) = std::env::var("GROK_BINARY") {
        let p = PathBuf::from(env);
        tried.push(describe(&p));
        if is_executable(&p) {
            return Ok(p);
        }
    }

    // 2. ~/.grok/bin/grok
    let managed = paths::managed_grok();
    tried.push(describe(&managed));
    if is_executable(&managed) {
        return Ok(managed);
    }

    // 3. ~/.grok/bin/agent (alias created by installer)
    let agent = paths::managed_agent();
    tried.push(describe(&agent));
    if is_executable(&agent) {
        return Ok(agent);
    }

    // 4. Bundled resource (process.resourcesPath/bin/grok on the Electron
    //    side; for Tauri we look under the executable dir at runtime).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("bin").join("grok");
            tried.push(describe(&p));
            if is_executable(&p) {
                return Ok(p);
            }
            let p = dir.join("bin").join("agent");
            tried.push(describe(&p));
            if is_executable(&p) {
                return Ok(p);
            }
        }
    }

    // 5. $PATH (which)
    if let Ok(p) = which::which("grok") {
        return Ok(p);
    }
    if let Ok(p) = which::which("agent") {
        return Ok(p);
    }
    tried.push("PATH lookup `which grok`".into());
    tried.push("PATH lookup `which agent`".into());

    Err(BinaryError::NotFound(tried.join(", ")))
}

fn is_executable(p: &Path) -> bool {
    // Resolve symlinks first — the managed binary at ~/.grok/bin/grok
    // is a symlink to ../downloads/grok-linux-x86_64. exists() on the
    // symlink follows the target, but metadata needs the resolved path
    // on some platforms.
    let resolved = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if !resolved.exists() { return false; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(p) {
            return meta.permissions().mode() & 0o111 != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        true
    }
}