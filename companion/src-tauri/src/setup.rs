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
    let was_present = servers.contains_key("aiui-local");
    servers.insert("aiui-local".into(), entry);
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
            message: if was_present {
                "aiui-local in Claude Desktop Config aktualisiert.".into()
            } else {
                "aiui-local zu Claude Desktop Config hinzugefügt.".into()
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
/// - "user@host" → ("host", Some("user"))
/// - "host"      → ("host", None)
/// The block match name is what goes into the "Host <name>" line so SSH
/// config matches on the hostname part of any connection to that host.
fn split_user_host(input: &str) -> (&str, Option<&str>) {
    match input.split_once('@') {
        Some((u, h)) if !u.is_empty() && !h.is_empty() => (h, Some(u)),
        _ => (input, None),
    }
}

pub fn patch_ssh_config(host_alias: &str, hostname: Option<&str>, port: u16) -> StepResult {
    let path = home().join(".ssh").join("config");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let (match_name, user_from_alias) = split_user_host(host_alias);
    let existing = fs::read_to_string(&path).unwrap_or_default();

    // Split into Host blocks. A block starts at a line beginning with "Host ".
    let lines: Vec<&str> = existing.lines().collect();
    let mut blocks: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut preamble_done = false;

    for line in &lines {
        let trimmed = line.trim_start();
        let is_host_line = trimmed.starts_with("Host ") || trimmed == "Host";
        if is_host_line {
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

    let want_remote_forward = format!("    RemoteForward {port} localhost:{port}");
    let want_alive = "    ServerAliveInterval 30".to_string();
    let want_exit_on_fail = "    ExitOnForwardFailure no".to_string();

    let mut found = false;
    let mut added_lines_in_existing = false;
    for block in blocks.iter_mut() {
        // Block header check — match on the hostname part (not the user@ prefix)
        let header_matches = block.first().is_some_and(|l| {
            let t = l.trim_start();
            if let Some(rest) = t.strip_prefix("Host ") {
                rest.split_whitespace().any(|a| a == match_name)
            } else {
                false
            }
        });
        if !header_matches {
            continue;
        }
        found = true;

        let has_line = |prefix: &str| -> bool {
            block
                .iter()
                .any(|l| l.trim_start().to_lowercase().starts_with(&prefix.to_lowercase()))
        };
        let has_port = block.iter().any(|l| {
            let t = l.trim_start().to_lowercase();
            t.starts_with("remoteforward") && t.contains(&format!("{port} localhost:{port}"))
        });
        let has_alive = has_line("ServerAliveInterval");
        let has_exit = has_line("ExitOnForwardFailure");

        if !has_port {
            block.push(want_remote_forward.clone());
            added_lines_in_existing = true;
        }
        if !has_alive {
            block.push(want_alive.clone());
            added_lines_in_existing = true;
        }
        if !has_exit {
            block.push(want_exit_on_fail.clone());
            added_lines_in_existing = true;
        }
        break;
    }

    let mut message;
    if !found {
        // Append new block
        let mut new_block: Vec<String> = Vec::new();
        if !existing.is_empty() && !existing.ends_with('\n') {
            new_block.push("".into());
        }
        new_block.push(format!("Host {match_name}"));
        if let Some(h) = hostname {
            new_block.push(format!("    HostName {h}"));
        }
        if let Some(u) = user_from_alias {
            new_block.push(format!("    User {u}"));
        }
        new_block.push(want_remote_forward);
        new_block.push(want_alive);
        new_block.push(want_exit_on_fail);
        blocks.push(new_block);
        message = format!("Neuer Host-Block '{match_name}' in ~/.ssh/config angelegt.");
    } else if added_lines_in_existing {
        message = format!("Host-Block '{match_name}' in ~/.ssh/config um RemoteForward ergänzt.");
    } else {
        message = format!("Host-Block '{match_name}' hatte bereits alle nötigen Einträge.");
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
            message: "Backup der ~/.ssh/config fehlgeschlagen".into(),
            details: Some(e.to_string()),
        };
    }
    match fs::write(&path, out) {
        Ok(_) => {
            // chmod 600
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(md) = fs::metadata(&path) {
                    let mut p = md.permissions();
                    p.set_mode(0o600);
                    let _ = fs::set_permissions(&path, p);
                }
            }
            message.push_str(&format!(" ({})", path.display()));
            StepResult {
                ok: true,
                message,
                details: None,
            }
        }
        Err(e) => StepResult {
            ok: false,
            message: "Schreiben fehlgeschlagen".into(),
            details: Some(e.to_string()),
        },
    }
}

pub fn push_token_to_remote(host_alias: &str, token_path: &str) -> StepResult {
    // ensure remote dir
    let out1 = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(host_alias)
        .arg("mkdir -p ~/.config/aiui && chmod 700 ~/.config/aiui")
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
    let Some(entry) = v.pointer("/mcpServers/aiui-local") else {
        return false;
    };
    entry
        .get("command")
        .and_then(|v| v.as_str())
        .map(|c| c == app_binary_path)
        .unwrap_or(false)
}

/// Patches `~/.claude.json` so Claude Code CLI sees `aiui` as a globally
/// available MCP server — every session, every project, no per-project
/// .mcp.json required. Uses the public uvx entrypoint on PyPI.
pub fn patch_claude_code_config() -> StepResult {
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
        "command": "uvx",
        "args": ["aiui-mcp"]
    });
    let was_present = servers.contains_key("aiui");
    let already_correct = was_present && servers.get("aiui") == Some(&entry);
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
        Ok(_) => StepResult {
            ok: true,
            message: if was_present {
                "Updated aiui entry in ~/.claude.json".into()
            } else {
                "Added aiui to ~/.claude.json — available in every Claude Code session".into()
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
    let had = v
        .pointer("/mcpServers/aiui-local")
        .is_some();
    if let Some(servers) = v.get_mut("mcpServers").and_then(|x| x.as_object_mut()) {
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
                "aiui-local aus Claude Desktop Config entfernt.".into()
            } else {
                "aiui-local war bereits nicht eingetragen.".into()
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
    let out = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            host_alias,
            "python3 -c \"$1\"",
            "--",
            script,
        ])
        .output();
    match out {
        Err(e) => StepResult {
            ok: false,
            message: format!("ssh {host_alias} could not start"),
            details: Some(e.to_string()),
        },
        Ok(o) if !o.status.success() => StepResult {
            ok: false,
            message: format!("Patching ~/.claude.json on {host_alias} failed"),
            details: Some(String::from_utf8_lossy(&o.stderr).to_string()),
        },
        Ok(_) => StepResult {
            ok: true,
            message: format!("aiui registered in ~/.claude.json on {host_alias}"),
            details: None,
        },
    }
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
    let out = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            host_alias,
            "python3 -c \"$1\"",
            "--",
            script,
        ])
        .output();
    match out {
        Err(e) => StepResult {
            ok: false,
            message: format!("ssh {host_alias} could not start"),
            details: Some(e.to_string()),
        },
        Ok(_) => StepResult {
            ok: true,
            message: format!("Removed aiui from ~/.claude.json on {host_alias}"),
            details: None,
        },
    }
}

pub fn remove_token_from_remote(host_alias: &str) -> StepResult {
    let out = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(host_alias)
        .arg("rm -f ~/.config/aiui/token")
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
