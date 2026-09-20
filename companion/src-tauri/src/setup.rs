use serde::Serialize;
use serde_json::{Map, Value};
use crate::fsutil::atomic_write;
use crate::proc_ext::no_window;
use std::fs;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
pub struct StepResult {
    pub ok: bool,
    pub message: String,
    pub details: Option<String>,
}

fn home() -> PathBuf {
    dirs::home_dir().expect("home dir")
}

/// Path to Claude Desktop's user config file. Differs per OS:
///
/// - macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
/// - Windows: `%APPDATA%\Claude\claude_desktop_config.json` —
///   `dirs::config_dir()` resolves Roaming AppData on Windows.
/// - Linux: same XDG-style layout as macOS for completeness.
fn claude_desktop_config_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let base = dirs::config_dir().unwrap_or_else(home);
        base.join("Claude").join("claude_desktop_config.json")
    }
    #[cfg(not(target_os = "windows"))]
    {
        home()
            .join("Library")
            .join("Application Support")
            .join("Claude")
            .join("claude_desktop_config.json")
    }
}

/// Path to Claude Code's user config file: `~/.claude.json` on all OSes.
///
/// #183: one function per host computes that host's path, and every entry
/// point — patch, remove, health-check — goes through it. The rule is what
/// keeps patch and remove pointed at the same file; a test that only
/// exercises the `_at` cores cannot catch a wrapper that rebuilds the path
/// by hand (which is exactly how `remove_claude_desktop_config` came to
/// hardcode the macOS path and never remove anything on Windows).
fn claude_code_config_path() -> PathBuf {
    home().join(".claude.json")
}

/// Copy `path` aside before we rewrite it. Returns the backup's path so the
/// caller can name it in `StepResult.details` — a backup the user cannot find
/// is not a backup (#182).
///
/// The suffix is **appended** rather than replacing the extension, so
/// `~/.claude.json` yields `~/.claude.json.bak.<ts>` and not the misleading
/// `~/.claude.bak.<ts>`, which looked like a backup of a different file and
/// did not match what the remote script already produced. Milliseconds,
/// because two writes in the same second used to silently overwrite each
/// other's backup.
fn backup(path: &Path) -> std::io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".bak.{ts}"));
    let bak = path.with_file_name(name);
    fs::copy(path, &bak)?;
    prune_backups(path);
    Ok(Some(bak))
}

/// Render a backup path for `StepResult.details`, so the user can actually
/// find the copy we made before rewriting their file (#182).
fn backup_detail(bak: Option<PathBuf>) -> Option<String> {
    bak.map(|p| format!("Backup: {}", p.display()))
}

/// Keep at most [`MAX_BACKUPS`] `<file>.bak.*` siblings per target.
///
/// These are full copies of credential-bearing configs (`~/.claude.json`
/// carries OAuth data and other servers' `env` blocks), and one was dropped
/// on every GUI launch that changed anything — an unbounded pile nobody
/// ever looked at. Also matches the legacy `<stem>.bak.*` shape for one
/// release, so strays written by older builds get swept too.
fn prune_backups(path: &Path) {
    const MAX_BACKUPS: usize = 5;
    let Some(dir) = path.parent() else { return };
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let stem = path.file_stem().and_then(|n| n.to_str()).unwrap_or("");
    let new_prefix = format!("{file_name}.bak.");
    let legacy_prefix = format!("{stem}.bak.");
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| {
                    n.starts_with(&new_prefix)
                        || (!stem.is_empty() && n.starts_with(&legacy_prefix))
                })
                .unwrap_or(false)
        })
        .collect();
    if found.len() <= MAX_BACKUPS {
        return;
    }
    // Sort by mtime, oldest first; fall back to the name (which carries the
    // timestamp) when mtime is unavailable.
    found.sort_by_key(|p| {
        fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH)
    });
    let excess = found.len() - MAX_BACKUPS;
    for old in found.into_iter().take(excess) {
        let _ = fs::remove_file(old);
    }
}

/// Set `command`/`args` on the `aiui` entry **without discarding the rest**.
///
/// #182: the old code built a fresh `{command, args}` object and overwrote
/// whatever was there, so an `env` block, a `disabled` flag or any other key
/// the user (or a future Claude Code version) added to the aiui entry
/// vanished on the next launch. Only the two keys we actually own are
/// touched. The `uvx` → native-binary migration still works, because it
/// overwrites those same two values.
fn upsert_aiui_entry(servers: &mut Map<String, Value>, app_binary_path: &str) {
    let mut entry = servers
        .get("aiui")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    entry.insert("command".into(), Value::String(app_binary_path.to_string()));
    entry.insert(
        "args".into(),
        Value::Array(vec![Value::String("--mcp-stdio".into())]),
    );
    servers.insert("aiui".into(), Value::Object(entry));
}

/// Is the stored entry already pointing at this binary with our args?
///
/// #182: compares **only** the two keys we own. The old
/// `existing_entry == Some(&entry)` compared whole objects, so an entry
/// carrying any extra key could never compare equal — meaning a rewrite,
/// and a fresh `.bak`, on every single launch.
fn aiui_entry_is_current(entry: Option<&Value>, app_binary_path: &str) -> bool {
    let Some(obj) = entry.and_then(|v| v.as_object()) else {
        return false;
    };
    let command_ok = obj.get("command").and_then(|v| v.as_str()) == Some(app_binary_path);
    let args_ok = obj
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| a.len() == 1 && a[0].as_str() == Some("--mcp-stdio"))
        .unwrap_or(false);
    command_ok && args_ok
}

/// Read a JSON host config.
///
/// `Ok(None)` = the file is absent. An **empty** file parses as an empty
/// object, which is genuinely safe. A parse error is an `Err` and never an
/// empty object (#182): treating unparsable content as `{}` meant the next
/// write replaced the user's entire file — every MCP server they had
/// configured, every project entry, the OAuth block — with a document
/// containing only aiui. A trailing comma was enough.
fn read_json_config(path: &Path) -> Result<Option<Value>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    if raw.trim().is_empty() {
        return Ok(Some(Value::Object(Map::new())));
    }
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub fn patch_claude_desktop_config(app_binary_path: &str) -> StepResult {
    patch_claude_desktop_config_at(&claude_desktop_config_path(), app_binary_path)
}

/// The body of [`patch_claude_desktop_config`], with the target file passed
/// in (#183). Splitting the path off is what makes this write path testable
/// at all: the wrapper resolves the user's real config, the core takes a
/// path, so tests drive it against a temp directory instead of rewriting the
/// developer's — or the CI runner's — own Claude Desktop config.
fn patch_claude_desktop_config_at(path: &Path, app_binary_path: &str) -> StepResult {
    // #182: a parse error must never be laundered into an empty object —
    // that replaced the user's whole config with one containing only aiui.
    let existing: Value = match read_json_config(path) {
        Ok(Some(v)) => v,
        Ok(None) => Value::Object(Map::new()),
        Err(e) => {
            return StepResult {
                ok: false,
                message: format!(
                    "{} is not valid JSON — left untouched",
                    path.display()
                ),
                details: Some(e),
            }
        }
    };

    let mut root = existing.as_object().cloned().unwrap_or_default();
    let mut servers = root
        .get("mcpServers")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    // Migration: ≤ v0.4.5 wrote the entry under the key `aiui-local`. That
    // mismatched `~/.claude.json`'s `aiui` key, breaking slash commands like
    // `/aiui:test-dialog` in Claude Desktop (would have needed
    // `/aiui-local:test-dialog`). Unify on `aiui` and drop the old entry on
    // every patch — idempotent for fresh installs, healing for upgrades.
    let had_legacy = servers.contains_key("aiui-local");
    servers.remove("aiui-local");
    let was_present = servers.contains_key("aiui");
    upsert_aiui_entry(&mut servers, app_binary_path);
    root.insert("mcpServers".into(), Value::Object(servers));

    let bak = match backup(path) {
        Ok(b) => b,
        Err(e) => {
            return StepResult {
                ok: false,
                message: "Backup fehlgeschlagen".into(),
                details: Some(e.to_string()),
            }
        }
    };

    let pretty = serde_json::to_string_pretty(&Value::Object(root)).unwrap();
    match atomic_write(path, pretty.as_bytes()) {
        Ok(_) => StepResult {
            ok: true,
            message: match (was_present, had_legacy) {
                (true, _) => "aiui in Claude Desktop Config aktualisiert.".into(),
                (false, true) => {
                    "aiui in Claude Desktop Config eingetragen — alter `aiui-local`-Eintrag migriert.".into()
                }
                (false, false) => "aiui zu Claude Desktop Config hinzugefügt.".into(),
            },
            details: Some(match backup_detail(bak) {
                Some(b) => format!("Datei: {} — {b}", path.display()),
                None => format!("Datei: {}", path.display()),
            }),
        },
        Err(e) => StepResult {
            ok: false,
            message: "Schreiben fehlgeschlagen".into(),
            details: Some(e.to_string()),
        },
    }
}

/// Parse a user@host input. Returns (block_match_name, optional user).
///
/// - `"user@host"` → `("host", Some("user"))`
/// - `"host"`      → `("host", None)`
///
/// The block match name is what goes into the `Host <name>` line so SSH
/// config matches on the hostname part of any connection to that host.
fn split_user_host(input: &str) -> (&str, Option<&str>) {
    match input.split_once('@') {
        Some((u, h)) if !u.is_empty() && !h.is_empty() => (h, Some(u)),
        _ => (input, None),
    }
}

/// Validate that a remote host alias is safe to pass to ssh/scp without it
/// being misinterpreted as an option (`-oProxyCommand=…` style injection).
///
/// Allows `[A-Za-z0-9._-]` for the host part and the same plus `+` for the
/// user part — i.e. real RFC-style hostnames, IPs, IPv6 in brackets, and
/// SSH-config aliases. Rejects whitespace, control characters, leading
/// `-`, shell metacharacters, and anything > 253 chars per part.
///
/// Public so the `add_remote` Tauri command can validate at the boundary
/// before ever spawning ssh.
pub fn is_valid_host_alias(input: &str) -> bool {
    if input.is_empty() || input.len() > 256 {
        return false;
    }
    if input.starts_with('-') {
        return false;
    }
    let (host, user) = split_user_host(input);
    if host.is_empty() || host.starts_with('-') || host.len() > 253 {
        return false;
    }
    if let Some(u) = user {
        if u.is_empty() || u.starts_with('-') || u.len() > 253 {
            return false;
        }
        if !u.bytes().all(host_alias_user_byte_ok) {
            return false;
        }
    }
    // Host part: allow alphanumerics, dot, hyphen, underscore, colon (IPv6
    // separators), and the bracketing chars `[]` for `[::1]`-style input.
    host.bytes().all(host_alias_host_byte_ok)
}

fn host_alias_user_byte_ok(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+')
}

fn host_alias_host_byte_ok(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'[' | b']')
}

#[cfg(test)]
mod host_alias_tests {
    use super::is_valid_host_alias;

    #[test]
    fn accepts_plain_host() { assert!(is_valid_host_alias("macmini")); }
    #[test]
    fn accepts_user_at_host() { assert!(is_valid_host_alias("customer@macmini")); }
    #[test]
    fn accepts_dotted_host() { assert!(is_valid_host_alias("dev.example.com")); }
    #[test]
    fn accepts_ipv4() { assert!(is_valid_host_alias("user@10.0.0.1")); }
    #[test]
    fn accepts_ipv6_bracketed() { assert!(is_valid_host_alias("[::1]")); }

    #[test]
    fn rejects_leading_dash() { assert!(!is_valid_host_alias("-oProxyCommand=foo")); }
    #[test]
    fn rejects_user_leading_dash() { assert!(!is_valid_host_alias("-evil@host")); }
    #[test]
    fn rejects_host_leading_dash() { assert!(!is_valid_host_alias("user@-evil")); }
    #[test]
    fn rejects_whitespace() { assert!(!is_valid_host_alias("foo bar")); }
    #[test]
    fn rejects_quotes() { assert!(!is_valid_host_alias("foo\"bar")); }
    #[test]
    fn rejects_semicolon() { assert!(!is_valid_host_alias("foo;rm -rf /")); }
    #[test]
    fn rejects_empty() { assert!(!is_valid_host_alias("")); }
    #[test]
    fn rejects_only_at() { assert!(!is_valid_host_alias("@")); }
    #[test]
    fn rejects_pipe() { assert!(!is_valid_host_alias("a|b")); }
    #[test]
    fn rejects_newline() { assert!(!is_valid_host_alias("a\nb")); }

}

// Note: an earlier version of aiui patched ~/.ssh/config with a
// RemoteForward line. The tunnel manager now owns the forward directly, so
// only the remove path remains (to clean up legacy installs).

