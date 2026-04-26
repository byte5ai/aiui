use serde::Serialize;
use serde_json::{Map, Value};
use std::fs;
use std::path::PathBuf;
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

fn backup(path: &PathBuf) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bak = path.with_extension(format!("bak.{ts}"));
    fs::copy(path, &bak)?;
    Ok(())
}

pub fn patch_claude_desktop_config(app_binary_path: &str) -> StepResult {
    let path = home()
        .join("Library")
        .join("Application Support")
        .join("Claude")
        .join("claude_desktop_config.json");

    let existing: Value = if path.exists() {
        match fs::read_to_string(&path) {
            Ok(s) if s.trim().is_empty() => Value::Object(Map::new()),
            Ok(s) => serde_json::from_str(&s).unwrap_or(Value::Object(Map::new())),
            Err(e) => {
                return StepResult {
                    ok: false,
                    message: "Konnte claude_desktop_config.json nicht lesen".into(),
                    details: Some(e.to_string()),
                }
            }
        }
    } else {
        Value::Object(Map::new())
    };

    let mut root = existing.as_object().cloned().unwrap_or_default();
    let mut servers = root
        .get("mcpServers")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    let entry = serde_json::json!({
        "command": app_binary_path,
        "args": ["--mcp-stdio"]
    });
    // Migration: ≤ v0.4.5 wrote the entry under the key `aiui-local`. That
    // mismatched `~/.claude.json`'s `aiui` key, breaking slash commands like
    // `/aiui:test-dialog` in Claude Desktop (would have needed
    // `/aiui-local:test-dialog`). Unify on `aiui` and drop the old entry on
    // every patch — idempotent for fresh installs, healing for upgrades.
    let had_legacy = servers.contains_key("aiui-local");
    servers.remove("aiui-local");
    let was_present = servers.contains_key("aiui");
    servers.insert("aiui".into(), entry);
    root.insert("mcpServers".into(), Value::Object(servers));

    if let Err(e) = backup(&path) {
        return StepResult {
            ok: false,
            message: "Backup fehlgeschlagen".into(),
            details: Some(e.to_string()),
        };
    }

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let pretty = serde_json::to_string_pretty(&Value::Object(root)).unwrap();
    match fs::write(&path, pretty) {
        Ok(_) => StepResult {
            ok: true,
            message: match (was_present, had_legacy) {
                (true, _) => "aiui in Claude Desktop Config aktualisiert.".into(),
                (false, true) => {
                    "aiui in Claude Desktop Config eingetragen — alter `aiui-local`-Eintrag migriert.".into()
                }
                (false, false) => "aiui zu Claude Desktop Config hinzugefügt.".into(),
            },
            details: Some(format!("Datei: {}", path.display())),
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
    let out1 = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "--",
            host_alias,
            "mkdir -p ~/.config/aiui && chmod 700 ~/.config/aiui",
        ])
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
    let out2 = Command::new("scp").arg(token_path).arg(&dest).output();
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
    // when bundled, the executable sits at aiui.app/Contents/MacOS/aiui
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/Applications/aiui.app/Contents/MacOS/aiui".to_string())
}

