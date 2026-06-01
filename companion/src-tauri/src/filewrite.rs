//! Issue #135 — typed input field with file-write (incl. `secret` mode).
//!
//! A `form` field may carry an optional `target`: on affirmative submit, aiui
//! writes the entered value to a file **on the host the agent runs on** —
//! locally on the Mac, or (for an SSH session) back to the registered remote
//! via `scp`. For a `secret` field the value is written *only*; it is never
//! returned to the agent, so it never enters the LLM transcript.
//!
//! This is a **QoL convenience, not a security guarantee** (see #135): the
//! point is to stop agents from guessing fragile shell one-liners to stash a
//! value. The real security property here is the *confused-deputy* guard:
//! aiui writes with the user's own SSH/file identity, so every write target
//! is (a) constrained to the agent's own host — never an arbitrary
//! `attacker@evil` — and (b) shown to the user, whose affirmative click on
//! the form button IS the per-operation approval.
//!
//! ## Modes (explicit, never inferred from file existence)
//!
//! - `create` — write the raw value. Precondition: file absent, unless
//!   `overwrite: true`. (Inferring create-vs-append from existence is a
//!   footgun: a path typo silently flips behaviour.)
//! - `substitute` — file present, a caller-named `placeholder` occurs exactly
//!   once; replace it with the value. Format-agnostic string substitution.
//!
//! ## Destination — always the agent's host
//!
//! - Local session (`session_origin` absent) → local path on the Mac.
//! - Remote session (`session_origin` set by the Python bridge) → the one
//!   registered remote it maps to, via `scp`/`ssh` with the user's SSH
//!   identity. The host alias is validated (`is_valid_host_alias`, `--`
//!   end-of-options) so the agent cannot redirect the write off-host.
//!
//! v1 covers local `create`+`substitute` and remote `create`. Remote
//! `substitute` (read-modify-write over ssh) is v1.1 — it returns a clear
//! "not yet supported" error rather than a half-write.

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
    /// Octal string like "0600". Applied to the created/updated file. For a
    /// `secret` we default to 0600 when unset; otherwise the process umask
    /// applies.
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
    /// Human-legible resolved destination, e.g. `byte5host:~/.config/foo/key`
    /// or `/Users/me/.config/foo/key` — what the user should have seen.
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

/// Parse an octal permission string like "0600"/"600" into a mode bits value.
fn parse_perm(s: &str) -> Option<u32> {
    let t = s.trim().trim_start_matches("0o");
    u32::from_str_radix(t, 8).ok()
}

/// Reject target paths carrying shell metacharacters. Local writes never go
/// through a shell, but the remote `scp`/`ssh` path is interpreted by the
/// remote shell — so the same conservative charset guards both, and keeps the
/// approval string the user sees unambiguous. Allows the characters real
/// config paths use; rejects everything that could break out.
pub fn is_safe_target_path(p: &str) -> bool {
    if p.is_empty() || p.len() > 4096 {
        return false;
    }
    // No NULs, no whitespace/newlines, no shell metacharacters or globs.
    p.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'~' | b'@' | b'+' | b':')
    })
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
/// `perm` if given. The temp file is created with the target perm *before* the
/// rename so a secret never sits world-readable even briefly.
fn atomic_local_write(path: &Path, bytes: &[u8], perm: Option<u32>) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| "target has no parent directory".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create dir {}: {e}", dir.display()))?;
    // Unique temp name in the same dir (same filesystem → atomic rename).
    let tmp = dir.join(format!(".aiui-write-{}.tmp", uuid::Uuid::new_v4()));
    // Scope the write so the file is closed before we chmod/rename.
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| format!("create temp {}: {e}", tmp.display()))?;
        #[cfg(unix)]
        if let Some(mode) = perm {
            use std::os::unix::fs::PermissionsExt;
            let _ = f.set_permissions(std::fs::Permissions::from_mode(mode));
        }
        f.write_all(bytes).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("write temp: {e}")
        })?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename into place: {e}")
    })?;
    Ok(())
}