pub fn push_token_to_remote(host_alias: &str, token_path: &str) -> StepResult {
    if !is_valid_host_alias(host_alias) {
        return StepResult {
            ok: false,
            message: format!("Refusing unsafe host alias '{host_alias}'"),
            details: Some("Only [A-Za-z0-9._-] in host, no leading '-', no shell metacharacters.".into()),
        };
    }
    // ensure remote dir. `--` keeps host_alias out of ssh option position
    // even if validation regresses one day.
    let out1 = no_window(
        Command::new("ssh").args([
            "-o",
            "BatchMode=yes",
            "--",
            host_alias,
            "mkdir -p ~/.config/aiui && chmod 700 ~/.config/aiui",
        ]),
    )
    .output();
    match out1 {
        Err(e) => {
            return StepResult {
                ok: false,
                message: "ssh konnte nicht gestartet werden".into(),
                details: Some(e.to_string()),
            }
        }
        Ok(o) if !o.status.success() => {
            return StepResult {
                ok: false,
                message: format!("ssh {host_alias} 'mkdir -p …' fehlgeschlagen"),
                details: Some(String::from_utf8_lossy(&o.stderr).to_string()),
            }
        }
        _ => {}
    }

    let dest = format!("{host_alias}:.config/aiui/token");
    let out2 = no_window(Command::new("scp").arg(token_path).arg(&dest)).output();
    match out2 {
        Err(e) => StepResult {
            ok: false,
            message: "scp konnte nicht gestartet werden".into(),
            details: Some(e.to_string()),
        },
        Ok(o) if !o.status.success() => StepResult {
            ok: false,
            message: format!("scp {token_path} {dest} fehlgeschlagen"),
            details: Some(String::from_utf8_lossy(&o.stderr).to_string()),
        },
        Ok(_) => StepResult {
            ok: true,
            message: format!("Token nach {host_alias}:~/.config/aiui/token übertragen."),
            details: None,
        },
    }
}

pub fn app_binary_path() -> String {
    // when bundled, the executable sits at aiui.app/Contents/MacOS/aiui on
    // macOS and at the NSIS install dir on Windows. `current_exe` is the
    // truth source on both — the per-OS string is only a last-ditch
    // fallback for environments where `current_exe` errors out (rare).
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| default_app_binary_path().to_string())
}

#[cfg(target_os = "macos")]
fn default_app_binary_path() -> &'static str {
    "/Applications/aiui.app/Contents/MacOS/aiui"
}

#[cfg(target_os = "windows")]
fn default_app_binary_path() -> &'static str {
    // Default per-user NSIS install location. If the user picked a custom
    // path during install, `current_exe` returns the truth and this
    // fallback never fires.
    r"C:\Users\Public\aiui\aiui.exe"
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn default_app_binary_path() -> &'static str {
    "/usr/local/bin/aiui"
}

pub fn is_claude_config_current(app_binary_path: &str) -> bool {
    is_claude_config_current_at(&claude_desktop_config_path(), app_binary_path)
}

/// The body of [`is_claude_config_current`], with the file passed in (#183).
///
/// Both JSON hosts store the entry under the same `/mcpServers/aiui`
/// pointer, so this is also the core behind
/// [`is_claude_code_config_current`] — the check is about the file's shape,
/// not about which host wrote it.
fn is_claude_config_current_at(path: &Path, app_binary_path: &str) -> bool {
    let Ok(s) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(&s) else {
        return false;
    };
    let Some(entry) = v.pointer("/mcpServers/aiui") else {
        return false;
    };
    entry
        .get("command")
        .and_then(|v| v.as_str())
        .map(|c| c == app_binary_path)
        .unwrap_or(false)
}

/// Same shape as `is_claude_config_current`, but for Claude Code's
/// `~/.claude.json`. Used by the welcome health-check so we can tell the
/// user *which* Claude variant is wired up vs. missing.
pub fn is_claude_code_config_current(app_binary_path: &str) -> bool {
    is_claude_code_config_current_at(&claude_code_config_path(), app_binary_path)
}

/// The body of [`is_claude_code_config_current`], with the file passed in
/// (#183). Named after its host rather than folded into the caller so the
/// wrapper/core pairing is uniform across all three hosts; the predicate
/// itself is shared with Claude Desktop.
fn is_claude_code_config_current_at(path: &Path, app_binary_path: &str) -> bool {
    is_claude_config_current_at(path, app_binary_path)
}

// ─── Installed-host detection (#168) ────────────────────────────────────
//
// aiui registers itself only with the MCP hosts actually present on this
// machine — no phantom config files for hosts the user doesn't use.
// Detection is "installed", not "running": setup runs when the host may be
// closed, so a running-check would skip a host the user really uses.

/// Whether Claude Desktop is installed here — its app bundle or its config
/// directory exists. Deliberately NOT [`is_claude_desktop_running`]:
/// "running" is too strict for install-time registration.
pub fn is_claude_desktop_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        if PathBuf::from("/Applications/Claude.app").exists() {
            return true;
        }
    }
    claude_desktop_config_path()
        .parent()
        .map(|d| d.exists())
        .unwrap_or(false)
}

/// Whether Claude Code (the CLI) is set up here — its global config file or
/// dot-directory exists, or the `claude` binary is on `PATH`.
pub fn is_claude_code_installed() -> bool {
    home().join(".claude.json").exists()
        || home().join(".claude").is_dir()
        || binary_on_path("claude")
}

/// Whether OpenAI Codex is set up here — its config directory exists or the
/// `codex` binary is on `PATH`.
pub fn is_codex_installed() -> bool {
    codex_config_path()
        .parent()
        .map(|d| d.exists())
        .unwrap_or(false)
        || binary_on_path("codex")
}

/// Cheap `$PATH` scan for an executable by name — no subprocess, unlike a
/// `which` shell-out. On Windows also tries the usual executable extensions.
fn binary_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    std::env::split_paths(&paths)
        .any(|dir| exts.iter().any(|ext| dir.join(format!("{name}{ext}")).is_file()))
}

// ─── Codex (OpenAI) registration (#168) ─────────────────────────────────
//
// The Codex counterpart of `patch_claude_code_config`, but Codex reads TOML
// (`~/.codex/config.toml`) rather than JSON. We edit it with `toml_edit` so
// the user's own comments and other MCP servers survive untouched. The entry
// points at the same bundled `--mcp-stdio` server as the Claude hosts —
// self-contained, no `uv`/`uvx`.

/// Path to OpenAI Codex's user config: `~/.codex/config.toml` on all OSes.
fn codex_config_path() -> PathBuf {
    home().join(".codex").join("config.toml")
}

/// Upsert `[mcp_servers.aiui]` into an existing Codex `config.toml` (or an
/// empty document), pointing `command` at the bundled binary with
/// `--mcp-stdio`. Everything else — comments, formatting, other
/// `mcp_servers` — is preserved by `toml_edit`. Overwriting the entry is also
/// the migration path for a legacy `command = "uvx"` install. Idempotent:
/// re-running with the same binary yields identical output.
fn codex_toml_upsert(
    existing: Option<&str>,
    app_binary_path: &str,
) -> Result<String, toml_edit::TomlError> {
    use toml_edit::{value, Array, DocumentMut, Item, Table};
    let mut doc: DocumentMut = match existing {
        Some(s) => s.parse()?,
        None => DocumentMut::new(),
    };

    let mut args = Array::new();
    args.push("--mcp-stdio");

    // Ensure a `[mcp_servers]` parent table exists — implicit, so we never emit
    // an empty `[mcp_servers]` header.
    if doc.get("mcp_servers").and_then(|i| i.as_table()).is_none() {
        let mut parent = Table::new();
        parent.set_implicit(true);
        doc.insert("mcp_servers", Item::Table(parent));
    }

    // #182: if an `[mcp_servers.aiui]` table is already there, set only the
    // two keys we own so the user's other keys — and toml_edit's decor, i.e.
    // their comments and spacing — survive. Build a fresh table only when
    // there is none.
    let has_table = doc["mcp_servers"]
        .get("aiui")
        .map(|i| i.is_table())
        .unwrap_or(false);
    if has_table {
        doc["mcp_servers"]["aiui"]["command"] = value(app_binary_path);
        doc["mcp_servers"]["aiui"]["args"] = value(args);
    } else {
        // Explicit table so it serializes as `[mcp_servers.aiui]` (the
        // conventional, readable form) rather than dotted keys.
        let mut aiui = Table::new();
        aiui["command"] = value(app_binary_path);
        aiui["args"] = value(args);
        doc["mcp_servers"]["aiui"] = Item::Table(aiui);
    }

    Ok(doc.to_string())
}

/// Register aiui in `~/.codex/config.toml` so Codex sees it as an MCP server.
/// Call only when [`is_codex_installed`] is true. Backs up first; leaves a
/// malformed user config untouched rather than clobbering it.
pub fn patch_codex_config(app_binary_path: &str) -> StepResult {
    patch_codex_config_at(&codex_config_path(), app_binary_path)
}

/// The body of [`patch_codex_config`], with the target file passed in
/// (#183) so tests can exercise the read → back up → write path against a
/// temp file. Only the pure string helper `codex_toml_upsert` was covered
/// before; the function that actually touches the user's config was not.
fn patch_codex_config_at(path: &Path, app_binary_path: &str) -> StepResult {
    let existing: Option<String> = match fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return StepResult {
                ok: false,
                message: "Could not read ~/.codex/config.toml".into(),
                details: Some(e.to_string()),
            }
        }
    };
    let new_toml = match codex_toml_upsert(existing.as_deref(), app_binary_path) {
        Ok(s) => s,
        Err(e) => {
            return StepResult {
                ok: false,
                message: "~/.codex/config.toml is not valid TOML — left untouched".into(),
                details: Some(e.to_string()),
            }
        }
    };
    // Idempotent: if the on-disk file already matches, don't rewrite it — and
    // don't drop a fresh timestamped `.bak` — on every GUI launch. Mirrors the
    // already-correct short-circuit in `patch_claude_code_config`. Because
    // `codex_toml_upsert` is stable (see the idempotency test), this settles
    // after at most one write.
    if existing.as_deref() == Some(new_toml.as_str()) {
        return StepResult {
            ok: true,
            message: "aiui already registered in ~/.codex/config.toml".into(),
            details: None,
        };
    }
    let was_new = existing.is_none();
    let bak = match backup(path) {
        Ok(b) => b,
        Err(e) => {
            return StepResult {
                ok: false,
                message: "~/.codex/config.toml backup failed".into(),
                details: Some(e.to_string()),
            }
        }
    };
    match atomic_write(path, new_toml.as_bytes()) {
        Ok(_) => StepResult {
            ok: true,
            message: if was_new {
                "Added aiui to ~/.codex/config.toml — available in every Codex session".into()
            } else {
                "Updated aiui entry in ~/.codex/config.toml".into()
            },
            details: Some(match backup_detail(bak) {
                Some(b) => format!("Datei: {} — {b}", path.display()),
                None => format!("Datei: {}", path.display()),
            }),
        },
        Err(e) => StepResult {
            ok: false,
            message: "Writing ~/.codex/config.toml failed".into(),
            details: Some(e.to_string()),
        },
    }
}

/// Remove aiui from `~/.codex/config.toml` on uninstall — unconditional (safe
/// if no entry exists); preserves the rest of the user's config.
pub fn remove_codex_config() -> StepResult {
    let path = codex_config_path();
    if !path.exists() {
        return StepResult {
            ok: true,
            message: "~/.codex/config.toml does not exist".into(),
            details: None,
        };
    }
    let Ok(s) = fs::read_to_string(&path) else {
        return StepResult {
            ok: true,
            message: "~/.codex/config.toml unreadable, skipping".into(),
            details: None,
        };
    };
    let mut doc: toml_edit::DocumentMut = match s.parse() {
        Ok(d) => d,
        Err(_) => {
            return StepResult {
                ok: true,
                message: "~/.codex/config.toml not valid TOML, skipping".into(),
                details: None,
            }
        }
    };
    let had = doc
        .get("mcp_servers")
        .and_then(|i| i.as_table())
        .map(|t| t.contains_key("aiui"))
        .unwrap_or(false);
    if let Some(servers) = doc.get_mut("mcp_servers").and_then(|i| i.as_table_mut()) {
        servers.remove("aiui");
    }
    let bak = match backup(&path) {
        Ok(b) => b,
        Err(e) => {
            return StepResult {
                ok: false,
                message: "~/.codex/config.toml backup failed".into(),
                details: Some(e.to_string()),
            }
        }
    };
    match atomic_write(&path, doc.to_string().as_bytes()) {
        Ok(_) => StepResult {
            ok: true,
            message: if had {
                "Removed aiui from ~/.codex/config.toml".into()
            } else {
                "aiui was not registered in ~/.codex/config.toml".into()
            },
            details: backup_detail(bak),
        },
        Err(e) => StepResult {
            ok: false,
            message: "Writing ~/.codex/config.toml failed".into(),
            details: Some(e.to_string()),
        },
    }
}