pub fn is_claude_config_current(app_binary_path: &str) -> bool {
    let path = home()
        .join("Library")
        .join("Application Support")
        .join("Claude")
        .join("claude_desktop_config.json");
    let Ok(s) = fs::read_to_string(&path) else {
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
    let path = home().join(".claude.json");
    let Ok(s) = fs::read_to_string(&path) else {
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

/// Best-effort check whether the Claude Desktop application is currently
/// running on this Mac. Used to switch the "Restart Claude Desktop" button
/// label between Start / Restart so we don't ask the user to "restart"
/// something that isn't running.
///
/// Pure read-only — uses `pgrep -f` against the typical install path. If
/// `pgrep` is missing or errors, we assume "not running" rather than
/// blocking the UI.
pub fn is_claude_desktop_running() -> bool {
    let out = std::process::Command::new("pgrep")
        .args(["-f", "/Applications/Claude.app/"])
        .output();
    match out {
        Ok(o) => o.status.success() && !o.stdout.is_empty(),
        Err(_) => false,
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
    let path = home().join(".claude.json");
    let existing: Value = if path.exists() {
        match fs::read_to_string(&path) {
            Ok(s) if s.trim().is_empty() => Value::Object(Map::new()),
            Ok(s) => serde_json::from_str(&s).unwrap_or(Value::Object(Map::new())),
            Err(e) => {
                return StepResult {
                    ok: false,
                    message: "Could not read ~/.claude.json".into(),
                    details: Some(e.to_string()),
                }
            }
        }
    } else {
        Value::Object(Map::new())
    };

    let mut root = existing.as_object().cloned().unwrap_or_default();
    let mut servers = root
        .get("mcpServers")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    let entry = serde_json::json!({
        "command": app_binary_path,
        "args": ["--mcp-stdio"]
    });

    let existing_entry = servers.get("aiui");
    let previous_kind = classify_aiui_entry(existing_entry);
    let was_present = existing_entry.is_some();
    let already_correct = existing_entry == Some(&entry);
    if already_correct {
        return StepResult {
            ok: true,
            message: "aiui already registered in ~/.claude.json".into(),
            details: None,
        };
    }
    servers.insert("aiui".into(), entry);
    root.insert("mcpServers".into(), Value::Object(servers));

    if let Err(e) = backup(&path) {
        return StepResult {
            ok: false,
            message: "~/.claude.json backup failed".into(),
            details: Some(e.to_string()),
        };
    }
    let pretty = serde_json::to_string_pretty(&Value::Object(root)).unwrap();
    match fs::write(&path, pretty) {
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
                details: None,
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
    let path = home().join(".claude.json");
    if !path.exists() {
        return StepResult {
            ok: true,
            message: "~/.claude.json does not exist".into(),
            details: None,
        };
    }
    let Ok(s) = fs::read_to_string(&path) else {
        return StepResult {
            ok: true,
            message: "~/.claude.json unreadable, skipping".into(),
            details: None,
        };
    };
    let mut v: Value = serde_json::from_str(&s).unwrap_or(Value::Object(Map::new()));
    let had = v.pointer("/mcpServers/aiui").is_some();
    if let Some(servers) = v.get_mut("mcpServers").and_then(|x| x.as_object_mut()) {
        servers.remove("aiui");
    }
    if let Err(e) = backup(&path) {
        return StepResult {
            ok: false,
            message: "~/.claude.json backup failed".into(),
            details: Some(e.to_string()),
        };
    }
    let pretty = serde_json::to_string_pretty(&v).unwrap();
    match fs::write(&path, pretty) {
        Ok(_) => StepResult {
            ok: true,
            message: if had {
                "Removed aiui from ~/.claude.json".into()
            } else {
                "aiui was not registered in ~/.claude.json".into()
            },
            details: None,
        },
        Err(e) => StepResult {
            ok: false,
            message: "Writing ~/.claude.json failed".into(),
            details: Some(e.to_string()),
        },
    }
}

pub fn remove_claude_desktop_config() -> StepResult {
    let path = home()
        .join("Library")
        .join("Application Support")
        .join("Claude")
        .join("claude_desktop_config.json");
    if !path.exists() {
        return StepResult {
            ok: true,
            message: "claude_desktop_config.json existiert nicht, nichts zu tun.".into(),
            details: None,
        };
    }
    let s = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return StepResult {
                ok: false,
                message: "Konnte claude_desktop_config.json nicht lesen".into(),
                details: Some(e.to_string()),
            }
        }
    };
    let mut v: Value = serde_json::from_str(&s).unwrap_or(Value::Object(Map::new()));
    // Remove both the current `aiui` key and the legacy `aiui-local` key so
    // Uninstall always leaves a clean state regardless of which version
    // wrote the entry.
    let had = v.pointer("/mcpServers/aiui").is_some()
        || v.pointer("/mcpServers/aiui-local").is_some();
    if let Some(servers) = v.get_mut("mcpServers").and_then(|x| x.as_object_mut()) {
        servers.remove("aiui");
        servers.remove("aiui-local");
    }
    if let Err(e) = backup(&path) {
        return StepResult {
            ok: false,
            message: "Backup fehlgeschlagen".into(),
            details: Some(e.to_string()),
        };
    }
    let pretty = serde_json::to_string_pretty(&v).unwrap();
    match fs::write(&path, pretty) {
        Ok(_) => StepResult {
            ok: true,
            message: if had {
                "aiui aus Claude Desktop Config entfernt.".into()
            } else {
                "aiui war bereits nicht eingetragen.".into()
            },
            details: None,
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

    if let Err(e) = backup(&path) {
        return StepResult {
            ok: false,
            message: "Backup fehlgeschlagen".into(),
            details: Some(e.to_string()),
        };
    }
    match fs::write(&path, out) {
        Ok(_) => StepResult {
            ok: true,
            message: if changed {
                format!("aiui-Einträge aus Host '{host_alias}' entfernt.")
            } else {
                format!("Host '{host_alias}' hatte keine aiui-Einträge.")
            },
            details: None,
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
pub fn patch_claude_code_config_remote(host_alias: &str) -> StepResult {
    let script = r#"
import json, os, pathlib, shutil, time
p = pathlib.Path.home() / ".claude.json"
data = {}
if p.exists():
    try:
        data = json.loads(p.read_text())
    except Exception:
        data = {}
    ts = int(time.time())
    shutil.copy(p, p.with_suffix(f".json.bak.{ts}"))
servers = data.get("mcpServers") or {}
servers["aiui"] = {"command": "uvx", "args": ["aiui-mcp"]}
data["mcpServers"] = servers
p.parent.mkdir(parents=True, exist_ok=True)
p.write_text(json.dumps(data, indent=2))
print("ok")
"#;
    run_remote_python(host_alias, script, "Patching ~/.claude.json", |stdout| {
        let confirmed = stdout.trim() == "ok";
        StepResult {
            ok: confirmed,
            message: if confirmed {
                format!("aiui registered in ~/.claude.json on {host_alias}")
            } else {
                format!("Patching ~/.claude.json on {host_alias} did not confirm 'ok'")
            },
            details: if confirmed {
                None
            } else {
                Some(format!("stdout: {}", stdout.trim()))
            },
        }
    })
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

    let child = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "--",
            host_alias,
            "python3 -",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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

pub fn remove_claude_code_config_remote(host_alias: &str) -> StepResult {
    let script = r#"
import json, pathlib
p = pathlib.Path.home() / ".claude.json"
if not p.exists():
    print("ok")
else:
    try:
        data = json.loads(p.read_text())
    except Exception:
        data = {}
    servers = data.get("mcpServers") or {}
    servers.pop("aiui", None)
    data["mcpServers"] = servers
    p.write_text(json.dumps(data, indent=2))
    print("ok")
"#;
    run_remote_python(host_alias, script, "Removing aiui from ~/.claude.json", |stdout| {
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

pub fn remove_token_from_remote(host_alias: &str) -> StepResult {
    if !is_valid_host_alias(host_alias) {
        return StepResult {
            ok: false,
            message: format!("Refusing unsafe host alias '{host_alias}'"),
            details: None,
        };
    }
    let out = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "--",
            host_alias,
            "rm -f ~/.config/aiui/token",
        ])
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

fn remotes_path() -> PathBuf {
    home().join(".config").join("aiui").join("remotes.json")
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
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&p, serde_json::to_string_pretty(list).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_none() {
        assert!(classify_aiui_entry(None).is_none());
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
}