/// Replace exactly one occurrence of `placeholder` in `haystack`. Errors on 0
/// (typo/already-substituted) or >1 (ambiguous) — never a partial/`all` write.
pub fn substitute_once(haystack: &str, placeholder: &str, value: &str) -> Result<String, String> {
    if placeholder.is_empty() {
        return Err("substitute mode requires a non-empty 'placeholder'".into());
    }
    let count = haystack.matches(placeholder).count();
    match count {
        1 => Ok(haystack.replacen(placeholder, value, 1)),
        0 => Err(format!("placeholder '{placeholder}' not found in target file")),
        n => Err(format!("placeholder '{placeholder}' found {n}× (must be exactly 1)")),
    }
}

/// Resolve the destination and perform the write. `value` is the raw entered
/// string. `session_origin` is `None`/empty for a local Mac session, or the
/// remote's hostname for an SSH session. `registered_remotes` is the user's
/// `remotes.json` list, used to map a remote session to a validated alias.
///
/// Never logs `value`. Returns a [`WriteOutcome`] (the only thing that may
/// reach the agent for a secret).
pub fn resolve_and_write(
    value: &str,
    target: &Target,
    session_origin: Option<&str>,
    registered_remotes: &[String],
) -> WriteOutcome {
    if !is_safe_target_path(&target.path) {
        return WriteOutcome::fail(
            target.path.clone(),
            "unsafe target path (allowed: A-Za-z0-9 . _ - ~ @ + : /)".into(),
        );
    }
    let origin = session_origin.map(str::trim).filter(|s| !s.is_empty());
    match origin {
        // ---- Local Mac session ----------------------------------------
        None => {
            let path = expand_tilde(&target.path);
            let display = path.display().to_string();
            let perm = target
                .perm
                .as_deref()
                .and_then(parse_perm)
                .or(Some(0o600)); // default tight; harmless for non-secret
            match target.mode {
                WriteMode::Create => {
                    if path.exists() && !target.overwrite {
                        return WriteOutcome::fail(
                            display,
                            "file exists and overwrite is false (mode: create)".into(),
                        );
                    }
                    match atomic_local_write(&path, value.as_bytes(), perm) {
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
                        Err(e) => {
                            return WriteOutcome::fail(display, format!("read target: {e}"))
                        }
                    };
                    match substitute_once(&existing, placeholder, value) {
                        Ok(updated) => {
                            let bytes = updated.len();
                            match atomic_local_write(&path, updated.as_bytes(), perm) {
                                Ok(()) => WriteOutcome::ok(display, bytes),
                                Err(e) => WriteOutcome::fail(display, e),
                            }
                        }
                        Err(e) => WriteOutcome::fail(display, e),
                    }
                }
            }
        }
        // ---- Remote SSH session ----------------------------------------
        Some(origin) => {
            let alias = match map_origin_to_remote(origin, registered_remotes) {
                Ok(a) => a,
                Err(e) => return WriteOutcome::fail(format!("{origin}:{}", target.path), e),
            };
            let display = format!("{alias}:{}", target.path);
            match target.mode {
                WriteMode::Create => crate::remotewrite::remote_create(
                    &alias,
                    &target.path,
                    value,
                    target.overwrite,
                    target.perm.as_deref(),
                )
                .map(|bytes| WriteOutcome::ok(display.clone(), bytes))
                .unwrap_or_else(|e| WriteOutcome::fail(display, e)),
                WriteMode::Substitute => WriteOutcome::fail(
                    display,
                    "remote substitute is not supported yet (v1.1); use mode 'create' or run on the Mac"
                        .into(),
                ),
            }
        }
    }
}