/// The substring that identifies a running Claude Desktop on macOS: the
/// bundle's executable path, which is the same wherever the app is
/// installed (#180). Private so it cannot drift from the matcher below.
#[cfg(target_os = "macos")]
const CLAUDE_DESKTOP_PROC_MATCH: &str = "Claude.app/Contents/MacOS/Claude";

/// Is this `pgrep -af` line a running Claude Desktop?
///
/// The authority for the liveness probe, not a description of it: the
/// `pgrep` pattern is a cheap pre-filter and this decides. Pure, so the
/// rule can be unit-tested over fixtures without a running Claude Desktop —
/// the same treatment `is_aiui_ssh_ntr_for_port` gets in `housekeeping.rs`.
/// The two properties that matter: it finds the app wherever it is
/// installed, and it never matches the `claude` CLI.
#[cfg(target_os = "macos")]
pub fn is_claude_desktop_proc(cmdline: &str) -> bool {
    cmdline.contains(CLAUDE_DESKTOP_PROC_MATCH)
}

/// Best-effort check whether the Claude Desktop application is currently
/// running. Used to switch the "Restart Claude Desktop" button label
/// between Start / Restart so we don't ask the user to "restart"
/// something that isn't running.
///
/// Pure read-only. Per-OS process probe:
///
/// - macOS: `pgrep -af Claude.app` pre-filters, then
///   [`is_claude_desktop_proc`] decides — the bundle executable, at any
///   install location, never the `claude` CLI.
/// - Windows: `tasklist /FI "IMAGENAME eq Claude.exe" /NH` lists running
///   processes by image name; non-empty stdout means at least one match.
///
/// If the probe errors out we assume "not running" rather than blocking
/// the UI.
pub fn is_claude_desktop_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        // #180: match the bundle EXECUTABLE, not a fixed install prefix. The
        // old `-f /Applications/Claude.app/` missed an install in
        // `~/Applications` (which `is_claude_desktop_installed` already
        // supports via the config dir), so the app looked permanently dead
        // to its own liveness probe — and the exit gate inverted.
        //
        // `pgrep -af Claude.app` is only the cheap pre-filter; the decision
        // is `is_claude_desktop_proc`, so the matching rule lives in one
        // unit-tested place rather than inside an argv string.
        let out = std::process::Command::new("pgrep")
            .args(["-af", "Claude.app"])
            .output();
        match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(is_claude_desktop_proc),
            _ => false,
        }
    }
    #[cfg(target_os = "windows")]
    {
        let out = no_window(
            std::process::Command::new("tasklist").args([
                "/FI",
                "IMAGENAME eq Claude.exe",
                "/NH",
            ]),
        )
        .output();
        match out {
            Ok(o) if o.status.success() => {
                // tasklist prints "INFO: No tasks are running …" on a
                // miss instead of empty stdout, so look for the actual
                // image name (case-insensitive).
                let s = String::from_utf8_lossy(&o.stdout).to_lowercase();
                s.contains("claude.exe")
            }
            _ => false,
        }
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        false
    }
}

/// Patches `~/.claude.json` so Claude Code CLI sees `aiui` as a globally
/// available MCP server — every session, every project, no per-project
/// .mcp.json required.
///
/// Since v0.3.0 the entry points directly at the aiui.app binary with
/// `--mcp-stdio`. That eliminates the `uv`/`uvx`/`pipx` dependency from
/// the onboarding path — the app bundle already ships the MCP server as
/// native code.
///
/// Auto-migrates legacy `uvx aiui-mcp` entries from ≤ v0.2.x installs.
pub fn patch_claude_code_config(app_binary_path: &str) -> StepResult {
    patch_claude_code_config_at(&claude_code_config_path(), app_binary_path)
}

/// The body of [`patch_claude_code_config`], with the target file passed in
/// (#183). `~/.claude.json` is the highest-stakes file aiui writes — it
/// carries every other MCP server plus Claude Code's whole per-project
/// state — so the write path needs tests, and tests need a path they may
/// clobber.
fn patch_claude_code_config_at(path: &Path, app_binary_path: &str) -> StepResult {
    // #182: see patch_claude_desktop_config — unparsable is a hard stop.
    let existing: Value = match read_json_config(path) {
        Ok(Some(v)) => v,
        Ok(None) => Value::Object(Map::new()),
        Err(e) => {
            return StepResult {
                ok: false,
                message: "~/.claude.json is not valid JSON — left untouched".into(),
                details: Some(e),
            }
        }
    };

    let mut root = existing.as_object().cloned().unwrap_or_default();
    let mut servers = root
        .get("mcpServers")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    let existing_entry = servers.get("aiui");
    let previous_kind = classify_aiui_entry(existing_entry);
    let was_present = existing_entry.is_some();
    // #182: compare only the keys we own, so an entry carrying an `env`
    // block isn't rewritten (and re-backed-up) on every launch.
    let already_correct = aiui_entry_is_current(existing_entry, app_binary_path);
    if already_correct {
        return StepResult {
            ok: true,
            message: "aiui already registered in ~/.claude.json".into(),
            details: None,
        };
    }
    upsert_aiui_entry(&mut servers, app_binary_path);
    root.insert("mcpServers".into(), Value::Object(servers));

    let bak = match backup(path) {
        Ok(b) => b,
        Err(e) => {
            return StepResult {
                ok: false,
                message: "~/.claude.json backup failed".into(),
                details: Some(e.to_string()),
            }
        }
    };
    let pretty = serde_json::to_string_pretty(&Value::Object(root)).unwrap();
    match atomic_write(path, pretty.as_bytes()) {
        Ok(_) => {
            let msg = match (was_present, previous_kind) {
                (true, Some(AiuiEntryKind::LegacyUvx)) => {
                    "Migrated aiui in ~/.claude.json from `uvx aiui-mcp` to the native app binary — no uv dependency required anymore".into()
                }
                (true, _) => "Updated aiui entry in ~/.claude.json".into(),
                (false, _) => {
                    "Added aiui to ~/.claude.json — available in every Claude Code session".into()
                }
            };
            StepResult {
                ok: true,
                message: msg,
                details: backup_detail(bak),
            }
        }
        Err(e) => StepResult {
            ok: false,
            message: "Writing ~/.claude.json failed".into(),
            details: Some(e.to_string()),
        },
    }
}

enum AiuiEntryKind {
    LegacyUvx,
    Other,
}

fn classify_aiui_entry(entry: Option<&Value>) -> Option<AiuiEntryKind> {
    let entry = entry?;
    let cmd = entry.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let args = entry
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if cmd == "uvx" && args.first().map(String::as_str) == Some("aiui-mcp") {
        Some(AiuiEntryKind::LegacyUvx)
    } else {
        Some(AiuiEntryKind::Other)
    }
}

pub fn remove_claude_code_config() -> StepResult {
    remove_claude_code_config_at(&claude_code_config_path())
}

/// The body of [`remove_claude_code_config`], with the target file passed
/// in (#183).
fn remove_claude_code_config_at(path: &Path) -> StepResult {
    if !path.exists() {
        return StepResult {
            ok: true,
            message: "~/.claude.json does not exist".into(),
            details: None,
        };
    }
    // #182: on an unparsable file, stop before touching it and tell the user
    // to remove the entry by hand. `ok: true` on purpose — a full uninstall
    // must not turn red over a file aiui did not break. Mirrors
    // remove_codex_config.
    let mut v: Value = match read_json_config(path) {
        Ok(Some(v)) => v,
        Ok(None) => {
            return StepResult {
                ok: true,
                message: "~/.claude.json does not exist".into(),
                details: None,
            }
        }
        Err(e) => {
            return StepResult {
                ok: true,
                message: "~/.claude.json is not valid JSON — left untouched; \
                          remove the `aiui` entry under `mcpServers` by hand"
                    .into(),
                details: Some(e),
            }
        }
    };
    let had = v.pointer("/mcpServers/aiui").is_some();
    if let Some(servers) = v.get_mut("mcpServers").and_then(|x| x.as_object_mut()) {
        servers.remove("aiui");
    }
    let bak = match backup(path) {
        Ok(b) => b,
        Err(e) => {
            return StepResult {
                ok: false,
                message: "~/.claude.json backup failed".into(),
                details: Some(e.to_string()),
            }
        }
    };
    let pretty = serde_json::to_string_pretty(&v).unwrap();
    match atomic_write(path, pretty.as_bytes()) {
        Ok(_) => StepResult {
            ok: true,
            message: if had {
                "Removed aiui from ~/.claude.json".into()
            } else {
                "aiui was not registered in ~/.claude.json".into()
            },
            details: backup_detail(bak),
        },
        Err(e) => StepResult {
            ok: false,
            message: "Writing ~/.claude.json failed".into(),
            details: Some(e.to_string()),
        },
    }
}

pub fn remove_claude_desktop_config() -> StepResult {
    // #183: goes through `claude_desktop_config_path()` like the patcher.
    // It used to rebuild the macOS path inline, so on Windows uninstall
    // looked at a file that does not exist there, reported "nothing to do",
    // and left the aiui entry behind pointing at a deleted binary.
    remove_claude_desktop_config_at(&claude_desktop_config_path())
}

/// The body of [`remove_claude_desktop_config`], with the target file
/// passed in (#183).
fn remove_claude_desktop_config_at(path: &Path) -> StepResult {
    if !path.exists() {
        return StepResult {
            ok: true,
            message: "claude_desktop_config.json existiert nicht, nichts zu tun.".into(),
            details: None,
        };
    }
    // #182: see remove_claude_code_config — never rewrite a file we could
    // not parse.
    let mut v: Value = match read_json_config(path) {
        Ok(Some(v)) => v,
        Ok(None) => {
            return StepResult {
                ok: true,
                message: "claude_desktop_config.json existiert nicht, nichts zu tun.".into(),
                details: None,
            }
        }
        Err(e) => {
            return StepResult {
                ok: true,
                message: "claude_desktop_config.json is not valid JSON — left untouched; \
                          remove the `aiui` entry under `mcpServers` by hand"
                    .into(),
                details: Some(e),
            }
        }
    };
    // Remove both the current `aiui` key and the legacy `aiui-local` key so
    // Uninstall always leaves a clean state regardless of which version
    // wrote the entry.
    let had = v.pointer("/mcpServers/aiui").is_some()
        || v.pointer("/mcpServers/aiui-local").is_some();
    if let Some(servers) = v.get_mut("mcpServers").and_then(|x| x.as_object_mut()) {
        servers.remove("aiui");
        servers.remove("aiui-local");
    }
    let bak = match backup(path) {
        Ok(b) => b,
        Err(e) => {
            return StepResult {
                ok: false,
                message: "Backup fehlgeschlagen".into(),
                details: Some(e.to_string()),
            }
        }
    };
    let pretty = serde_json::to_string_pretty(&v).unwrap();
    match atomic_write(path, pretty.as_bytes()) {
        Ok(_) => StepResult {
            ok: true,
            message: if had {
                "aiui aus Claude Desktop Config entfernt.".into()
            } else {
                "aiui war bereits nicht eingetragen.".into()
            },
            details: backup_detail(bak),
        },
        Err(e) => StepResult {
            ok: false,
            message: "Schreiben fehlgeschlagen".into(),
            details: Some(e.to_string()),
        },
    }
}

