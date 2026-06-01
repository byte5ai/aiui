//! Issue #135 — typed input field with file-write (incl. `secret` mode).
//!
//! A `form` field may carry an optional `target`: on affirmative submit, aiui
//! writes the entered value to a file. The write is **always a local file
//! operation on the host the agent runs on** — because an aiui module already
//! lives there: the native app on a local Mac session, the Python bridge on a
//! remote SSH session. Each side writes its own filesystem; the value reaches
//! the side that needs it over the existing :7777 channel (never via the
//! agent/LLM). No `scp`, no cross-host write, no atomic-remote-replace
//! problem — `substitute` is a plain local read-modify-write everywhere.
//!
//! This module is the **local writer**, used by the native app for a
//! Rust-bridge (local Mac) session. The Python bridge has the mirror
//! implementation for sessions it serves (local-via-uvx or remote). For a
//! `secret` field the value is written only and never returned to the agent.
//!
//! Confused-deputy note: because the write is always local to whichever aiui
//! module is on the agent's own host, there is **no host parameter** and thus
//! no way to redirect a write to a foreign host — exfiltration is structurally
//! impossible. The agent still controls the *path on its own host*, so the
//! user-visible approval (the affirmative button, with the path shown) remains
//! the authorization backstop.
//!
//! Modes (explicit, never inferred from file existence):
//! - `create` — write the raw value; refuse to clobber unless `overwrite`.
//! - `substitute` — replace a `placeholder` that occurs exactly once in an
//!   existing file (0 or >1 → error, never a partial write). Format-agnostic.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteMode {
    Create,
    Substitute,
}

/// Per-field write target, parsed from the spec's `target` object.
#[derive(Debug, Clone, Deserialize)]
pub struct Target {
    pub mode: WriteMode,
    pub path: String,
    /// Octal string like "0600". Defaults to 0600 when unset (tight by
    /// default; harmless for non-secret values too).
    #[serde(default)]
    pub perm: Option<String>,
    /// `create` only: permit clobbering an existing file.
    #[serde(default)]
    pub overwrite: bool,
    /// `substitute` only: the exact token to replace (must occur once).
    #[serde(default)]
    pub placeholder: Option<String>,
}

/// Per-field result handed back to the agent. For a `secret` field the value
/// is absent by construction — only this status crosses the wire.
#[derive(Debug, Serialize)]
pub struct WriteOutcome {
    pub written: bool,
    /// Human-legible resolved destination (the absolute local path written).
    pub target: String,
    pub bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl WriteOutcome {
    fn ok(target: String, bytes: usize) -> Self {
        Self { written: true, target, bytes, error: None }
    }
    fn fail(target: String, error: String) -> Self {
        Self { written: false, target, bytes: 0, error: Some(error) }
    }
    /// A field whose `target` spec couldn't even be parsed — no destination to
    /// name yet.
    pub fn invalid(error: String) -> Self {
        Self { written: false, target: String::new(), bytes: 0, error: Some(error) }
    }
}

/// Parse an octal permission string like "0600"/"600" into mode bits.
fn parse_perm(s: &str) -> Option<u32> {
    let t = s.trim().trim_start_matches("0o");
    u32::from_str_radix(t, 8).ok()
}

/// Reject obviously-unsafe target paths (NULs, control chars, empty). The
/// local write goes through `std::fs`, not a shell, so this is a sanity guard
/// rather than an injection defense — but it keeps the approval string the
/// user sees unambiguous.
pub fn is_sane_target_path(p: &str) -> bool {
    !p.is_empty() && p.len() <= 4096 && p.bytes().all(|b| b >= 0x20 && b != 0x7f)
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

/// Atomically write `bytes` to `path` (tmp in the same dir + rename), applying
/// `perm` before the rename so a secret never sits world-readable even briefly.
fn atomic_write(path: &Path, bytes: &[u8], perm: Option<u32>) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| "target has no parent directory".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create dir {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".aiui-write-{}.tmp", uuid::Uuid::new_v4()));
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| format!("create temp {}: {e}", tmp.display()))?;
        #[cfg(unix)]
        if let Some(mode) = perm {
            use std::os::unix::fs::PermissionsExt;
            let _ = f.set_permissions(std::fs::Permissions::from_mode(mode));
        }
        if let Err(e) = f.write_all(bytes) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("write temp: {e}"));
        }
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename into place: {e}")
    })
}