/// Map a remote session's reported origin (its `socket.gethostname()`) to one
/// registered remote alias. Conservative on purpose — a wrong guess would
/// write a secret to the wrong host:
///
/// 1. exact alias match, or alias host-part match (`user@host` → `host`); else
/// 2. if exactly one remote is registered, use it; else
/// 3. refuse with a clear error (the user must disambiguate in Settings).
pub fn map_origin_to_remote(origin: &str, remotes: &[String]) -> Result<String, String> {
    let origin_lc = origin.to_ascii_lowercase();
    // Exact, or host-part of `user@host`, match.
    for r in remotes {
        let host_part = r.rsplit('@').next().unwrap_or(r);
        let alias_host = host_part.split(':').next().unwrap_or(host_part);
        let matches = r.eq_ignore_ascii_case(origin)
            || alias_host.eq_ignore_ascii_case(origin)
            || alias_host
                .to_ascii_lowercase()
                .starts_with(&format!("{origin_lc}."));
        if matches && crate::setup::is_valid_host_alias(r) {
            return Ok(r.clone());
        }
    }
    match remotes.len() {
        0 => Err(format!(
            "no registered remotes — cannot map session origin '{origin}' to a write host (add it in aiui Settings)"
        )),
        1 if crate::setup::is_valid_host_alias(&remotes[0]) => Ok(remotes[0].clone()),
        _ => Err(format!(
            "session origin '{origin}' does not match a single registered remote — refusing to guess (register/disambiguate in aiui Settings)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_perm_octal() {
        assert_eq!(parse_perm("0600"), Some(0o600));
        assert_eq!(parse_perm("600"), Some(0o600));
        assert_eq!(parse_perm("0644"), Some(0o644));
        assert_eq!(parse_perm("not-octal"), None);
    }

    #[test]
    fn safe_target_path_rejects_metacharacters() {
        assert!(is_safe_target_path("~/.config/aiui/token"));
        assert!(is_safe_target_path("/Users/me/.github_tokens/byte5ai"));
        assert!(!is_safe_target_path("~/x; rm -rf ~"));
        assert!(!is_safe_target_path("~/x $(whoami)"));
        assert!(!is_safe_target_path("~/x\nevil"));
        assert!(!is_safe_target_path("~/x`id`"));
        assert!(!is_safe_target_path(""));
    }

    #[test]
    fn substitute_once_requires_exactly_one() {
        assert_eq!(substitute_once("a TOKEN b", "TOKEN", "X").unwrap(), "a X b");
        assert!(substitute_once("no marker", "TOKEN", "X").is_err());
        assert!(substitute_once("TOKEN TOKEN", "TOKEN", "X").is_err());
        assert!(substitute_once("x", "", "X").is_err());
    }

    #[test]
    fn local_create_writes_and_refuses_clobber() {
        let dir = std::env::temp_dir().join(format!("aiui-fw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sub").join("key");
        let target = Target {
            mode: WriteMode::Create,
            path: path.to_string_lossy().into_owned(),
            perm: Some("0600".into()),
            overwrite: false,
            placeholder: None,
        };
        let out = resolve_and_write("s3cr3t", &target, None, &[]);
        assert!(out.written, "first create: {:?}", out.error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "s3cr3t");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "perm applied");
        }
        // Second create without overwrite must refuse.
        let out2 = resolve_and_write("other", &target, None, &[]);
        assert!(!out2.written && out2.error.is_some());
        // ...unless overwrite is set.
        let target_ow = Target { overwrite: true, ..target };
        let out3 = resolve_and_write("new", &target_ow, None, &[]);
        assert!(out3.written);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn local_substitute_replaces_placeholder() {
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
        let out = resolve_and_write("ghp_xxx", &target, None, &[]);
        assert!(out.written, "{:?}", out.error);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "token: ghp_xxx\nother: 1\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn map_origin_prefers_match_then_single() {
        // Host-part match.
        assert_eq!(
            map_origin_to_remote("macmini", &["customer@macmini".into()]).unwrap(),
            "customer@macmini"
        );
        // Single registered remote → used even if name differs.
        assert_eq!(
            map_origin_to_remote("whatever", &["dev.example.com".into()]).unwrap(),
            "dev.example.com"
        );
        // Ambiguous (>1, no match) → refuse.
        assert!(map_origin_to_remote(
            "nomatch",
            &["a.example.com".into(), "b.example.com".into()]
        )
        .is_err());
        // No remotes → refuse.
        assert!(map_origin_to_remote("x", &[]).is_err());
    }

    #[test]
    fn remote_substitute_is_rejected_v1() {
        let target = Target {
            mode: WriteMode::Substitute,
            path: "~/.config/foo".into(),
            perm: None,
            overwrite: false,
            placeholder: Some("X".into()),
        };
        let out = resolve_and_write("v", &target, Some("dev.example.com"), &["dev.example.com".into()]);
        assert!(!out.written);
        assert!(out.error.unwrap().contains("not supported yet"));
    }
}