/// Removes our three lines (RemoteForward, ServerAliveInterval, ExitOnForwardFailure)
/// from a Host block. Keeps the Host block itself intact (user may have other config there).
pub fn remove_ssh_forward(host_alias: &str, port: u16) -> StepResult {
    let path = home().join(".ssh").join("config");
    let (match_name, _) = split_user_host(host_alias);
    let Ok(existing) = fs::read_to_string(&path) else {
        return StepResult {
            ok: true,
            message: "~/.ssh/config existiert nicht, nichts zu tun.".into(),
            details: None,
        };
    };

    let mut blocks: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut preamble_done = false;
    for line in existing.lines() {
        let t = line.trim_start();
        if t.starts_with("Host ") || t == "Host" {
            if !current.is_empty() || preamble_done {
                blocks.push(std::mem::take(&mut current));
            }
            preamble_done = true;
        }
        current.push(line.to_string());
    }
    if !current.is_empty() {
        blocks.push(current);
    }

    let mut changed = false;
    for block in blocks.iter_mut() {
        let matches = block.first().is_some_and(|l| {
            l.trim_start()
                .strip_prefix("Host ")
                .map(|rest| rest.split_whitespace().any(|a| a == match_name))
                .unwrap_or(false)
        });
        if !matches {
            continue;
        }
        let before = block.len();
        block.retain(|l| {
            let t = l.trim_start().to_lowercase();
            if t.starts_with("remoteforward")
                && t.contains(&format!("{port} localhost:{port}"))
            {
                return false;
            }
            if t.starts_with("serveraliveinterval 30") {
                return false;
            }
            if t.starts_with("exitonforwardfailure no") {
                return false;
            }
            true
        });
        if block.len() != before {
            changed = true;
        }
    }

    let out: String = blocks
        .into_iter()
        .map(|b| b.join("\n"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = if out.ends_with('\n') { out } else { format!("{out}\n") };

    let bak = match backup(&path) {
        Ok(b) => b,
        Err(e) => {
            return StepResult {
                ok: false,
                message: "Backup fehlgeschlagen".into(),
                details: Some(e.to_string()),
            }
        }
    };
    match atomic_write(&path, out.as_bytes()) {
        Ok(_) => StepResult {
            ok: true,
            message: if changed {
                format!("aiui-Einträge aus Host '{host_alias}' entfernt.")
            } else {
                format!("Host '{host_alias}' hatte keine aiui-Einträge.")
            },
            details: backup_detail(bak),
        },
        Err(e) => StepResult {
            ok: false,
            message: "Schreiben fehlgeschlagen".into(),
            details: Some(e.to_string()),
        },
    }
}

/// Patch ~/.claude.json on a remote host so aiui is available in every Claude
/// Code session there. Uses python3 for atomic JSON editing — avoids
/// shell-quoting pitfalls, universally available on macOS and Linux remotes.
///
/// Implementation note: the script is fed to `python3 -` over the ssh
/// connection's stdin. Earlier versions tried `python3 -c "$1" -- <script>`
/// in argv; the remote login shell expands `$1` to empty before python sees
/// anything (no positional-argument scope), so the patch silently no-op'd
/// while reporting success. Stdin avoids that whole class of shell-quoting
/// trap, and we additionally check that the script printed "ok" so any
/// future regression can't masquerade as success again.
/// Outcome of a `~/.claude.json` patch run on a remote — tells the
/// caller whether the file was actually rewritten or already carried
/// the expected pin. The caller uses this to decide whether to also
/// SIGTERM any in-flight mcp-stdio child on the remote (so the next
/// tool call respawns with the freshly pinned version).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteConfigPatch {
    /// File already carried the expected pin — no rewrite, no kill needed.
    AlreadyCurrent,
    /// File was rewritten — caller should sweep stale mcp-stdio children.
    Patched,
}

pub fn patch_claude_code_config_remote(
    host_alias: &str,
    uvx_path: Option<&str>,
    pinned_version: &str,
) -> (StepResult, Option<RemoteConfigPatch>) {
    // If we know the absolute uvx path from the reachability probe, use it.
    // JSON-escape via serde so paths with unusual characters don't break the
    // Python script's string literal.
    //
    // #184: when we DON'T know one, `None` no longer means "write the bare
    // name". The bare `uvx` depends on Claude Code's process PATH at spawn
    // time — fragile enough that the probe exists to avoid it — so
    // overwriting a working absolute path with it is a downgrade. The script
    // below keeps an existing resolvable command in that case and rewrites
    // only `args`, so the version pin is still enforced. The bare name is
    // written only when there is no usable entry at all, which is the same
    // position a fresh host without a successful probe was always in.
    let uvx_command_lit = serde_json::to_string(uvx_path.unwrap_or("uvx"))
        .unwrap_or_else(|_| "\"uvx\"".to_string());
    // Whether the literal above is a discovered absolute path or the
    // fallback. Drives the keep-what-works branch in the script.
    let command_is_known = if uvx_path.is_some() { "True" } else { "False" };
    // Pin to the exact aiui-mcp version that matches the local
    // companion. Without the pin, uvx silently caches whichever version
    // happened to be installed first on the remote — that's how a v0.3.1
    // mcp-stdio kept talking to a v0.4.27 companion (incident
    // 2026-04-30). With the pin, uvx is forced to fetch / install the
    // matching wheel before spawning. Idempotent: if the pin is already
    // correct, no rewrite happens (`ok:current`), and the caller skips
    // the kill-sweep on the remote.
    let pkg_spec = format!("aiui-mcp=={pinned_version}");
    let pkg_spec_lit = serde_json::to_string(&pkg_spec)
        .unwrap_or_else(|_| format!("\"{pkg_spec}\""));
    let script = format!(r#"{REMOTE_JSON_PREAMBLE}
servers = data.get("mcpServers") or {{}}
existing = servers.get("aiui") or {{}}
current_cmd = existing.get("command") if isinstance(existing, dict) else None

# #184: decide the command BEFORE comparing, so "we know of no path" never
# downgrades a working one. When the companion discovered an absolute path
# it wins. Otherwise keep whatever is already there if it looks resolvable
# (absolute and ending in /uvx) — the version pin in `args` is enforced
# either way. Only a host with no usable entry gets the bare name.
want_cmd = {uvx_command_lit}
if not {command_is_known}:
    if isinstance(current_cmd, str) and current_cmd.startswith("/") \
            and current_cmd.rstrip("/").endswith("/uvx"):
        want_cmd = current_cmd

if (current_cmd == want_cmd and existing.get("args") == [{pkg_spec_lit}]):
    print("ok:current")
    raise SystemExit(0)
backup()
# Keep any foreign keys on the entry (env, disabled, …) — set only ours.
entry = dict(existing) if isinstance(existing, dict) else {{}}
entry["command"] = want_cmd
entry["args"] = [{pkg_spec_lit}]
servers["aiui"] = entry
data["mcpServers"] = servers
save(data)
print("ok:patched")
"#);
    let script = script.as_str();
    let mut patch: Option<RemoteConfigPatch> = None;
    let step = run_remote_python(host_alias, script, "Patching ~/.claude.json", |stdout| {
        let trimmed = stdout.trim();
        match trimmed {
            "ok:patched" => {
                patch = Some(RemoteConfigPatch::Patched);
                StepResult {
                    ok: true,
                    message: format!("aiui pinned to {pinned_version} in ~/.claude.json on {host_alias}"),
                    details: None,
                }
            }
            "ok:current" => {
                patch = Some(RemoteConfigPatch::AlreadyCurrent);
                StepResult {
                    ok: true,
                    message: format!("aiui in ~/.claude.json on {host_alias} already pinned to {pinned_version}"),
                    details: None,
                }
            }
            "err:malformed" => StepResult {
                ok: false,
                message: format!(
                    "~/.claude.json on {host_alias} is not valid JSON — left untouched"
                ),
                details: Some(
                    "Fix the file on that host (or remove it) and register the remote again."
                        .into(),
                ),
            },
            other => StepResult {
                ok: false,
                message: format!("Patching ~/.claude.json on {host_alias} did not confirm 'ok'"),
                details: Some(format!("stdout: {other}")),
            },
        }
    });
    (step, patch)
}

// Step 2 removed `kill_remote_mcp_stdio` (an `ssh … pkill -f 'aiui-mcp'`).
// It existed to force a freshly-pinned version onto a running remote session,
// but `pkill -f` crashed live sessions mid-call (Claude Code does not respawn
// a disconnected MCP) and matched *every* aiui-mcp on the host — the remote
// twin of the 0.4.42 Cowork-kill, and outright unsafe once parallel sessions
// per remote are a requirement. The version pin in `~/.claude.json` now takes
// effect at the next natural spawn; live sessions finish on their current
// version. Deregistration (`remove_remote` / `uninstall_all`) relies on
// config-removal + natural session end, not a broad kill.

/// Result of a successful reachability probe — carries the absolute
/// uvx path discovered on the remote so subsequent setup steps can
/// embed it into `~/.claude.json` instead of relying on the remote's
/// PATH being right at Claude-Code-spawn time.
pub struct RemoteUvxLocation {
    pub uvx_path: String,
}

/// Probe whether `uvx aiui-mcp` actually works on the remote BEFORE we
/// persist the host. Without this check, `add_remote` happily writes the
/// `~/.claude.json` entry pointing at `uvx aiui-mcp` even on hosts where
/// `uv`/`uvx` aren't installed — Claude Code then errors at every tool
/// call with a confusing "command not found" the user has to chase
/// through logs.
///
/// Returns `(StepResult, Some(RemoteUvxLocation))` on success — the
/// uvx_path is the absolute path discovered on the remote, suitable for
/// embedding into the remote's `~/.claude.json`.
///
/// The probe script is piped via stdin (rather than passed as `bash -lc
/// <script>` argv), avoiding ssh's word-splitting of multi-line script
/// arguments. Same pattern as `run_remote_python` below.
///
/// uvx discovery walks four well-known install locations in addition to
/// `command -v uvx`, so brew-installed `uv` is found even if the remote's
/// bash login shell has a minimal PATH (the common case — `/opt/homebrew/bin`
/// is added by `brew shellenv` to `~/.zprofile`, not `~/.profile`).
pub fn check_remote_aiui_mcp(host_alias: &str) -> (StepResult, Option<RemoteUvxLocation>) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if !is_valid_host_alias(host_alias) {
        return (
            StepResult {
                ok: false,
                message: format!("Refusing unsafe host alias '{host_alias}'"),
                details: None,
            },
            None,
        );
    }

    let probe_script = r#"echo "STAGE:STARTED"
set +e

# STAGE:STARTED is the very first line so any earlier failure (set +e
# unexpected error, environment glitch, redirect-init issue) shows up
# as "no STAGE:STARTED" and we can tell the script never got past line
# one. Subsequent diagnostics about host/shell/user follow.
echo "STAGE:HOST $(hostname 2>/dev/null) shell:$0 user:$(id -un 2>/dev/null)"

# uv installs into one of these locations depending on installer:
#   /opt/homebrew/bin/uvx        Homebrew on Apple Silicon
#   /usr/local/bin/uvx           Homebrew on Intel / manual /usr/local install
#   $HOME/.local/bin/uvx         astral.sh/uv install script (Linux/macOS)
#   $HOME/.cargo/bin/uvx         `cargo install uv` (rare)
# Check all of them, plus PATH lookup, before declaring "not found".
UVX=""
for p in /opt/homebrew/bin/uvx /usr/local/bin/uvx "$HOME/.local/bin/uvx" "$HOME/.cargo/bin/uvx"; do
    if [ -x "$p" ]; then
        UVX="$p"
        break
    fi
done
if [ -z "$UVX" ]; then
    UVX="$(command -v uvx 2>/dev/null)"
fi
if [ -z "$UVX" ]; then
    echo "STAGE:NO_UVX" >&2
    echo "Searched: /opt/homebrew/bin, /usr/local/bin, ~/.local/bin, ~/.cargo/bin, and PATH" >&2
    echo "PATH=$PATH" >&2
    exit 11
fi

echo "STAGE:UVX_FOUND $UVX"

# Verify uvx itself is callable. Earlier versions tried `uvx aiui-mcp
# --help` here as a "does the package resolve from PyPI" probe — but
# aiui-mcp has no `--help` handler; the flag gets ignored and the full
# MCP server starts, blocking on stdin. Bash hangs on the child, ssh
# eventually truncates output, the script never reaches STAGE:OK.
# Better: trust that if `uvx` itself is healthy, `uvx aiui-mcp` will
# resolve at first-tool-call time — and fail there with a clear error
# from Claude Code if PyPI is unreachable. Cheap, idempotent, no
# server side-effects.
ERR_FILE="$(mktemp -t aiui-probe.XXXXXX)"
if ! "$UVX" --version >/dev/null 2>"$ERR_FILE"; then
    echo "STAGE:UVX_BROKEN" >&2
    cat "$ERR_FILE" >&2
    rm -f "$ERR_FILE"
    exit 13
fi
rm -f "$ERR_FILE"
echo "STAGE:OK"
"#;

    // Pipe the script via stdin to avoid ssh word-splitting multi-line
    // command arguments on the remote shell. Invocation choices:
    //   /bin/bash        absolute path — no PATH dependency before bash
    //                    initializes its own environment
    //   --login          source /etc/profile + ~/.profile; equivalent to
    //                    -l but unambiguously named
    //   -s               read script from stdin
    //   --               end-of-options sentinel; defends against a future
    //                    `--` interpretation of the first script byte
    //   -T               disable TTY allocation; ssh would refuse PTY when
    //                    stdin is a pipe anyway, but this is explicit
    let mut child = match no_window(
        Command::new("ssh")
            .args([
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "--",
                host_alias,
                "/bin/bash",
                "--login",
                "-s",
                "--",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StepResult {
                    ok: false,
                    message: format!("SSH-Verbindung zu {host_alias} schlug fehl"),
                    details: Some(format!(
                        "Konnte ssh nicht starten: {e}. Prüfe ~/.ssh/config und Schlüssel-Auth zum Host."
                    )),
                },
                None,
            );
        }
    };

    // Take stdin out of the Child explicitly and drop it after writing.
    // Dropping closes the pipe write-side, so bash sees EOF and exits
    // its read loop. wait_with_output() *should* drop stdin too via the
    // owned Self argument, but `as_mut()` keeps it alive in some
    // versions/configurations and that's the kind of subtle pipe-stays-
    // open bug that produces hangs or empty-output mysteries.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(probe_script.as_bytes());
        drop(stdin);
    }

    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return (
                StepResult {
                    ok: false,
                    message: format!("SSH zu {host_alias} brach ab"),
                    details: Some(format!("ssh wait error: {e}")),
                },
                None,
            );
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();

    if out.status.success() && stdout.contains("STAGE:OK") {
        let uvx_path = stdout
            .lines()
            .find_map(|l| l.strip_prefix("STAGE:UVX_FOUND ").map(str::to_string))
            .unwrap_or_default();
        return (
            StepResult {
                ok: true,
                message: format!("uvx aiui-mcp erreichbar auf {host_alias}"),
                details: if uvx_path.is_empty() {
                    None
                } else {
                    Some(format!("uvx-Pfad auf Remote: {uvx_path}"))
                },
            },
            if uvx_path.is_empty() {
                None
            } else {
                Some(RemoteUvxLocation { uvx_path })
            },
        );
    }

    let exit = out.status.code().unwrap_or(-1);
    let script_started = stdout.contains("STAGE:STARTED");
    // Collapse multi-line dumps to fit the inline error banner without
    // becoming a wall. Truncate generously enough to show the actual
    // failure detail, but cap so we don't break the layout.
    let truncate_at = 1500;
    let dump = |s: &str| -> String {
        let s = s.trim();
        if s.len() <= truncate_at {
            s.to_string()
        } else {
            format!("{}\n... (truncated, {} bytes total)", &s[..truncate_at], s.len())
        }
    };
    let (msg, hint) = match exit {
        11 => (
            format!("uv ist auf {host_alias} nicht installiert"),
            format!(
                "Auf dem Remote installieren: `curl -LsSf https://astral.sh/uv/install.sh | sh`. \
                 Remote-Diagnose:\n{}",
                dump(&stderr)
            ),
        ),
        13 => (
            format!("uvx auf {host_alias} ist nicht ausführbar"),
            format!(
                "uvx wurde gefunden, aber `uvx --version` schlug fehl. \
                 Vermutlich beschädigte uv-Installation. Detail vom Remote:\n{}",
                dump(&stderr)
            ),
        ),
        _ if !script_started => (
            format!("Probe-Script kam nicht beim Remote an (exit {exit})"),
            format!(
                "Der ssh-Aufruf war erfolgreich, aber das Probe-Script hat keine Markierung ausgegeben — \
                 vermutlich wird die Login-Shell auf {host_alias} durch eine ProxyCommand-, ForceCommand- \
                 oder andere ssh-Wrapper-Konfiguration umgeleitet, die unseren `bash -ls`-Aufruf \
                 nicht ausführt. Probier auf dem Mac: \
                 `ssh -o BatchMode=yes {host_alias} bash -ls </dev/null` — sollte ohne Fehler beenden.\n\n\
                 stdout vom ssh-Aufruf:\n{}\n\nstderr vom ssh-Aufruf:\n{}",
                dump(&stdout),
                dump(&stderr)
            ),
        ),
        _ => (
            format!("Pre-Flight-Check auf {host_alias} schlug fehl (exit {exit})"),
            format!(
                "Probe-Script lief an, hat aber kein STAGE:OK ausgegeben.\n\n\
                 stdout vom Remote:\n{}\n\nstderr vom Remote:\n{}",
                dump(&stdout),
                dump(&stderr)
            ),
        ),
    };
    (
        StepResult {
            ok: false,
            message: msg,
            details: Some(hint),
        },
        None,
    )
}

