//! Installs the aiui widget skill into Claude Code's skill directory, both
//! locally (~/.claude/skills/aiui/SKILL.md) and on every registered remote
//! via scp. This is the "Schicht 3" path: real Claude-Code skills the agent
//! picks up automatically at session start.

use crate::logging::trace;
use crate::proc_ext::no_window;
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

    let mkdir = no_window(
        Command::new("ssh").args([
            "-o",
            "BatchMode=yes",
            "--",
            host_alias,
            "mkdir -p ~/.claude/skills/aiui",
        ]),
    )
    .output();
    // #198: this was `if let Ok(o) = &mkdir`, which inspects the failure only
    // on the success branch — when `ssh` itself could not be spawned (no
    // OpenSSH client on Windows, say) execution fell through to `scp` and the
    // user was shown the scp error instead of the real cause. Same shape as
    // `push_token_to_remote`, and as `remove_from_remote` right below.
    match &mkdir {
        Err(e) => {
            let _ = fs::remove_file(&stage);
            return StepResult {
                ok: false,
                message: format!("ssh {host_alias} could not be started"),
                details: Some(e.to_string()),
            };
        }
        Ok(o) if !o.status.success() => {
            let _ = fs::remove_file(&stage);
            return StepResult {
                ok: false,
                message: format!("ssh {host_alias} 'mkdir …' failed"),
                details: Some(String::from_utf8_lossy(&o.stderr).to_string()),
            };
        }
        Ok(_) => {}
    }

    let dest = format!("{host_alias}:.claude/skills/aiui/SKILL.md");
    let out = no_window(Command::new("scp").arg(&stage).arg(&dest)).output();
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
    remove_locally_at(&local_skill_dir())
}

/// The real body, with the skill directory passed in so tests can exercise
/// it without touching `$HOME`.
///
/// #198: both filesystem results used to be discarded with `let _ =` and the
/// step returned `ok: true` regardless — a read-only home or a file owned by
/// another user showed as a green uninstall line while
/// `~/.claude/skills/aiui/SKILL.md` stayed on disk and Claude Code kept
/// loading the aiui skill for a product the user had just uninstalled.
/// A `NotFound` is genuinely nothing to do; anything else is a failure and
/// says so. The trailing directory removal stays best-effort — a non-empty
/// dir means the user put something of their own in there, not a failure.
pub(crate) fn remove_locally_at(dir: &std::path::Path) -> StepResult {
    let file = dir.join("SKILL.md");
    if let Err(e) = fs::remove_file(&file) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return StepResult {
                ok: false,
                message: format!("Lokale Skill-Datei konnte nicht entfernt werden: {}", file.display()),
                details: Some(e.to_string()),
            };
        }
    }
    let _ = fs::remove_dir(dir);
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
    let out = no_window(
        Command::new("ssh").args([
            "-o",
            "BatchMode=yes",
            "--",
            host_alias,
            "rm -f ~/.claude/skills/aiui/SKILL.md; rmdir ~/.claude/skills/aiui 2>/dev/null; true",
        ]),
    )
    .output();
    match out {
        Err(e) => StepResult {
            ok: false,
            message: format!("ssh {host_alias} failed"),
            details: Some(e.to_string()),
        },
        // Report the ssh exit status honestly: callers gate this behind a
        // reachability probe, so a non-success here means the host answered
        // but the removal itself failed — that deserves a red line, not the
        // unconditional green this used to return.
        Ok(o) if !o.status.success() => StepResult {
            ok: false,
            message: format!("Skill-Entfernung auf {host_alias} fehlgeschlagen"),
            details: Some(String::from_utf8_lossy(&o.stderr).to_string()),
        },
        Ok(_) => StepResult {
            ok: true,
            message: format!("Skill auf {host_alias} entfernt."),
            details: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aiui-skill-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn remove_locally_is_ok_when_nothing_to_remove() {
        // #198: a NotFound is not a failure — uninstall on a machine that
        // never had the skill must still report green.
        let dir = tmpdir("absent");
        let r = remove_locally_at(&dir);
        assert!(r.ok, "missing SKILL.md must not turn the step red: {}", r.message);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn remove_locally_reports_failure() {
        // #198: the removal used to be `let _ = fs::remove_file(…)` with an
        // unconditional `ok: true` — state left behind while reporting
        // success. A read-only parent directory is the cheapest way to make
        // the unlink fail for real.
        use std::os::unix::fs::PermissionsExt;

        let parent = tmpdir("readonly");
        let file = parent.join("SKILL.md");
        fs::write(&file, "x").unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();

        let r = remove_locally_at(&parent);

        // Restore before asserting so a failed assertion can't leave an
        // undeletable directory behind in the temp dir.
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        let still_there = file.exists();
        fs::remove_dir_all(&parent).ok();

        // Running as root defeats the permission bits entirely; skip rather
        // than assert something the environment can't produce.
        if !still_there {
            return;
        }
        assert!(!r.ok, "a failed removal must not report success");
        assert!(r.details.is_some(), "the io::Error belongs in details");
    }
}
