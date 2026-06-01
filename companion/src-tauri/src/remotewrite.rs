//! Issue #135 — remote leg of the typed-input file write.
//!
//! For an SSH session the agent's host is the registered remote. aiui writes
//! there with the *user's* SSH identity (the same path it used to `scp` the
//! pairing token during remote setup), so the value travels keyboard → Mac →
//! `scp` → remote file, never through the agent context.
//!
//! Safety contract (mirrors `push_token_to_remote`):
//! - `host_alias` is validated by `is_valid_host_alias` and passed after `--`
//!   so it can never land in ssh/scp option position (#52 option-injection).
//! - `path` is pre-validated by `filewrite::is_safe_target_path` (no shell
//!   metacharacters/globs), so the remote-shell expansion of the destination
//!   is limited to a benign `~` home expansion.
//! - The value is staged in a local temp file (mode applied before transfer)
//!   and `scp -p` preserves the mode on the remote — the secret is never
//!   passed on a command line and never logged.

use crate::proc_ext::no_window;
use std::process::Command;

/// `create` a file on `host_alias` at `remote_path` with `value`. Honors the
/// existence precondition (refuse unless `overwrite`). Returns the byte count
/// written, or a human error string. Never logs `value`.
pub fn remote_create(
    host_alias: &str,
    remote_path: &str,
    value: &str,
    overwrite: bool,
    perm: Option<&str>,
) -> Result<usize, String> {
    if !crate::setup::is_valid_host_alias(host_alias) {
        return Err(format!("refusing unsafe host alias '{host_alias}'"));
    }
    if !crate::filewrite::is_safe_target_path(remote_path) {
        return Err("unsafe remote target path".into());
    }

    // Existence precondition for create (unless overwrite). `test -e` exits 0
    // when the path exists; `~` is expanded by the remote shell as intended.
    if !overwrite {
        let probe = no_window(Command::new("ssh").args([
            "-o",
            "BatchMode=yes",
            "--",
            host_alias,
            &format!("test -e {remote_path}"),
        ]))
        .output()
        .map_err(|e| format!("ssh (existence check) could not start: {e}"))?;
        if probe.status.success() {
            return Err("remote file exists and overwrite is false (mode: create)".into());
        }
    }

    // Ensure the remote parent directory exists.
    if let Some(parent) = remote_parent(remote_path) {
        let mk = no_window(Command::new("ssh").args([
            "-o",
            "BatchMode=yes",
            "--",
            host_alias,
            &format!("mkdir -p {parent}"),
        ]))
        .output()
        .map_err(|e| format!("ssh (mkdir) could not start: {e}"))?;
        if !mk.status.success() {
            return Err(format!(
                "remote mkdir failed: {}",
                String::from_utf8_lossy(&mk.stderr).trim()
            ));
        }
    }

    // Stage the value in a local temp file with the requested mode, then scp -p
    // so the mode rides along. Temp lives in the OS temp dir and is removed
    // regardless of outcome.
    let tmp = std::env::temp_dir().join(format!(".aiui-remote-{}.tmp", uuid::Uuid::new_v4()));
    let mode = perm
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0o"), 8).ok())
        .unwrap_or(0o600);
    if let Err(e) = stage_temp(&tmp, value.as_bytes(), mode) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    let dest = format!("{host_alias}:{remote_path}");
    let scp = no_window(Command::new("scp").args([
        "-p",
        "-o",
        "BatchMode=yes",
        "--",
        &tmp.to_string_lossy(),
        &dest,
    ]))
    .output();
    let _ = std::fs::remove_file(&tmp);

    match scp {
        Err(e) => Err(format!("scp could not start: {e}")),
        Ok(o) if !o.status.success() => Err(format!(
            "scp to {dest} failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Ok(_) => Ok(value.len()),
    }
}

/// The remote parent directory of `path` (everything up to the last `/`).
/// `None` when the path has no directory component.
fn remote_parent(path: &str) -> Option<String> {
    path.rfind('/').map(|i| {
        if i == 0 {
            "/".to_string()
        } else {
            path[..i].to_string()
        }
    })
}

#[cfg(unix)]
fn stage_temp(path: &std::path::Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let mut f = std::fs::File::create(path).map_err(|e| format!("stage temp: {e}"))?;
    f.set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("chmod temp: {e}"))?;
    f.write_all(bytes).map_err(|e| format!("write temp: {e}"))?;
    f.sync_all().ok();
    Ok(())
}

#[cfg(not(unix))]
fn stage_temp(path: &std::path::Path, bytes: &[u8], _mode: u32) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("stage temp: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_parent_extracts_dir() {
        assert_eq!(remote_parent("~/.config/aiui/token").as_deref(), Some("~/.config/aiui"));
        assert_eq!(remote_parent("/etc/foo").as_deref(), Some("/etc"));
        assert_eq!(remote_parent("/top").as_deref(), Some("/"));
        assert_eq!(remote_parent("bare"), None);
    }

    #[test]
    fn remote_create_rejects_bad_alias_and_path() {
        assert!(remote_create("-evil@host", "~/x", "v", false, None).is_err());
        assert!(remote_create("good@host", "~/x; rm -rf ~", "v", false, None).is_err());
    }
}