/// Run a Python script on a remote host via `ssh ... python3 -` with the
/// script piped on stdin. Captures stdout for the caller to verify a
/// success marker. We validate `host_alias` and additionally use `--` so
/// it can't slip into ssh option position even if validation regresses.
fn run_remote_python(
    host_alias: &str,
    script: &str,
    op: &str,
    on_success: impl FnOnce(&str) -> StepResult,
) -> StepResult {
    use std::io::Write;
    use std::process::Stdio;

    if !is_valid_host_alias(host_alias) {
        return StepResult {
            ok: false,
            message: format!("Refusing unsafe host alias '{host_alias}'"),
            details: None,
        };
    }

    let child = no_window(
        Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "--",
                host_alias,
                "python3 -",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            return StepResult {
                ok: false,
                message: format!("ssh {host_alias} could not start"),
                details: Some(e.to_string()),
            };
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(script.as_bytes()) {
            return StepResult {
                ok: false,
                message: format!("{op} on {host_alias}: stdin write failed"),
                details: Some(e.to_string()),
            };
        }
        // Drop stdin so python3 sees EOF and starts executing.
        drop(stdin);
    }

    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return StepResult {
                ok: false,
                message: format!("{op} on {host_alias}: wait failed"),
                details: Some(e.to_string()),
            };
        }
    };

    if !out.status.success() {
        return StepResult {
            ok: false,
            message: format!("{op} on {host_alias} failed"),
            details: Some(String::from_utf8_lossy(&out.stderr).to_string()),
        };
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    on_success(&stdout)
}

/// Parse-or-bail + timestamped backup + atomic write, shared verbatim by both
/// remote `~/.claude.json` scripts (#182).
///
/// Factored out because the drift between them is exactly how the remove path
/// ended up with no backup at all while the patch path had one. Both used to
/// swallow a parse error into `data = {}` and then `p.write_text(...)` — an
/// open-truncate-write over a file a live Claude Code session may also be
/// writing, which replaced the user's entire remote config with one
/// containing only aiui.
///
/// Defines: `p` (the config path), `data` (parsed, or a bail-out), plus
/// `backup()` and `save(data)`.
const REMOTE_JSON_PREAMBLE: &str = r#"
import json, os, pathlib, shutil, time
p = pathlib.Path.home() / ".claude.json"
data = {}
if p.exists():
    try:
        data = json.loads(p.read_text())
    except Exception:
        # Never launder an unparsable config into an empty dict: the next
        # write would replace every MCP server, project entry and the OAuth
        # block with a document containing only aiui.
        print("err:malformed")
        raise SystemExit(2)
    if not isinstance(data, dict):
        print("err:malformed")
        raise SystemExit(2)

def backup():
    if p.exists():
        ts = int(time.time() * 1000)
        shutil.copy(p, p.with_name(p.name + f".bak.{ts}"))
        baks = sorted(p.parent.glob(p.name + ".bak.*"), key=lambda f: f.stat().st_mtime)
        for old in baks[:-5]:
            try:
                old.unlink()
            except OSError:
                pass

def save(data):
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_name(p.name + ".aiui-tmp")
    tmp.write_text(json.dumps(data, indent=2))
    os.replace(tmp, p)
"#;

pub fn remove_claude_code_config_remote(host_alias: &str) -> StepResult {
    let script = format!(r#"{REMOTE_JSON_PREAMBLE}
if not p.exists():
    print("ok")
else:
    servers = data.get("mcpServers") or {{}}
    if "aiui" in servers:
        backup()
        servers.pop("aiui", None)
        data["mcpServers"] = servers
        save(data)
    print("ok")
"#);
    let script = script.as_str();
    run_remote_python(host_alias, script, "Removing aiui from ~/.claude.json", |stdout| {
        if stdout.trim() == "err:malformed" {
            // ok:true on purpose — a full uninstall must not turn red over a
            // file aiui did not break.
            return StepResult {
                ok: true,
                message: format!(
                    "~/.claude.json on {host_alias} is not valid JSON — left untouched"
                ),
                details: Some(
                    "Remove the `aiui` entry under `mcpServers` on that host by hand.".into(),
                ),
            };
        }
        let confirmed = stdout.trim() == "ok";
        StepResult {
            ok: confirmed,
            message: if confirmed {
                format!("Removed aiui from ~/.claude.json on {host_alias}")
            } else {
                format!("Removal on {host_alias} did not confirm 'ok'")
            },
            details: if confirmed {
                None
            } else {
                Some(format!("stdout: {}", stdout.trim()))
            },
        }
    })
}

/// Cheap liveness probe run before remote cleanup in the removal/uninstall
/// paths: a single `BatchMode` ssh that runs `true`. ssh exits 255 when it
/// cannot establish or authenticate the connection — an unreachable host, a
/// refused connection, or (the case that motivated this: the user removes a
/// host precisely *because* its key no longer works) "Permission denied".
/// Any exit code the remote command itself produced (0..=254) means we got
/// in, so the host counts as reachable. A spawn failure or a signal-killed
/// ssh (no exit code) is treated as unreachable — better to skip remote
/// cleanup than to spew errors we can do nothing about. `ConnectTimeout`
/// keeps a dead host from hanging the probe on the default TCP timeout.
pub fn host_reachable(host_alias: &str) -> bool {
    if !is_valid_host_alias(host_alias) {
        return false;
    }
    no_window(Command::new("ssh").args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=6",
        "--",
        host_alias,
        "true",
    ]))
    .output()
    .map(|o| matches!(o.status.code(), Some(c) if c != 255))
    .unwrap_or(false)
}

/// Informational step emitted in place of the three remote-cleanup steps when
/// a host is unreachable (see [`host_reachable`]). The user's action — remove
/// this host — has still succeeded locally (ssh forward + `remotes.json`),
/// which is all that is meaningful for a host aiui can no longer reach. We
/// say plainly what was left behind instead of reporting red "failed" lines
/// for remote files we have no way to touch.
pub fn remote_cleanup_skipped(host_alias: &str) -> StepResult {
    StepResult {
        ok: true,
        message: format!(
            "Host {host_alias} nicht erreichbar — lokal deregistriert, Remote-Cleanup übersprungen."
        ),
        details: Some(
            "Token, ~/.claude.json-Eintrag und Skill verbleiben auf dem Host (harmlos). \
             Sobald der Host wieder erreichbar ist, kann er dort manuell entfernt werden."
                .into(),
        ),
    }
}

pub fn remove_token_from_remote(host_alias: &str) -> StepResult {
    if !is_valid_host_alias(host_alias) {
        return StepResult {
            ok: false,
            message: format!("Refusing unsafe host alias '{host_alias}'"),
            details: None,
        };
    }
    let out = no_window(
        Command::new("ssh").args([
            "-o",
            "BatchMode=yes",
            "--",
            host_alias,
            "rm -f ~/.config/aiui/token",
        ]),
    )
    .output();
    match out {
        Err(e) => StepResult {
            ok: false,
            message: format!("ssh {host_alias} konnte nicht gestartet werden"),
            details: Some(e.to_string()),
        },
        Ok(o) if !o.status.success() => StepResult {
            ok: false,
            message: format!("Token-Löschung auf {host_alias} fehlgeschlagen"),
            details: Some(String::from_utf8_lossy(&o.stderr).to_string()),
        },
        Ok(_) => StepResult {
            ok: true,
            message: format!("Token auf {host_alias} gelöscht."),
            details: None,
        },
    }
}

/// `remotes.json` inside a given config dir. Pure, so the mapping can be
/// asserted without touching `$HOME` — and so nothing here can re-derive a
/// path of its own (#196).
pub(crate) fn remotes_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("remotes.json")
}

/// Where `remotes.json` lived before #196: always `~/.config/aiui`, whatever
/// the OS. Identical to the new path on macOS/Linux; on Windows it is
/// `%USERPROFILE%\.config\aiui` — a second state directory no aiui surface
/// ever named, while the token, `first_run_done` and `gui.lock` sat in
/// `%APPDATA%\aiui`.
fn legacy_remotes_path() -> PathBuf {
    home().join(".config").join("aiui").join("remotes.json")
}

/// The per-OS config dir, falling back to the legacy layout if it cannot be
/// resolved — `load_remotes`/`save_remotes` have no error channel of their
/// own, and the fallback is exactly what the old code did unconditionally.
fn config_dir_or_legacy() -> PathBuf {
    crate::config::config_dir().unwrap_or_else(|_| home().join(".config").join("aiui"))
}

fn remotes_path() -> PathBuf {
    remotes_path_in(&config_dir_or_legacy())
}

/// Move a pre-#196 `remotes.json` into the per-OS config dir, once.
///
/// Called exactly once from `run()`, before the first `load_remotes()` —
/// deliberately **not** from `load_remotes` itself, which runs on every 2 s
/// status tick and in the tunnel loops. Repointing the path without this
/// would silently empty the remotes list of every Windows install from
/// v0.10.1 onwards, and the tunnel `ensure` loops would stop reconnecting
/// registered hosts. On macOS/Linux both paths are the same file, so it is a
/// no-op by construction.
pub fn migrate_remotes_if_needed(config_dir: &Path) -> std::io::Result<bool> {
    migrate_remotes_from(&legacy_remotes_path(), &remotes_path_in(config_dir))
}

