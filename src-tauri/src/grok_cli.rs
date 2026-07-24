//! Thin wrapper around `spawn("grok", args)` — mirrors the Electron
//! main's `spawn(binary, args)` pattern. Returns stdout (trimmed) on
//! success; rejects on non-zero exit or spawn failure.

use crate::binary::resolve_grok;
use std::time::Duration;
use tokio::process::Command;

const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn run(args: &[&str]) -> Result<String, String> {
    run_with_env(args, &[], CLI_TIMEOUT).await
}

pub async fn run_long(args: &[&str]) -> Result<String, String> {
    run_with_env(args, &[], INSTALL_TIMEOUT).await
}

pub async fn run_with_env(
    args: &[&str],
    extra_envs: &[(&str, &str)],
    timeout: Duration,
) -> Result<String, String> {
    let binary = resolve_grok().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(binary);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    for (k, v) in extra_envs {
        cmd.env(k, v);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn grok: {}", e))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("grok {} timed out", args.join(" ")))?
        .map_err(|e| format!("grok {} error: {}", args.join(" "), e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "grok {} exited with {}: {}",
            args.join(" "),
            output.status,
            stderr
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn run_inline_bash(script: &str) -> Result<String, String> {
    let output = Command::new("bash")
        .args(["-lc", script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to spawn bash: {}", e))?
        .wait_with_output()
        .await
        .map_err(|e| format!("bash script error: {}", e))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        return Err(format!(
            "bash script exited with {}: {}",
            output.status,
            if stderr.is_empty() { stdout } else { stderr }
        ));
    }
    Ok(stdout)
}
