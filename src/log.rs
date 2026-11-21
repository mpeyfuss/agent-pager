//! Minimal append-only log. Hooks must keep stdout silent (Codex treats
//! PermissionRequest hook output as a decision), so diagnostics go here.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn path() -> PathBuf {
    crate::home().join("Library/Logs/agent-pager.log")
}

fn write(level: &str, msg: &str) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path()) {
        let _ = writeln!(f, "{ts} {level} [pid {}] {msg}", std::process::id());
    }
}

pub fn info(msg: &str) {
    write("INFO", msg);
}

pub fn error(msg: &str) {
    write("ERROR", msg);
}

/// Debug logging is on when AGENT_PAGER_DEBUG is set, or when the file
/// ~/.config/agent-pager/debug exists (handy since hooks inherit the
/// agent's environment, not your shell's).
pub fn debug_enabled() -> bool {
    std::env::var_os("AGENT_PAGER_DEBUG").is_some()
        || crate::home().join(".config/agent-pager/debug").exists()
}

pub fn debug(msg: &str) {
    if debug_enabled() {
        write("DEBUG", msg);
    }
}
