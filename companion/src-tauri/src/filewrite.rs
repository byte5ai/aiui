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
//!
//! Contract shared with the Python bridge's `_write_local_target` (issue
//! #199 — the two used to disagree, which meant the same spec did different
//! things depending on which module served the session):
//! - **Path rule.** `path` must be absolute or `~/`-rooted. A relative path
//!   has no stable cwd to resolve against (the Finder-launched app's is `/`,
//!   the bridge's is the agent's), and `~user/` is not portable across the
//!   two implementations — both are rejected rather than written somewhere
//!   unpredictable. Same rule `upload`'s `target_dir` already enforces.
//! - **Symlinks are followed.** The parent is canonicalised and an existing
//!   final symlink is resolved, so `substitute` edits the file the link
//!   points at instead of replacing the link with a regular file. The
//!   outcome reports that resolved path.
//! - **Permissions.** `create` defaults to `0600` (tight by default — a
//!   credential must never land world-readable). `substitute` is *editing a
//!   file the user already owns*, so it keeps that file's existing mode; an
//!   explicit `perm` wins in both modes. The applied mode is reported back
//!   in [`WriteOutcome::mode`] so a permission change is never invisible.

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
    /// Octal string like "0600". When unset, `create` defaults to 0600
    /// (tight by default; harmless for non-secret values too) and
    /// `substitute` keeps the destination's existing mode (#199).
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
    /// The octal mode actually applied ("0644"), so a permission change is
    /// never invisible (#199). Absent on non-POSIX hosts, where mode bits
    /// are meaningless and the file inherits default ACLs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl WriteOutcome {
    fn ok(target: String, bytes: usize, mode: Option<String>) -> Self {
        Self { written: true, target, bytes, mode, error: None }
    }
    fn fail(target: String, error: String) -> Self {
        Self { written: false, target, bytes: 0, mode: None, error: Some(error) }
    }
    /// A field whose `target` spec couldn't even be parsed — no destination to
    /// name yet.
    pub fn invalid(error: String) -> Self {
        Self { written: false, target: String::new(), bytes: 0, mode: None, error: Some(error) }
    }
}

/// Parse an octal permission string like "0600"/"600" into mode bits.
fn parse_perm(s: &str) -> Option<u32> {
    let t = s.trim().trim_start_matches("0o");
    u32::from_str_radix(t, 8).ok()
}

/// Why a target path is unusable, or `None` when it is fine. Two concerns:
/// obviously-unsafe strings (NULs, control chars, empty) — the local write
/// goes through `std::fs`, not a shell, so that half is a sanity guard rather
/// than an injection defense — and the #199 path rule, which keeps the
/// approval string the user sees an actual destination.
pub fn target_path_error(p: &str) -> Option<String> {
    if p.is_empty() || p.len() > 4096 || !p.bytes().all(|b| b >= 0x20 && b != 0x7f) {
        return Some("invalid target path".into());
    }
    // `has_root` in addition to `is_absolute` so a POSIX-style "/etc/x" is
    // accepted identically on Windows, where `is_absolute` also wants a
    // drive prefix — the two bridges must agree on what they accept.
    let path = Path::new(p);
    if p.starts_with("~/") || path.is_absolute() || path.has_root() {
        return None;
    }
    Some(format!(
        "target path must be an absolute or ~/-rooted path, got '{p}'"
    ))
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

/// Resolve the destination a write will really land on (#199).
///
/// `fs::canonicalize` on the whole path is wrong here: for `create` the file
/// does not exist yet and it would fail with ENOENT. So canonicalise the
/// *parent* and re-join the file name, then follow the final component while
/// it is an existing symlink. That way `substitute` reads and rewrites the
/// file the link points at, instead of `rename`-ing a fresh regular file over
/// the link and leaving the real config untouched.
fn resolve_target(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    // Bounded, so a symlink cycle can't spin here.
    for _ in 0..32 {
        let base = match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => match parent.canonicalize() {
                Ok(c) => c.join(name),
                // Parent doesn't exist yet (the normal `create`-into-a-fresh-
                // tree case): nothing to resolve, use the path as given.
                Err(_) => cur.clone(),
            },
            _ => cur.clone(),
        };
        let is_link = std::fs::symlink_metadata(&base)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if !is_link {
            return base;
        }
        match std::fs::read_link(&base) {
            Ok(dest) if dest.is_absolute() => cur = dest,
            Ok(dest) => match base.parent() {
                Some(p) => cur = p.join(dest),
                None => return base,
            },
            Err(_) => return base,
        }
    }
    cur
}

/// The destination string a `target.path` resolves to on THIS host — what the
/// approval line should show, and what the outcome later reports (#199).
pub fn resolve_display(raw_path: &str) -> String {
    resolve_target(&expand_tilde(raw_path)).display().to_string()
}