/// Replace exactly one occurrence of `placeholder`. Errors on 0 or >1.
pub fn substitute_once(haystack: &str, placeholder: &str, value: &str) -> Result<String, String> {
    if placeholder.is_empty() {
        return Err("substitute mode requires a non-empty 'placeholder'".into());
    }
    match haystack.matches(placeholder).count() {
        1 => Ok(haystack.replacen(placeholder, value, 1)),
        0 => Err(format!("placeholder '{placeholder}' not found in target file")),
        n => Err(format!("placeholder '{placeholder}' found {n}× (must be exactly 1)")),
    }
}

/// Write `value` to the field's `target` as a local file operation. Never logs
/// `value`. Returns the [`WriteOutcome`] (the only thing that may reach the
/// agent for a secret).
pub fn write_local(value: &str, target: &Target) -> WriteOutcome {
    if !is_sane_target_path(&target.path) {
        return WriteOutcome::fail(target.path.clone(), "invalid target path".into());
    }
    let path = expand_tilde(&target.path);
    let display = path.display().to_string();
    let perm = target.perm.as_deref().and_then(parse_perm).or(Some(0o600));
    match target.mode {
        WriteMode::Create => {
            if path.exists() && !target.overwrite {
                return WriteOutcome::fail(
                    display,
                    "file exists and overwrite is false (mode: create)".into(),
                );
            }
            match atomic_write(&path, value.as_bytes(), perm) {
                Ok(()) => WriteOutcome::ok(display, value.len()),
                Err(e) => WriteOutcome::fail(display, e),
            }
        }
        WriteMode::Substitute => {
            let placeholder = match target.placeholder.as_deref() {
                Some(p) => p,
                None => {
                    return WriteOutcome::fail(
                        display,
                        "substitute mode requires 'placeholder'".into(),
                    )
                }
            };
            let existing = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => return WriteOutcome::fail(display, format!("read target: {e}")),
            };
            match substitute_once(&existing, placeholder, value) {
                Ok(updated) => {
                    let bytes = updated.len();
                    match atomic_write(&path, updated.as_bytes(), perm) {
                        Ok(()) => WriteOutcome::ok(display, bytes),
                        Err(e) => WriteOutcome::fail(display, e),
                    }
                }
                Err(e) => WriteOutcome::fail(display, e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_perm_octal() {
        assert_eq!(parse_perm("0600"), Some(0o600));
        assert_eq!(parse_perm("600"), Some(0o600));
        assert_eq!(parse_perm("not-octal"), None);
    }

    #[test]
    fn sane_target_path_basic() {
        assert!(is_sane_target_path("~/.config/aiui/token"));
        assert!(is_sane_target_path("/Users/me/.github_tokens/byte5ai"));
        assert!(!is_sane_target_path(""));
        assert!(!is_sane_target_path("a\nb"));
        assert!(!is_sane_target_path("a\0b"));
    }

    #[test]
    fn substitute_once_requires_exactly_one() {
        assert_eq!(substitute_once("a TOKEN b", "TOKEN", "X").unwrap(), "a X b");
        assert!(substitute_once("no marker", "TOKEN", "X").is_err());
        assert!(substitute_once("TOKEN TOKEN", "TOKEN", "X").is_err());
        assert!(substitute_once("x", "", "X").is_err());
    }

    #[test]
    fn create_writes_and_refuses_clobber() {
        let dir = std::env::temp_dir().join(format!("aiui-fw-{}", uuid::Uuid::new_v4()));
        let path = dir.join("sub").join("key");
        let target = Target {
            mode: WriteMode::Create,
            path: path.to_string_lossy().into_owned(),
            perm: Some("0600".into()),
            overwrite: false,
            placeholder: None,
        };
        let out = write_local("s3cr3t", &target);
        assert!(out.written, "first create: {:?}", out.error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "s3cr3t");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "perm applied");
        }
        let out2 = write_local("other", &target);
        assert!(!out2.written && out2.error.is_some(), "refuses clobber");
        let target_ow = Target { overwrite: true, ..target };
        let out3 = write_local("new", &target_ow);
        assert!(out3.written);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn substitute_replaces_placeholder() {
        let dir = std::env::temp_dir().join(format!("aiui-fw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, "token: __PAT__\nother: 1\n").unwrap();
        let target = Target {
            mode: WriteMode::Substitute,
            path: path.to_string_lossy().into_owned(),
            perm: None,
            overwrite: false,
            placeholder: Some("__PAT__".into()),
        };
        let out = write_local("ghp_xxx", &target);
        assert!(out.written, "{:?}", out.error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "token: ghp_xxx\nother: 1\n");
        std::fs::remove_dir_all(&dir).ok();
    }
}
