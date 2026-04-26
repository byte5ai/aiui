//! Installs the aiui widget skill into Claude Code's skill directory, both
//! locally (~/.claude/skills/aiui/SKILL.md) and on every registered remote
//! via scp. This is the "Schicht 3" path: real Claude-Code skills the agent
//! picks up automatically at session start.

use crate::logging::trace;
use crate::setup::StepResult;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// The skill content is embedded at compile time so the companion doesn't
/// need the repo tree to install it. Source of truth: docs/skill.md.
pub const SKILL_MD: &str = include_str!("../../../docs/skill.md");

fn local_skill_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".claude")
        .join("skills")
        .join("aiui")
}

/// Cheap predicate for the Settings UI: is the local skill file present and
/// non-empty? Used to drive the "Skill installiert ✓" status row that
/// replaces the old "Skill installieren" button. Doesn't try to verify
/// content — just existence — because content stays in sync with the app
/// version automatically (it's overwritten on every GUI launch).
pub fn is_installed_locally() -> bool {
    let path = local_skill_dir().join("SKILL.md");
    fs::metadata(&path)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// Writes SKILL.md into ~/.claude/skills/aiui/ on the local Mac. Idempotent:
/// overwrites any previous copy so skill updates ride along with app updates.
pub fn install_locally() -> StepResult {
    let dir = local_skill_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        return StepResult {
            ok: false,
            message: format!("Could not create {}", dir.display()),
            details: Some(e.to_string()),
        };
    }
    let dest = dir.join("SKILL.md");
    match fs::write(&dest, SKILL_MD) {
        Ok(_) => {
            trace(&format!("skill: wrote {}", dest.display()));
            StepResult {
                ok: true,
                message: format!("aiui-Skill installiert: {}", dest.display()),
                details: None,
            }
        }
        Err(e) => StepResult {
            ok: false,
            message: format!("Could not write {}", dest.display()),
            details: Some(e.to_string()),
        },
    }
}

/// scp the skill to a remote host's ~/.claude/skills/aiui/SKILL.md.
/// Requires passwordless SSH (same requirement as the tunnel manager).
pub fn install_to_remote(host_alias: &str) -> StepResult {
    if !crate::setup::is_valid_host_alias(host_alias) {
        return StepResult {
            ok: false,
            message: format!("Refusing unsafe host alias '{host_alias}'"),
            details: None,
        };
    }
    // stage a temp file so scp has a filename to work with
    let stage = std::env::temp_dir().join(format!("aiui-skill-{}.md", std::process::id()));
    if let Err(e) = fs::write(&stage, SKILL_MD) {
        return StepResult {
            ok: false,
            message: "Could not stage skill for scp".into(),
            details: Some(e.to_string()),
        };
    }

    let mkdir = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "--",
            host_alias,
            "mkdir -p ~/.claude/skills/aiui",
        ])
        .output();
    if let Ok(o) = &mkdir {
        if !o.status.success() {
            let _ = fs::remove_file(&stage);
            return StepResult {
                ok: false,
                message: format!("ssh {host_alias} 'mkdir …' failed"),
                details: Some(String::from_utf8_lossy(&o.stderr).to_string()),
            };
        }
    }

    let dest = format!("{host_alias}:.claude/skills/aiui/SKILL.md");
    let out = Command::new("scp").arg(&stage).arg(&dest).output();
    let _ = fs::remove_file(&stage);
    match out {
        Err(e) => StepResult {
            ok: false,
            message: format!("scp {dest} failed to start"),
            details: Some(e.to_string()),
        },
        Ok(o) if !o.status.success() => StepResult {
            ok: false,
            message: format!("scp {dest} failed"),
            details: Some(String::from_utf8_lossy(&o.stderr).to_string()),
        },
        Ok(_) => StepResult {
            ok: true,
            message: format!("aiui-Skill installiert auf {host_alias}"),
            details: None,
        },
    }
}

/// Removes the local skill file (and empty parent). Counterpart to uninstall.
pub fn remove_locally() -> StepResult {
    let file = local_skill_dir().join("SKILL.md");
    let _ = fs::remove_file(&file);
    let _ = fs::remove_dir(local_skill_dir());
    StepResult {
        ok: true,
        message: "Lokale Skill-Datei entfernt.".into(),
        details: None,
    }
}

/// Counterpart remote cleanup, used from uninstall_all and remove_remote.
pub fn remove_from_remote(host_alias: &str) -> StepResult {
    if !crate::setup::is_valid_host_alias(host_alias) {
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
            "rm -f ~/.claude/skills/aiui/SKILL.md; rmdir ~/.claude/skills/aiui 2>/dev/null; true",
        ])
        .output();
    match out {
        Err(e) => StepResult {
            ok: false,
            message: format!("ssh {host_alias} failed"),
            details: Some(e.to_string()),
        },
        Ok(_) => StepResult {
            ok: true,
            message: format!("Skill auf {host_alias} entfernt."),
            details: None,
        },
    }
}