/// The destination's current mode bits, or `None` where they don't exist
/// (non-POSIX host, or no such file).
#[cfg(unix)]
fn existing_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn existing_mode(_path: &Path) -> Option<u32> {
    None
}

/// Render the applied mode for the outcome. `None` off Unix, mirroring the
/// `#[cfg(unix)]` guard on the chmod itself.
#[cfg(unix)]
fn mode_str(perm: Option<u32>) -> Option<String> {
    perm.map(|m| format!("{:04o}", m & 0o7777))
}

#[cfg(not(unix))]
fn mode_str(_perm: Option<u32>) -> Option<String> {
    None
}

/// Atomically write `bytes` to `path` (tmp in the same dir + rename), applying
/// `perm` before the rename so a secret never sits world-readable even briefly.
fn atomic_write(path: &Path, bytes: &[u8], perm: Option<u32>) -> Result<(), String> {
    // `perm` is applied only on Unix (mode bits); on Windows the file inherits
    // default ACLs. Bind it so the param isn't flagged unused on non-Unix
    // (clippy -D warnings on the Windows target).
    #[cfg(not(unix))]
    let _ = perm;
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
    if let Some(why) = target_path_error(&target.path) {
        return WriteOutcome::fail(target.path.clone(), why);
    }
    // Resolve before anything else, so the read, the rename and the reported
    // destination all name the same real file even when `path` is a symlink.
    let path = resolve_target(&expand_tilde(&target.path));
    let display = path.display().to_string();
    // An empty credential is never a legitimate write, and truncating the
    // user's file is not a dialog's job (issue #177). Refusing here, before
    // the mode match, covers `create` (which would clobber under
    // `overwrite`) *and* `substitute` (which needs no `overwrite` and would
    // destroy the sentinel, making a retry impossible).
    if value.is_empty() {
        return WriteOutcome::fail(display, "refusing to write an empty value".into());
    }
    // #199: one default per mode. `create` is tight by default — dropping
    // that would hand the process umask (0644/0664) a fresh credential file.
    // `substitute` is editing a file the user already owns, so silently
    // re-chmod'ing it to 0600 broke the very service the write was for; it
    // inherits the destination's mode instead. An explicit `perm` wins.
    let perm = target
        .perm
        .as_deref()
        .and_then(parse_perm)
        .or_else(|| match target.mode {
            WriteMode::Create => Some(0o600),
            // The 0600 fallback only bites when the file is unreadable — in
            // which case `substitute` fails at the read below and never writes.
            WriteMode::Substitute => existing_mode(&path).or(Some(0o600)),
        });
    match target.mode {
        WriteMode::Create => {
            if path.exists() && !target.overwrite {
                return WriteOutcome::fail(
                    display,
                    "file exists and overwrite is false (mode: create)".into(),
                );
            }
            match atomic_write(&path, value.as_bytes(), perm) {
                Ok(()) => WriteOutcome::ok(display, value.len(), mode_str(perm)),
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
                        Ok(()) => WriteOutcome::ok(display, bytes, mode_str(perm)),
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

    /// Readable predicate over [`target_path_error`] for the path tests.
    fn is_sane_target_path(p: &str) -> bool {
        target_path_error(p).is_none()
    }

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
    fn target_path_must_be_absolute_or_tilde_rooted() {
        // #199: the two bridges resolved these differently — the Python one
        // against the agent's cwd / `~alice`, the companion against the GUI's
        // cwd (typically `/` for a Finder launch) / a literal `~alice` dir.
        // Neither destination is what the user approved, so both are out.
        assert!(is_sane_target_path("/abs/x"));
        assert!(is_sane_target_path("~/x"));
        assert!(!is_sane_target_path("notes/key"));
        assert!(!is_sane_target_path("~alice/key"));
        assert!(!is_sane_target_path("./key"));
        let why = target_path_error("notes/key").unwrap();
        assert!(why.contains("absolute or ~/-rooted"), "{why}");
        assert!(why.contains("notes/key"), "names the offending path: {why}");
        // And it comes back as a structured outcome, not a surprise write.
        let out = write_local(
            "v",
            &Target {
                mode: WriteMode::Create,
                path: "notes/key".into(),
                perm: None,
                overwrite: false,
                placeholder: None,
            },
        );
        assert!(!out.written);
        assert!(out.error.unwrap().contains("absolute or ~/-rooted"));
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
    fn create_refuses_empty_value() {
        // Issue #177: a blank field must never truncate an existing file, not
        // even with `overwrite: true` — the guard sits before the mode match.
        let dir = std::env::temp_dir().join(format!("aiui-fw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("token");
        std::fs::write(&path, "ghp_existing").unwrap();
        let target = Target {
            mode: WriteMode::Create,
            path: path.to_string_lossy().into_owned(),
            perm: Some("0600".into()),
            overwrite: true,
            placeholder: None,
        };
        let out = write_local("", &target);
        assert!(!out.written, "empty value must be refused");
        assert_eq!(out.bytes, 0);
        assert!(
            out.error.as_deref().unwrap_or("").contains("empty"),
            "error names the cause: {:?}",
            out.error
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "ghp_existing",
            "file is byte-identical to before"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn substitute_refuses_empty_value() {
        // Issue #177: the worse half — `substitute` needs no `overwrite`, and
        // an empty value would erase the sentinel, so even a retry fails.
        let dir = std::env::temp_dir().join(format!("aiui-fw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, "token: __AIUI_SECRET_PAT__\nother: 1\n").unwrap();
        let target = Target {
            mode: WriteMode::Substitute,
            path: path.to_string_lossy().into_owned(),
            perm: None,
            overwrite: false,
            placeholder: Some("__AIUI_SECRET_PAT__".into()),
        };
        let out = write_local("", &target);
        assert!(!out.written, "empty value must be refused");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("__AIUI_SECRET_PAT__"),
            "sentinel survives so a retry is still possible: {after:?}"
        );
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

    // --- issue #199: the two writers must agree, and never surprise the user

    #[test]
    #[cfg(unix)]
    fn substitute_preserves_existing_mode() {
        // The headline: a 0644 compose file read by a container running as
        // another uid came back 0600 and the service stopped starting — with
        // nothing in the outcome to connect it to the aiui write.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aiui-fw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("docker-compose.yml");
        std::fs::write(&path, "token: __PAT__\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let target = Target {
            mode: WriteMode::Substitute,
            path: path.to_string_lossy().into_owned(),
            perm: None,
            overwrite: false,
            placeholder: Some("__PAT__".into()),
        };
        let out = write_local("ghp_x", &target);
        assert!(out.written, "{:?}", out.error);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o644, "substitute keeps the file's own mode");
        assert_eq!(out.mode.as_deref(), Some("0644"), "and says so");

        // An explicit `perm` still wins.
        std::fs::write(&path, "token: __PAT__\n").unwrap();
        let target_perm = Target { perm: Some("0640".into()), ..target };
        let out2 = write_local("ghp_y", &target_perm);
        assert!(out2.written, "{:?}", out2.error);
        let mode2 = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode2, 0o640);
        assert_eq!(out2.mode.as_deref(), Some("0640"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn create_defaults_to_0600_without_perm() {
        // Pins the tight-by-default guarantee against a naive "just drop the
        // 0600 default" fix for the substitute bug — that would hand a fresh
        // credential file the process umask (0644/0664).
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aiui-fw-{}", uuid::Uuid::new_v4()));
        let path = dir.join("token");
        let target = Target {
            mode: WriteMode::Create,
            path: path.to_string_lossy().into_owned(),
            perm: None,
            overwrite: false,
            placeholder: None,
        };
        let out = write_local("ghp_secret", &target);
        assert!(out.written, "{:?}", out.error);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600, "create stays tight by default");
        assert_eq!(out.mode.as_deref(), Some("0600"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn substitute_writes_through_symlink() {
        // Before: the read followed the link but the rename landed ON it, so
        // the link became a regular file holding the secret and the real
        // config kept its untouched placeholder — reported as `written: true`.
        let dir = std::env::temp_dir().join(format!("aiui-fw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real.yml");
        let link = dir.join("link.yml");
        std::fs::write(&real, "token: __PAT__\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let target = Target {
            mode: WriteMode::Substitute,
            path: link.to_string_lossy().into_owned(),
            perm: None,
            overwrite: false,
            placeholder: Some("__PAT__".into()),
        };
        let out = write_local("ghp_x", &target);
        assert!(out.written, "{:?}", out.error);
        assert!(
            std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
            "the link must survive as a link"
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "token: ghp_x\n");
        let resolved = real.canonicalize().unwrap().display().to_string();
        assert_eq!(out.target, resolved, "the outcome names the real destination");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn outcome_reports_applied_mode() {
        let dir = std::env::temp_dir().join(format!("aiui-fw-{}", uuid::Uuid::new_v4()));
        let path = dir.join("token");
        let target = Target {
            mode: WriteMode::Create,
            path: path.to_string_lossy().into_owned(),
            perm: Some("0640".into()),
            overwrite: false,
            placeholder: None,
        };
        let out = write_local("v", &target);
        assert!(out.written, "{:?}", out.error);
        #[cfg(unix)]
        assert_eq!(out.mode.as_deref(), Some("0640"), "the octal actually applied");
        #[cfg(not(unix))]
        assert!(out.mode.is_none(), "mode bits are meaningless off POSIX");
        std::fs::remove_dir_all(&dir).ok();
    }
}