/// The testable core of [`migrate_remotes_if_needed`]: move `legacy` to
/// `new`, but only when `new` does not exist yet. An existing `new` always
/// wins — it is the file the running build reads and writes, and overwriting
/// it with older content would lose hosts rather than rescue them.
pub(crate) fn migrate_remotes_from(legacy: &Path, new: &Path) -> std::io::Result<bool> {
    if legacy == new || new.exists() || !legacy.exists() {
        return Ok(false);
    }
    let content = fs::read(legacy)?;
    atomic_write(new, &content)?;
    fs::remove_file(legacy)?;
    Ok(true)
}

pub fn load_remotes() -> Vec<String> {
    let p = remotes_path();
    if !p.exists() {
        return vec![];
    }
    fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

pub fn save_remotes(list: &[String]) -> std::io::Result<()> {
    let p = remotes_path();
    let json = serde_json::to_string_pretty(list).unwrap();
    atomic_write(&p, json.as_bytes())
}

/// Where the absolute `uvx` path discovered for each remote is remembered
/// (#184).
///
/// A **sidecar** rather than a second field in `remotes.json`, deliberately.
/// Widening `remotes.json` from `["host"]` to `[{alias, uvx_path}]` would be
/// read fine by a new build and silently as an empty list by an older one —
/// `load_remotes` ends in `unwrap_or_default()`, so a downgrade would wipe
/// the user's registered hosts. An extra file costs one `read_to_string`;
/// an older build simply ignores it and is back to today's behaviour.
fn remote_uvx_path() -> PathBuf {
    home().join(".config").join("aiui").join("remote-uvx.json")
}

/// The absolute `uvx` path discovered for `host_alias`, if we know one.
///
/// #184: `add_remote` probes the remote for an absolute path precisely
/// because the bare name depends on Claude Code's PATH at spawn time. That
/// discovery was then thrown away, so the two resync paths — one of which
/// runs on every launch — passed `None` and rewrote the pinned absolute
/// path back down to `"uvx"`, reporting success while breaking the host.
pub fn load_remote_uvx(host_alias: &str) -> Option<String> {
    let raw = fs::read_to_string(remote_uvx_path()).ok()?;
    let map: HashMap<String, String> = serde_json::from_str(&raw).ok()?;
    map.get(host_alias).cloned()
}

/// Remember (or forget, with `None`) the uvx path for one host. Malformed
/// or absent sidecar content starts from empty rather than failing — this
/// is a cache, not state we cannot rebuild.
pub fn save_remote_uvx(host_alias: &str, uvx_path: Option<&str>) -> std::io::Result<()> {
    let p = remote_uvx_path();
    let mut map: HashMap<String, String> = fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    match uvx_path {
        Some(path) => {
            map.insert(host_alias.to_string(), path.to_string());
        }
        None => {
            map.remove(host_alias);
        }
    }
    let json = serde_json::to_string_pretty(&map).unwrap_or_else(|_| "{}".into());
    atomic_write(&p, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_none() {
        assert!(classify_aiui_entry(None).is_none());
    }

    /// Temp dir for the remotes-migration tests, in the house style
    /// (`temp_dir()` + a pid-suffixed name, removed at the end of the test).
    fn migration_test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("aiui-test-remotes-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn remotes_path_follows_config_dir() {
        // #196: the helper maps the config dir it is given and nothing else.
        // Before the fix it rebuilt `~/.config/aiui/remotes.json` from $HOME,
        // which on Windows is a different directory than the one holding the
        // token, `first_run_done` and `gui.lock`.
        let base = Path::new("/x/aiui");
        assert_eq!(remotes_path_in(base), base.join("remotes.json"));
        let home_remotes = home().join(".config").join("aiui").join("remotes.json");
        assert_ne!(
            remotes_path_in(base),
            home_remotes,
            "must not re-derive a path from $HOME"
        );
    }

    #[test]
    fn migrate_remotes_moves_legacy_file_once() {
        let dir = migration_test_dir("move");
        let legacy = dir.join("legacy/remotes.json");
        let new = dir.join("config/remotes.json");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, br#"["dev@devhost"]"#).unwrap();

        assert!(migrate_remotes_from(&legacy, &new).unwrap(), "first call migrates");
        assert_eq!(fs::read_to_string(&new).unwrap(), r#"["dev@devhost"]"#);
        assert!(!legacy.exists(), "legacy file is moved, not copied");

        // Second call is a no-op: nothing left to move, nothing rewritten.
        assert!(!migrate_remotes_from(&legacy, &new).unwrap());
        assert_eq!(fs::read_to_string(&new).unwrap(), r#"["dev@devhost"]"#);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_remotes_never_overwrites_new_file() {
        let dir = migration_test_dir("no-overwrite");
        let legacy = dir.join("legacy/remotes.json");
        let new = dir.join("config/remotes.json");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::create_dir_all(new.parent().unwrap()).unwrap();
        fs::write(&legacy, br#"["stale@host"]"#).unwrap();
        fs::write(&new, br#"["current@host"]"#).unwrap();

        assert!(
            !migrate_remotes_from(&legacy, &new).unwrap(),
            "an existing file at the new path always wins"
        );
        assert_eq!(fs::read_to_string(&new).unwrap(), r#"["current@host"]"#);
        assert!(legacy.exists(), "nothing is removed when nothing moved");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_remotes_is_a_noop_when_both_paths_are_the_same_file() {
        // macOS/Linux: legacy and new resolve to the identical path, so the
        // migration must not read-write-delete its own file.
        let dir = migration_test_dir("same-path");
        let p = dir.join("remotes.json");
        fs::write(&p, br#"["dev@devhost"]"#).unwrap();
        assert!(!migrate_remotes_from(&p, &p).unwrap());
        assert_eq!(fs::read_to_string(&p).unwrap(), r#"["dev@devhost"]"#);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_cleanup_skipped_is_a_non_error() {
        // Removing an unreachable host is a success (deregistered locally),
        // not a failure — it must not render as a red log line, and it must
        // name the host and say what was left behind.
        let r = remote_cleanup_skipped("dev@devhost");
        assert!(r.ok);
        assert!(r.message.contains("dev@devhost"));
        assert!(r.message.contains("nicht erreichbar"));
        assert!(r.details.is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn claude_desktop_proc_match_is_location_independent() {
        // #180: the old probe hard-coded /Applications, so a ~/Applications
        // install looked permanently dead and the exit gate inverted.
        assert!(is_claude_desktop_proc(
            "/Applications/Claude.app/Contents/MacOS/Claude"
        ));
        assert!(is_claude_desktop_proc(
            "/Users/ada/Applications/Claude.app/Contents/MacOS/Claude"
        ));
        // The real input shape: `pgrep -af` prefixes the pid.
        assert!(is_claude_desktop_proc(
            "4711 /Applications/Claude.app/Contents/MacOS/Claude"
        ));
        assert!(
            is_claude_desktop_proc(
                "/Applications/Claude.app/Contents/Frameworks/Claude Helper.app\
                 /Contents/MacOS/Claude Helper --type=renderer"
            ) == false,
            "a helper's own executable path is not the main binary"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn claude_desktop_proc_match_never_matches_the_cli() {
        // A Claude Code session must never be mistaken for Claude Desktop —
        // that would make the host think its Wirt is alive when it is not.
        assert!(!is_claude_desktop_proc("/usr/local/bin/claude"));
        assert!(!is_claude_desktop_proc("node /opt/homebrew/bin/claude --print"));
        assert!(!is_claude_desktop_proc(
            "/Applications/aiui.app/Contents/MacOS/aiui --mcp-stdio"
        ));
        assert!(!is_claude_desktop_proc("claude-desktop"));
        assert!(!is_claude_desktop_proc(""));
    }

    #[test]
    fn classify_legacy_uvx() {
        let v = serde_json::json!({"command": "uvx", "args": ["aiui-mcp"]});
        assert!(matches!(
            classify_aiui_entry(Some(&v)),
            Some(AiuiEntryKind::LegacyUvx)
        ));
    }

    #[test]
    fn classify_legacy_uvx_with_extra_args() {
        // Extra args after "aiui-mcp" shouldn't disqualify the entry from
        // being recognized as legacy — first arg is the package name.
        let v = serde_json::json!({"command": "uvx", "args": ["aiui-mcp", "--verbose"]});
        assert!(matches!(
            classify_aiui_entry(Some(&v)),
            Some(AiuiEntryKind::LegacyUvx)
        ));
    }

    #[test]
    fn classify_native_binary_is_other() {
        // The native binary is considered "Other" here; callers compare
        // to the current binary path and migrate only when it differs.
        let v = serde_json::json!({
            "command": "/Applications/aiui.app/Contents/MacOS/aiui",
            "args": ["--mcp-stdio"]
        });
        assert!(matches!(
            classify_aiui_entry(Some(&v)),
            Some(AiuiEntryKind::Other)
        ));
    }

    #[test]
    fn classify_unrelated_command() {
        let v = serde_json::json!({"command": "python", "args": ["-m", "something"]});
        assert!(matches!(
            classify_aiui_entry(Some(&v)),
            Some(AiuiEntryKind::Other)
        ));
    }

    #[test]
    fn classify_uvx_but_wrong_package() {
        let v = serde_json::json!({"command": "uvx", "args": ["some-other-package"]});
        assert!(matches!(
            classify_aiui_entry(Some(&v)),
            Some(AiuiEntryKind::Other)
        ));
    }

    #[test]
    fn classify_malformed_entry() {
        let v = serde_json::json!({});
        assert!(matches!(
            classify_aiui_entry(Some(&v)),
            Some(AiuiEntryKind::Other)
        ));
    }

    const AIUI_BIN: &str = "/Applications/aiui.app/Contents/MacOS/aiui";

    // ─── #184: the discovered uvx path survives a restart ───────────────

    #[test]
    fn remote_uvx_sidecar_round_trips() {
        // The store is a sidecar rather than a second field in
        // remotes.json: widening that file would make an older build read
        // it as an empty list and silently drop the user's hosts.
        let dir = std::env::temp_dir().join(format!("aiui-uvx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("remote-uvx.json");

        // Written by hand here, because the real accessors resolve against
        // the user's home; this asserts the shape they read and write.
        let mut map = HashMap::new();
        map.insert("macmini".to_string(), "/opt/homebrew/bin/uvx".to_string());
        std::fs::write(&path, serde_json::to_string_pretty(&map).unwrap()).unwrap();

        let read: HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(read.get("macmini").unwrap(), "/opt/homebrew/bin/uvx");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_or_broken_sidecar_is_not_fatal() {
        // It is a cache, not state we cannot rebuild: a corrupt file must
        // degrade to "no path known", which now means "keep what works".
        assert!(load_remote_uvx("no-such-host-at-all").is_none());
        let broken: Result<HashMap<String, String>, _> = serde_json::from_str("{not json");
        assert!(broken.is_err(), "and such content parses as an error, not a map");
    }

    // ─── #182: never destroy a user config we could not parse ───────────

    #[test]
    fn read_json_config_distinguishes_absent_empty_and_broken() {
        let dir = std::env::temp_dir().join(format!("aiui-rjc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let missing = dir.join("nope.json");
        assert!(matches!(read_json_config(&missing), Ok(None)), "absent");

        let empty = dir.join("empty.json");
        std::fs::write(&empty, "   \n").unwrap();
        assert_eq!(
            read_json_config(&empty).unwrap(),
            Some(Value::Object(Map::new())),
            "an empty file is genuinely safe to treat as {{}}"
        );

        // The failure that used to wipe the file: one trailing comma.
        let broken = dir.join("broken.json");
        std::fs::write(&broken, r#"{"mcpServers": {"other": {"command": "x"},}}"#).unwrap();
        assert!(
            read_json_config(&broken).is_err(),
            "unparsable must be an error, never an empty object"
        );

        let good = dir.join("good.json");
        std::fs::write(&good, r#"{"a": 1}"#).unwrap();
        assert!(read_json_config(&good).unwrap().is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upsert_keeps_foreign_keys_on_the_aiui_entry() {
        // #182: an `env` block (or anything else) on the aiui entry used to
        // be discarded on every launch.
        let mut servers: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "aiui": {
                "command": "/old/path/aiui",
                "args": ["--mcp-stdio"],
                "env": {"AIUI_DEBUG": "1"},
                "disabled": false
            },
            "other": {"command": "keep-me"}
        }))
        .unwrap();
        upsert_aiui_entry(&mut servers, "/Applications/aiui.app/Contents/MacOS/aiui");
        let aiui = servers["aiui"].as_object().unwrap();
        assert_eq!(
            aiui["command"].as_str().unwrap(),
            "/Applications/aiui.app/Contents/MacOS/aiui",
            "the migration must still be able to overwrite command"
        );
        assert_eq!(aiui["env"]["AIUI_DEBUG"], "1", "foreign key survives");
        assert_eq!(aiui["disabled"], false, "foreign key survives");
        assert_eq!(servers["other"]["command"], "keep-me", "other servers untouched");
    }

    #[test]
    fn upsert_creates_the_entry_when_absent() {
        let mut servers: Map<String, Value> = Map::new();
        upsert_aiui_entry(&mut servers, "/bin/aiui");
        assert_eq!(servers["aiui"]["command"], "/bin/aiui");
        assert_eq!(servers["aiui"]["args"][0], "--mcp-stdio");
    }

    #[test]
    fn entry_is_current_compares_only_owned_keys() {
        // #182: whole-object equality meant an entry with an `env` block was
        // rewritten — and re-backed-up — on every single launch.
        let with_env = serde_json::json!({
            "command": "/bin/aiui", "args": ["--mcp-stdio"], "env": {"X": "1"}
        });
        assert!(
            aiui_entry_is_current(Some(&with_env), "/bin/aiui"),
            "extra keys must not force a rewrite"
        );
        let stale = serde_json::json!({"command": "/old/aiui", "args": ["--mcp-stdio"]});
        assert!(!aiui_entry_is_current(Some(&stale), "/bin/aiui"), "stale path");
        let legacy = serde_json::json!({"command": "uvx", "args": ["aiui-mcp"]});
        assert!(!aiui_entry_is_current(Some(&legacy), "/bin/aiui"), "legacy uvx");
        assert!(!aiui_entry_is_current(None, "/bin/aiui"), "absent");
    }

    #[test]
    fn backup_appends_and_prunes() {
        // #182: `~/.claude.json` must back up to `~/.claude.json.bak.<ts>`,
        // not the misleading `~/.claude.bak.<ts>`, and the pile is bounded.
        let dir = std::env::temp_dir().join(format!("aiui-bak-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join(".claude.json");
        std::fs::write(&target, "{}").unwrap();

        let first = backup(&target).unwrap().expect("a backup was made");
        let name = first.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with(".claude.json.bak."),
            "backup name appends to the full file name: {name}"
        );

        for i in 0..9 {
            std::fs::write(&target, format!("{{\"n\": {i}}}")).unwrap();
            backup(&target).unwrap();
        }
        let baks = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_str().unwrap().contains(".bak."))
            .count();
        assert!(baks <= 5, "backups are pruned to at most 5, found {baks}");

        // An absent file yields no backup and no error.
        assert!(backup(&dir.join("nope")).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn codex_upsert_preserves_foreign_keys_on_the_aiui_table() {
        let existing = r#"
[mcp_servers.aiui]
command = "/old/aiui"
args = ["--mcp-stdio"]
# the user's own note
startup_timeout_ms = 30000
"#;
        let out = codex_toml_upsert(Some(existing), "/new/aiui").unwrap();
        assert!(out.contains("/new/aiui"), "command updated");
        assert!(
            out.contains("startup_timeout_ms = 30000"),
            "foreign key survives: {out}"
        );
        assert!(out.contains("# the user's own note"), "comment survives: {out}");
    }

    #[test]
    fn codex_upsert_fresh_document() {
        let out = codex_toml_upsert(None, AIUI_BIN).unwrap();
        assert!(out.contains("[mcp_servers.aiui]"), "got: {out}");
        assert!(out.contains(&format!("command = \"{AIUI_BIN}\"")), "got: {out}");
        assert!(out.contains(r#"args = ["--mcp-stdio"]"#), "got: {out}");
    }

    #[test]
    fn codex_upsert_preserves_other_servers_and_comments() {
        let existing = "# my codex config\n[mcp_servers.other]\ncommand = \"other-bin\"\nargs = [\"x\"]\n";
        let out = codex_toml_upsert(Some(existing), AIUI_BIN).unwrap();
        assert!(out.contains("# my codex config"), "comment preserved: {out}");
        assert!(out.contains("[mcp_servers.other]"), "other server preserved: {out}");
        assert!(out.contains("other-bin"));
        assert!(out.contains("[mcp_servers.aiui]"), "aiui added: {out}");
    }

    #[test]
    fn codex_upsert_migrates_legacy_uvx() {
        let existing = "[mcp_servers.aiui]\ncommand = \"uvx\"\nargs = [\"aiui-mcp\"]\n";
        let out = codex_toml_upsert(Some(existing), AIUI_BIN).unwrap();
        assert!(out.contains(&format!("command = \"{AIUI_BIN}\"")), "migrated: {out}");
        assert!(!out.contains("uvx"), "legacy uvx command replaced: {out}");
        assert!(out.contains(r#"args = ["--mcp-stdio"]"#), "args replaced: {out}");
    }

    #[test]
    fn codex_upsert_is_idempotent() {
        let once = codex_toml_upsert(None, AIUI_BIN).unwrap();
        let twice = codex_toml_upsert(Some(&once), AIUI_BIN).unwrap();
        assert_eq!(once, twice, "second apply changed the document");
    }

    // ─── #183: the host-registration write path, end to end on disk ─────
    //
    // These are the files aiui does NOT own: they carry every other MCP
    // server the user configured, and `~/.claude.json` carries Claude
    // Code's whole per-project state as well. The companion rewrites them
    // unattended on every GUI launch, so a regression here is silent data
    // loss on the user's own data. Every assertion below is on the bytes
    // that ended up on disk, not on a message string — the Desktop
    // patcher speaks German while Code and Codex speak English, so
    // messages generalise badly and are only used where a branch is
    // observable nowhere else (migrated vs. added vs. already-registered).
    //
    // Tests drive the `_at` cores exclusively. Calling a wrapper here
    // would rewrite the developer's — and the CI runner's — real config.

    /// A temp directory that removes itself, including when a test panics.
    ///
    /// Deliberately no `tempfile` dev-dependency: the crate has none, and
    /// the repo already has a unique-scratch-path idiom (`filewrite.rs`,
    /// `uuid::Uuid::new_v4()`). Not the PID-based idiom from `fsutil.rs` —
    /// every test in `aiui_lib` shares one process, so PID-named
    /// directories collide the moment two tests pick the same prefix.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("aiui-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }

        /// The `<name>.bak.*` siblings sitting in this directory.
        fn backups_of(&self, name: &str) -> Vec<PathBuf> {
            let prefix = format!("{name}.bak.");
            let mut found: Vec<PathBuf> = std::fs::read_dir(&self.0)
                .expect("read temp dir")
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with(&prefix))
                        .unwrap_or(false)
                })
                .collect();
            found.sort();
            found
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn read_json(path: &Path) -> Value {
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{} should exist: {e}", path.display()));
        serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{} should be valid JSON: {e}\n{raw}", path.display()))
    }

    // ── Claude Code (`~/.claude.json`) ──────────────────────────────────

    #[test]
    fn claude_code_patch_creates_fresh_config() {
        let dir = TempDir::new("cc-fresh");
        let path = dir.join(".claude.json");

        let r = patch_claude_code_config_at(&path, AIUI_BIN);
        assert!(r.ok, "{}: {:?}", r.message, r.details);

        let v = read_json(&path);
        assert_eq!(v["mcpServers"]["aiui"]["command"], AIUI_BIN);
        assert_eq!(
            v["mcpServers"]["aiui"]["args"],
            serde_json::json!(["--mcp-stdio"])
        );
    }

    #[test]
    fn claude_code_patch_preserves_foreign_servers_and_top_level_keys() {
        // The regression that would hurt most: aiui rewriting this file
        // and taking the user's other MCP servers — or Claude Code's
        // `projects` block — with it.
        let dir = TempDir::new("cc-foreign");
        let path = dir.join(".claude.json");
        let seed = serde_json::json!({
            "mcpServers": {
                "other": {"command": "other-bin", "args": ["x"], "env": {"K": "V"}}
            },
            "projects": {"/Users/ada/code": {"allowedTools": ["Bash"]}},
            "oauthAccount": {"accountUuid": "abc-123"}
        });
        std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        let r = patch_claude_code_config_at(&path, AIUI_BIN);
        assert!(r.ok, "{}", r.message);

        let v = read_json(&path);
        assert_eq!(v["mcpServers"]["other"], seed["mcpServers"]["other"]);
        assert_eq!(v["projects"], seed["projects"]);
        assert_eq!(v["oauthAccount"], seed["oauthAccount"]);
        let servers = v["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 2, "only aiui was added: {servers:?}");
        assert_eq!(v["mcpServers"]["aiui"]["command"], AIUI_BIN);
    }

    #[test]
    fn claude_code_patch_is_idempotent() {
        // Every GUI launch calls this. A rewrite per launch means a fresh
        // `.bak` per launch — a pile of full copies of a credential-bearing
        // file nobody ever looks at.
        let dir = TempDir::new("cc-idem");
        let path = dir.join(".claude.json");
        std::fs::write(&path, r#"{"mcpServers": {"other": {"command": "x"}}}"#).unwrap();

        let first = patch_claude_code_config_at(&path, AIUI_BIN);
        assert!(first.ok, "{}", first.message);
        let after_first = std::fs::read(&path).unwrap();
        let baks = dir.backups_of(".claude.json");
        assert_eq!(baks.len(), 1, "one backup for the one rewrite");

        let second = patch_claude_code_config_at(&path, AIUI_BIN);
        assert!(second.ok, "{}", second.message);
        assert!(
            second.message.contains("already registered"),
            "second call must short-circuit, got: {}",
            second.message
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            after_first,
            "file rewritten on a no-op patch"
        );
        let baks_again = dir.backups_of(".claude.json");
        assert_eq!(baks_again.len(), 1, "a second `.bak` was dropped");
        // Same-millisecond backups would overwrite rather than add, so
        // also pin the content: it must still be the pre-first-patch file.
        assert_eq!(
            std::fs::read_to_string(&baks_again[0]).unwrap(),
            r#"{"mcpServers": {"other": {"command": "x"}}}"#
        );
    }

    #[test]
    fn claude_code_patch_migrates_legacy_uvx_entry() {
        // ≤ v0.2.x installs point at `uvx aiui-mcp`; the app bundle now
        // ships the server natively and `uv` may well be gone.
        let dir = TempDir::new("cc-uvx");
        let path = dir.join(".claude.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {"aiui": {"command": "uvx", "args": ["aiui-mcp"]}}}"#,
        )
        .unwrap();

        let r = patch_claude_code_config_at(&path, AIUI_BIN);
        assert!(r.ok, "{}", r.message);
        assert!(
            r.message.contains("uvx"),
            "the migration must be named in the message, got: {}",
            r.message
        );

        let v = read_json(&path);
        assert_eq!(v["mcpServers"]["aiui"]["command"], AIUI_BIN);
        assert_eq!(
            v["mcpServers"]["aiui"]["args"],
            serde_json::json!(["--mcp-stdio"])
        );
    }

    #[test]
    fn claude_code_patch_backs_up_before_rewrite() {
        // #182: the backup is the user's only way back, so it must exist,
        // be findable, and hold the bytes from *before* the rewrite.
        let dir = TempDir::new("cc-bak");
        let path = dir.join(".claude.json");
        let before = r#"{"mcpServers": {"other": {"command": "keep-me"}}}"#;
        std::fs::write(&path, before).unwrap();

        let r = patch_claude_code_config_at(&path, AIUI_BIN);
        assert!(r.ok, "{}", r.message);

        let baks = dir.backups_of(".claude.json");
        assert_eq!(baks.len(), 1, "exactly one backup: {baks:?}");
        assert_eq!(std::fs::read_to_string(&baks[0]).unwrap(), before);
        assert!(
            r.details.unwrap_or_default().contains("Backup: "),
            "the backup's path must be reported"
        );
    }

    #[test]
    fn claude_code_remove_drops_only_aiui() {
        let dir = TempDir::new("cc-remove");
        let path = dir.join(".claude.json");
        let seed = serde_json::json!({
            "mcpServers": {
                "aiui": {"command": AIUI_BIN, "args": ["--mcp-stdio"]},
                "other": {"command": "other-bin"}
            },
            "projects": {"/Users/ada/code": {"allowedTools": ["Bash"]}}
        });
        std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        let r = remove_claude_code_config_at(&path);
        assert!(r.ok, "{}", r.message);
        assert!(r.message.contains("Removed"), "got: {}", r.message);

        let v = read_json(&path);
        assert!(v.pointer("/mcpServers/aiui").is_none(), "aiui is gone");
        assert_eq!(v["mcpServers"]["other"], seed["mcpServers"]["other"]);
        assert_eq!(v["projects"], seed["projects"]);
    }

    #[test]
    fn claude_code_remove_on_missing_file_is_ok() {
        // Uninstall must not turn red — nor conjure a config file for a
        // host the user never wired up.
        let dir = TempDir::new("cc-remove-missing");
        let path = dir.join(".claude.json");

        let r = remove_claude_code_config_at(&path);
        assert!(r.ok, "{}", r.message);
        assert!(!path.exists(), "no file may be created on removal");
    }

    #[test]
    fn is_claude_code_config_current_only_on_an_exact_binary_match() {
        let dir = TempDir::new("cc-current");
        let path = dir.join(".claude.json");
        assert!(
            !is_claude_code_config_current_at(&path, AIUI_BIN),
            "missing file"
        );

        std::fs::write(&path, r#"{"mcpServers": {"other": {"command": "x"}}}"#).unwrap();
        assert!(
            !is_claude_code_config_current_at(&path, AIUI_BIN),
            "no aiui entry"
        );

        std::fs::write(
            &path,
            r#"{"mcpServers": {"aiui": {"command": "/old/aiui", "args": ["--mcp-stdio"]}}}"#,
        )
        .unwrap();
        assert!(
            !is_claude_code_config_current_at(&path, AIUI_BIN),
            "a different binary is not current — this is what triggers the \
             re-registration after an app move or update"
        );

        let r = patch_claude_code_config_at(&path, AIUI_BIN);
        assert!(r.ok, "{}", r.message);
        assert!(is_claude_code_config_current_at(&path, AIUI_BIN));

        std::fs::write(&path, "{not json").unwrap();
        assert!(
            !is_claude_code_config_current_at(&path, AIUI_BIN),
            "unparsable is 'not current', never a panic"
        );
    }

    // ── Claude Desktop (`claude_desktop_config.json`) ───────────────────

    #[test]
    fn claude_desktop_patch_creates_fresh_config() {
        let dir = TempDir::new("cd-fresh");
        let path = dir.join("claude_desktop_config.json");

        let r = patch_claude_desktop_config_at(&path, AIUI_BIN);
        assert!(r.ok, "{}: {:?}", r.message, r.details);

        let v = read_json(&path);
        assert_eq!(v["mcpServers"]["aiui"]["command"], AIUI_BIN);
        assert_eq!(
            v["mcpServers"]["aiui"]["args"],
            serde_json::json!(["--mcp-stdio"])
        );
    }

    #[test]
    fn claude_desktop_patch_preserves_foreign_servers() {
        let dir = TempDir::new("cd-foreign");
        let path = dir.join("claude_desktop_config.json");
        let seed = serde_json::json!({
            "mcpServers": {"other": {"command": "other-bin", "env": {"K": "V"}}},
            "globalShortcut": "Alt+Space"
        });
        std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        let r = patch_claude_desktop_config_at(&path, AIUI_BIN);
        assert!(r.ok, "{}", r.message);

        let v = read_json(&path);
        assert_eq!(v["mcpServers"]["other"], seed["mcpServers"]["other"]);
        assert_eq!(v["globalShortcut"], seed["globalShortcut"]);
        assert_eq!(v["mcpServers"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn claude_desktop_patch_migrates_aiui_local_key() {
        // ≤ v0.4.5 registered under `aiui-local`, which made the slash
        // commands read `/aiui-local:…` in Desktop and `/aiui:…` in Code.
        // The order of remove-then-insert here is load-bearing: swap it and
        // the freshly written `aiui` entry is the one that gets dropped.
        let dir = TempDir::new("cd-legacy");
        let path = dir.join("claude_desktop_config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {
                 "aiui-local": {"command": "uvx", "args": ["aiui-mcp"]},
                 "other": {"command": "other-bin"}
               }}"#,
        )
        .unwrap();

        let r = patch_claude_desktop_config_at(&path, AIUI_BIN);
        assert!(r.ok, "{}", r.message);
        assert!(
            r.message.contains("aiui-local"),
            "the migration must be named in the message, got: {}",
            r.message
        );

        let v = read_json(&path);
        assert!(
            v.pointer("/mcpServers/aiui-local").is_none(),
            "the legacy key must be gone"
        );
        assert_eq!(v["mcpServers"]["aiui"]["command"], AIUI_BIN);
        assert_eq!(
            v["mcpServers"]["aiui"]["args"],
            serde_json::json!(["--mcp-stdio"])
        );
        assert_eq!(v["mcpServers"]["other"]["command"], "other-bin");
    }

    #[test]
    fn claude_desktop_remove_drops_both_aiui_and_aiui_local() {
        let dir = TempDir::new("cd-remove");
        let path = dir.join("claude_desktop_config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {
                 "aiui": {"command": "/Applications/aiui.app/Contents/MacOS/aiui"},
                 "aiui-local": {"command": "uvx", "args": ["aiui-mcp"]},
                 "other": {"command": "other-bin"}
               }}"#,
        )
        .unwrap();

        let r = remove_claude_desktop_config_at(&path);
        assert!(r.ok, "{}", r.message);
        assert!(r.message.contains("entfernt"), "got: {}", r.message);

        let v = read_json(&path);
        let servers = v["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 1, "only `other` survives: {servers:?}");
        assert_eq!(servers["other"]["command"], "other-bin");
    }

    #[test]
    fn claude_desktop_patch_then_remove_round_trip() {
        // Patch and remove must meet on the same file. When they disagree
        // — as they did while the remover rebuilt the macOS path by hand —
        // remove reports a cheerful "nothing to do" and leaves an entry
        // pointing at a binary that no longer exists.
        let dir = TempDir::new("cd-roundtrip");
        let path = dir.join("claude_desktop_config.json");
        std::fs::write(&path, r#"{"mcpServers": {"other": {"command": "other-bin"}}}"#).unwrap();

        let patched = patch_claude_desktop_config_at(&path, AIUI_BIN);
        assert!(patched.ok, "{}", patched.message);
        assert!(read_json(&path).pointer("/mcpServers/aiui").is_some());

        let removed = remove_claude_desktop_config_at(&path);
        assert!(removed.ok, "{}", removed.message);
        assert!(
            removed.message.contains("entfernt"),
            "remove must find what patch wrote, got: {}",
            removed.message
        );

        let v = read_json(&path);
        assert!(v.pointer("/mcpServers/aiui").is_none());
        assert_eq!(v["mcpServers"]["other"]["command"], "other-bin");
    }

    #[test]
    fn claude_desktop_config_path_is_os_correct() {
        let p = claude_desktop_config_path();
        assert_eq!(
            p.file_name().and_then(|n| n.to_str()),
            Some("claude_desktop_config.json")
        );
        assert_eq!(
            p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()),
            Some("Claude")
        );
        #[cfg(target_os = "windows")]
        {
            // Roaming AppData, where Claude Desktop actually reads it.
            // Dead code until the Windows CI job executes tests.
            let base = dirs::config_dir().expect("config dir on Windows");
            assert!(
                p.starts_with(base.as_path()),
                "{} must sit under {}",
                p.display(),
                base.display()
            );
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert!(
                p.ends_with("Library/Application Support/Claude/claude_desktop_config.json"),
                "got {}",
                p.display()
            );
        }
    }

    #[test]
    fn every_host_wrapper_resolves_its_path_through_the_one_path_fn() {
        // The live bug #183 was filed over: `remove_claude_desktop_config`
        // rebuilt `~/Library/Application Support/Claude/…` inline instead
        // of calling `claude_desktop_config_path()`, so Windows uninstall
        // never removed the entry. No behavioural test can catch that —
        // the `_at` cores are handed their path — so the invariant is
        // pinned textually, the way `scripts/check-release-ordering.sh`
        // pins the release step order.
        // `\r` stripped because the Windows runner checks out with
        // `core.autocrlf=true`, and the body delimiter below is LF.
        let src = include_str!("setup.rs").replace('\r', "");
        let wrappers = [
            (
                "pub fn patch_claude_desktop_config(app_binary_path: &str) -> StepResult {",
                "claude_desktop_config_path()",
            ),
            (
                "pub fn is_claude_config_current(app_binary_path: &str) -> bool {",
                "claude_desktop_config_path()",
            ),
            (
                "pub fn remove_claude_desktop_config() -> StepResult {",
                "claude_desktop_config_path()",
            ),
            (
                "pub fn patch_claude_code_config(app_binary_path: &str) -> StepResult {",
                "claude_code_config_path()",
            ),
            (
                "pub fn is_claude_code_config_current(app_binary_path: &str) -> bool {",
                "claude_code_config_path()",
            ),
            (
                "pub fn remove_claude_code_config() -> StepResult {",
                "claude_code_config_path()",
            ),
            (
                "pub fn patch_codex_config(app_binary_path: &str) -> StepResult {",
                "codex_config_path()",
            ),
        ];
        for (signature, path_fn) in wrappers {
            let after = src
                .split_once(signature)
                .unwrap_or_else(|| panic!("wrapper not found — renamed? {signature}"))
                .1;
            let body = after
                .split_once("\n}\n")
                .unwrap_or_else(|| panic!("could not delimit the body of {signature}"))
                .0;
            assert!(
                body.contains(path_fn),
                "{signature} must resolve its path via {path_fn}"
            );
            assert!(
                !body.contains("home()"),
                "{signature} must not rebuild the host's path from home()"
            );
        }
    }

    // ── Codex (`~/.codex/config.toml`) ──────────────────────────────────

    #[test]
    fn codex_patch_creates_fresh_config() {
        let dir = TempDir::new("codex-fresh");
        let path = dir.join("config.toml");

        let r = patch_codex_config_at(&path, AIUI_BIN);
        assert!(r.ok, "{}: {:?}", r.message, r.details);
        assert!(r.message.contains("Added"), "got: {}", r.message);

        let raw = std::fs::read_to_string(&path).unwrap();
        let doc: toml_edit::DocumentMut = raw.parse().expect("valid TOML");
        assert_eq!(
            doc["mcp_servers"]["aiui"]["command"].as_str(),
            Some(AIUI_BIN)
        );
        let args: Vec<&str> = doc["mcp_servers"]["aiui"]["args"]
            .as_array()
            .expect("args array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(args, ["--mcp-stdio"]);
    }

    #[test]
    fn codex_patch_preserves_comments_and_other_servers() {
        let dir = TempDir::new("codex-foreign");
        let path = dir.join("config.toml");
        let before = "# my codex config\nmodel = \"gpt-5\"\n\n\
                      [mcp_servers.other]\ncommand = \"other-bin\"\nargs = [\"x\"]\n";
        std::fs::write(&path, before).unwrap();

        let r = patch_codex_config_at(&path, AIUI_BIN);
        assert!(r.ok, "{}", r.message);

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("# my codex config"), "comment survives: {raw}");
        assert!(raw.contains("model = \"gpt-5\""), "other keys survive: {raw}");
        assert!(raw.contains("[mcp_servers.other]"), "other server: {raw}");
        assert!(raw.contains("other-bin"), "other server's command: {raw}");
        assert!(raw.contains("[mcp_servers.aiui]"), "aiui added: {raw}");
        assert!(raw.contains(AIUI_BIN), "aiui points at the bundle: {raw}");
    }

    #[test]
    fn codex_patch_is_idempotent_no_second_backup() {
        let dir = TempDir::new("codex-idem");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[mcp_servers.other]\ncommand = \"other-bin\"\n").unwrap();

        let first = patch_codex_config_at(&path, AIUI_BIN);
        assert!(first.ok, "{}", first.message);
        let after_first = std::fs::read(&path).unwrap();
        assert_eq!(dir.backups_of("config.toml").len(), 1);

        let second = patch_codex_config_at(&path, AIUI_BIN);
        assert!(second.ok, "{}", second.message);
        assert!(
            second.message.contains("already registered"),
            "second call must short-circuit, got: {}",
            second.message
        );
        assert_eq!(std::fs::read(&path).unwrap(), after_first, "file rewritten");
        let baks = dir.backups_of("config.toml");
        assert_eq!(baks.len(), 1, "a fresh `.bak` on every GUI launch");
        assert_eq!(
            std::fs::read_to_string(&baks[0]).unwrap(),
            "[mcp_servers.other]\ncommand = \"other-bin\"\n",
            "the one backup still holds the pre-patch bytes"
        );
    }

    #[test]
    fn codex_patch_refuses_malformed_toml() {
        // A config we cannot parse is a config we must not rewrite: the
        // alternative is replacing the user's Codex setup with one holding
        // nothing but aiui.
        let dir = TempDir::new("codex-broken");
        let path = dir.join("config.toml");
        let before = "[mcp_servers.aiui]\ncommand =\n";
        std::fs::write(&path, before).unwrap();

        let r = patch_codex_config_at(&path, AIUI_BIN);
        assert!(!r.ok, "a malformed config must fail the step");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "the file must be byte-identical"
        );
        assert!(
            dir.backups_of("config.toml").is_empty(),
            "nothing was written, so nothing needed backing up"
        );
    }
}
