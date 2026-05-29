mod ack;
mod config;
mod dialog;
mod fsutil;
mod housekeeping;
mod http;
mod imageresolve;
mod lifetime;
mod logging;
mod mcp;
mod proc_ext;
mod setup;
mod skill;
mod tunnel;

use std::sync::Arc;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// Tauri window labels. Setup and dialog live in *separate* windows so:
///  • the agent's dialog never visually overlaps the user's settings,
///  • neither window can hide behind the other in macOS' z-stack,
///  • each gets its own movable title bar without weird re-layout
///    artefacts when the content kind changes.
/// See the v0.4.25 multi-window refactor in lib.rs for the lifecycle
/// rules that govern when each is created and torn down.
pub const SETUP_WINDOW_LABEL: &str = "setup";
pub const DIALOG_WINDOW_LABEL: &str = "dialog";

#[tauri::command]
fn dialog_submit(
    state: tauri::State<'_, Arc<dialog::DialogState>>,
    id: String,
    result: serde_json::Value,
) -> Result<(), String> {
    state.complete(&id, result);
    Ok(())
}

#[tauri::command]
fn dialog_cancel(
    state: tauri::State<'_, Arc<dialog::DialogState>>,
    id: String,
) -> Result<(), String> {
    state.cancel(&id);
    Ok(())
}

/// Frontend confirms it received the matching `dialog:show` event. The
/// `/render` handler waits up to 500 ms for this before assuming the WebView
/// event loop is dead and triggering a recreate.
#[tauri::command]
fn dialog_received(
    state: tauri::State<'_, Arc<dialog::DialogState>>,
    id: String,
) -> Result<(), String> {
    state.ack(&id);
    Ok(())
}

/// Frontend response to a `ui:ping` event from `/health`. Same shape as
/// `dialog_received` but routed to the generic ack registry.
#[tauri::command]
fn ui_pong(
    state: tauri::State<'_, Arc<ack::AckRegistry>>,
    id: String,
) -> Result<(), String> {
    state.ack(&id);
    Ok(())
}

/// Frontend signals that the dialog window is mounted and its
/// `dialog:show` / `ui:ping` listeners are registered. The render
/// path on the Rust side waits on this watch *before* emitting, so
/// a freshly-built dialog window never receives a `dialog:show`
/// event before the listener is up. Without this handshake we hit
/// the 500 ms ack timeout, reload the WebView, and lose the user's
/// dialog (the failure mode reported on 2026-05-03).
#[tauri::command]
fn dialog_window_ready(
    tx: tauri::State<'_, Arc<tokio::sync::watch::Sender<bool>>>,
) -> Result<(), String> {
    let _ = tx.send(true);
    Ok(())
}

#[tauri::command]
async fn close_window(window: tauri::WebviewWindow) -> Result<(), String> {
    // The frontend calls this after a dialog submit/cancel. We *destroy*
    // the dialog window (not hide) so the next render starts from a clean
    // slate — no stale Svelte state, no z-order quirks, no visible frame
    // sitting empty. The setup window calls this too if the user clicks
    // its custom close button (none today, but the contract should be
    // symmetric).
    let label = window.label().to_string();
    // App handle only needed for the macOS dock-demote path below; bind
    // it inside that cfg-block so Windows builds don't see it as unused.
    #[cfg(target_os = "macos")]
    let app = window.app_handle().clone();
    let _ = window.close();
    log::debug!("[aiui] close_window: closed {label}");

    // If that was the dialog window and no setup window is open,
    // demote the app back to Accessory mode so we don't permanently
    // grow a Dock icon. `ensure_dialog_window` promotes us to Regular
    // for the dialog's lifetime; this is the matching demote.
    #[cfg(target_os = "macos")]
    if label == DIALOG_WINDOW_LABEL {
        let setup_open = app
            .get_webview_window(SETUP_WINDOW_LABEL)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false);
        if !setup_open {
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
        }
    }
    Ok(())
}

/// Authoritatively tear down the dialog window from the Rust side
/// (v0.4.46, Bug B). Uses `destroy()` (immediate) rather than `close()`
/// so it bypasses the `CloseRequested` → frontend round-trip that could
/// strand an empty window when the WebView's handler failed to complete
/// the close. This is the single teardown point for the dialog window:
/// the `/render` handler calls it once a render reaches *any* terminal
/// outcome (submit, cancel, X-close, TTL, channel-drop), so the window
/// can never outlive the dialog it was showing. Idempotent — a no-op
/// when the window is already gone.
pub(crate) fn destroy_dialog_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(DIALOG_WINDOW_LABEL) {
        let _ = win.destroy();
    }
    // Matching demote for `ensure_dialog_window`'s Regular-mode promote:
    // drop back to Accessory once the dialog is gone, unless the setup
    // window is still up.
    #[cfg(target_os = "macos")]
    {
        let setup_open = app
            .get_webview_window(SETUP_WINDOW_LABEL)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false);
        if !setup_open {
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
        }
    }
}

/// Belt-and-suspenders invariant (v0.4.46, Bug B+): a dialog window may
/// only exist while a dialog is pending in the registry. If a window is
/// found with an empty registry, it's a stranded empty window — destroy
/// it. Cheap (one mutex read + a window lookup); called on app
/// re-activation, exactly when a user would otherwise notice a leftover
/// empty frame.
pub(crate) fn sweep_orphan_dialog_window(app: &tauri::AppHandle) {
    let pending = app
        .try_state::<Arc<dialog::DialogState>>()
        .map(|s| s.stats().orphan_count)
        .unwrap_or(0);
    if pending == 0 && app.get_webview_window(DIALOG_WINDOW_LABEL).is_some() {
        log::debug!("[aiui] sweep: destroying orphan dialog window (no pending dialog)");
        destroy_dialog_window(app);
    }
}

/// Frontend silent-update gate (v0.4.43): returns `true` iff installing
/// + relaunching right now would NOT disrupt anything the user is
/// currently looking at. The silent updater path in `updater.ts` calls
/// this before `downloadAndInstall` so a post-render auto-check can
/// install a pending update **only** while the dialog window is idle.
/// Without this, the v0.4.39 silent-mode-too-silent regression kept
/// updates from ever shipping automatically — the v0.4.43 design is
/// "install transparently when safe, never interrupt a live form".
///
/// Criterion: the dialog registry is empty (no pending render-call
/// waiting on user input). We deliberately don't gate on Settings
/// being open — the user is in Settings *intentionally*; an install
/// + relaunch there is fine.
#[tauri::command]
async fn is_update_safe_to_install(
    dialog_state: tauri::State<'_, Arc<dialog::DialogState>>,
) -> Result<bool, String> {
    Ok(dialog_state.stats().orphan_count == 0)
}

/// Called from the frontend right before showing a modal update dialog.
/// An Accessory-mode app (LSUIElement) doesn't own a Dock entry, and macOS
/// won't reliably bring its dialogs to the foreground — we temporarily
/// promote the app to Regular so the prompt actually becomes visible.
#[tauri::command]
async fn surface_for_dialog(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    }
    // The update dialog is surfaced from whichever window is alive when
    // the check fires — usually the setup window (frontend triggers it
    // from there). We just need *some* visible window to attach the OS
    // dialog to.
    let win = app
        .get_webview_window(SETUP_WINDOW_LABEL)
        .or_else(|| app.get_webview_window(DIALOG_WINDOW_LABEL));
    if let Some(win) = win {
        let _ = win.show();
        let _ = win.set_focus();
    }
    Ok(())
}

/// Pending-update state (v0.4.44). Holds the version string Tauri's
/// updater plugin reported as the newest available release, set by
/// the silent auto-check path; consumed by the Settings banner. Newtype
/// so Tauri's `State<T>` can distinguish it from other
/// `Arc<Mutex<Option<String>>>`-typed state (notably `http_error`).
#[derive(Default)]
pub struct PendingUpdate(pub std::sync::Mutex<Option<String>>);

#[tauri::command]
async fn set_pending_update(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<PendingUpdate>>,
    version: String,
) -> Result<(), String> {
    let trimmed = version.trim();
    let new_value = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
    if let Ok(mut slot) = state.0.lock() {
        *slot = new_value.clone();
    }
    // Notify every window — both setup and dialog can re-read status
    // and refresh their banner accordingly. The dialog window will
    // typically ignore it, the setup window's banner reacts.
    let _ = app.emit("update:available", &new_value);
    Ok(())
}

#[tauri::command]
async fn clear_pending_update(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<PendingUpdate>>,
) -> Result<(), String> {
    if let Ok(mut slot) = state.0.lock() {
        *slot = None;
    }
    let _ = app.emit("update:available", &None::<String>);
    Ok(())
}

#[derive(serde::Serialize)]
struct StatusReport {
    app_binary_path: String,
    token_path: String,
    http_port: u16,
    /// True iff `claude_desktop_config.json` has the current `aiui` MCP
    /// server entry pointing at this binary. Mirrors what the welcome
    /// banner uses for its readiness check.
    claude_config_ok: bool,
    /// True iff `~/.claude.json` has an `aiui` MCP server entry pointing at
    /// this binary. Separate from `claude_config_ok` because Claude Desktop
    /// and Claude Code read different config files.
    claude_code_config_ok: bool,
    /// True iff `~/.claude/skills/aiui/SKILL.md` exists and is non-empty.
    /// Drives the skill-status row in Settings — replaces the old
    /// "Skill installieren" button which suggested optionality.
    skill_installed: bool,
    /// True iff the Claude Desktop app is currently running. Lets the
    /// "Restart Claude Desktop" button switch its label between
    /// "Start" / "Restart" depending on whether there's something to quit.
    claude_desktop_running: bool,
    remotes: Vec<String>,
    tunnels: std::collections::HashMap<String, tunnel::TunnelStatus>,
    build_info: &'static str,
    /// True until the user dismisses the welcome section. Drives the
    /// onboarding banner in the Settings UI — they see it on the very
    /// first launch and on every subsequent launch where they haven't
    /// clicked "Got it" yet.
    welcome_pending: bool,
    /// `Some(message)` if the HTTP server failed to bind/serve. Drives a
    /// red banner in Settings so the user knows why dialogs aren't
    /// landing.
    http_error: Option<String>,
    /// Live result of a TCP self-probe to `localhost:http_port`. The Rust
    /// side does this for us because a WebView `fetch()` would be blocked
    /// by macOS App Transport Security (ATS) on plaintext localhost
    /// requests — that's how v0.4.8 ended up showing a permanent red
    /// banner on a perfectly healthy server. Issue #77.
    http_alive: bool,
    /// Lower-case OS identifier — `"macos"`, `"windows"`, `"linux"`, or
    /// `"other"`. Lets the Svelte side render OS-specific copy (e.g. the
    /// uninstall instructions: drag to Trash vs. Apps & Features) without
    /// pulling in `@tauri-apps/plugin-os`. Set once at compile time.
    os: &'static str,
    /// `Some(version)` when the periodic auto-update check has found a
    /// newer release that hasn't been installed yet. Drives the
    /// non-modal banner in Settings ("Update auf v0.4.X verfügbar —
    /// Installieren"). v0.4.44.
    pending_update: Option<String>,
}

const fn current_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

#[tauri::command]
async fn status(
    cfg: tauri::State<'_, Arc<config::AppConfig>>,
    tm: tauri::State<'_, Arc<tunnel::TunnelManager>>,
    http_err: tauri::State<'_, Arc<std::sync::Mutex<Option<String>>>>,
    pending_update: tauri::State<'_, Arc<PendingUpdate>>,
) -> Result<StatusReport, String> {
    let bin = setup::app_binary_path();
    let http_alive = probe_http_self(&cfg).await;
    Ok(StatusReport {
        app_binary_path: bin.clone(),
        token_path: cfg.token_path.display().to_string(),
        http_port: cfg.http_port,
        claude_config_ok: setup::is_claude_config_current(&bin),
        claude_code_config_ok: setup::is_claude_code_config_current(&bin),
        skill_installed: skill::is_installed_locally(),
        claude_desktop_running: setup::is_claude_desktop_running(),
        remotes: setup::load_remotes(),
        tunnels: tm.snapshot().await,
        build_info: logging::BUILD_INFO,
        welcome_pending: is_first_run(&cfg),
        http_error: http_err.lock().ok().and_then(|s| s.clone()),
        http_alive,
        os: current_os(),
        pending_update: pending_update.0.lock().ok().and_then(|s| s.clone()),
    })
}

/// Authenticated HTTP self-probe to verify our own HTTP server is
/// actually serving aiui. A naked TCP connect would lie positive when an
/// SSH-session squatter or any other process happens to hold the port in
/// LISTEN — the kernel answers SYN regardless of who's behind it. Issue
/// #77 (revised in v0.4.10): we hit `/probe` with our bearer token and
/// verify the response carries the aiui marker. Anything else (squatter
/// without our token, non-aiui content, timeout) reads as "down".
///
/// 500 ms timeout to cover token-read + HTTP round-trip + JSON parse
/// over loopback; this stays well under the Settings refresh interval.
async fn probe_http_self(cfg: &config::AppConfig) -> bool {
    let token = match std::fs::read_to_string(&cfg.token_path) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return false,
    };
    let url = format!("http://127.0.0.1:{}/probe", cfg.http_port);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let resp = match client.get(&url).bearer_auth(&token).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return false,
    };
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return false,
    };
    body.get("aiui")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Marks the welcome banner as dismissed so it doesn't reappear on the
/// next launch. Frontend calls this when the user clicks "Got it" on the
/// first-run welcome section.
#[tauri::command]
fn dismiss_welcome(cfg: tauri::State<'_, Arc<config::AppConfig>>) -> Result<(), String> {
    mark_first_run_done(&cfg);
    Ok(())
}

/// Re-installs the local skill file. Bound to the "Skill reparieren" button
/// in the Settings status row, which only appears when `skill_installed`
/// reports false. The auto-install on every GUI launch covers the normal
/// case; this command is for the rare situation where the file got removed
/// or corrupted between launches.
#[tauri::command]
fn repair_skill() -> Result<setup::StepResult, String> {
    Ok(skill::install_locally())
}

/// Open a URL in the user's default browser. Tauri's WebView blocks
/// `window.open()` calls from JavaScript for security, so the
/// "Problem melden"-button (and any other future external-link case)
/// has to round-trip through Rust. Issue surfaced 2026-04-27 by tester
/// clicking the button for the first time.
///
/// The dispatch goes through the `open` crate, which calls
/// `ShellExecuteW` on Windows, `/usr/bin/open` on macOS, and `xdg-open`
/// on Linux/BSD. None of those routes the URL through a shell, so
/// metacharacters (`&`, `|`, `^`, …) inside the URL stay inert — closing
/// the command-injection surface that the previous `cmd /C start "" …`
/// path had on Windows (Codex review of PR #128).
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    // Sanity-check: only allow http(s) so a compromised renderer can't
    // smuggle file:// or shell URIs through this command. The downstream
    // launcher would also refuse most of those, but we want a single
    // explicit gate at the entry.
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("refusing non-http(s) URL: {url}"));
    }
    open::that(&url).map_err(|e| format!("open {url}: {e}"))?;
    Ok(())
}

/// Quit aiui after Uninstall has cleaned up configs/tokens/skill, killing
/// every `aiui --mcp-stdio` child first so the auto-resurrect path in
/// `mcp_attach` can't relaunch the GUI behind us. Without this, the user
/// still couldn't drag aiui.app to the Trash because the process kept
/// running. Issue #72.
#[tauri::command]
async fn quit_app(app: tauri::AppHandle) -> Result<(), String> {
    let killed = housekeeping::kill_all_mcp_stdio_children();
    logging::trace(&format!(
        "quit_app: killed {killed} mcp-stdio child(ren) before exit"
    ));
    // Give the kill commands a moment to deliver SIGTERM before we exit
    // ourselves. Otherwise an already-running mcp_attach loop on a child
    // can race the GUI exit and re-launch us.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let port = app
        .try_state::<Arc<config::AppConfig>>()
        .map(|c| c.http_port)
        .unwrap_or(7777);
    housekeeping::pre_exit_cleanup(port, "quit_app/uninstall");
    app.exit(0);
    Ok(())
}

/// Quit + relaunch Claude Desktop so it re-reads `claude_desktop_config.json`
/// and picks up the freshly-patched aiui MCP server entry. This is the
/// "after-Setup nudge" the user otherwise has to figure out themselves.
///
/// Per-OS implementation:
///
/// - macOS: AppleScript quit (graceful, lets Claude clean up) + `open -a`
///   for the relaunch. Both no-op silently if Claude isn't installed/running.
///
/// - Windows: `taskkill /IM Claude.exe` (no /F, that would SIGKILL) +
///   re-launch via the resolved install path. Claude Desktop on Windows
///   ships either as a per-user NSIS install (default
///   `%LOCALAPPDATA%\Programs\claude-desktop\Claude.exe`) or via the
///   maintainer's MSIX in `%LOCALAPPDATA%\AnthropicClaude\Claude.exe`.
///   We probe both; whichever exists wins. If neither does, surface a
///   clear error instead of silently no-oping.
#[tauri::command]
async fn restart_claude_desktop() -> Result<setup::StepResult, String> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;

        // Best-effort quit. Status of `osascript` is non-fatal — if Claude
        // isn't running, AppleScript returns an error that we treat as a
        // no-op.
        let _ = Command::new("osascript")
            .args(["-e", "tell application \"Claude\" to quit"])
            .output();

        // Give Claude a moment to actually shut down before relaunching,
        // so `open -a` doesn't race with a still-quitting instance.
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        let out = Command::new("open").args(["-a", "Claude"]).output();
        match out {
            Ok(o) if o.status.success() => Ok(setup::StepResult {
                ok: true,
                message: "Claude Desktop neu gestartet — neuer aiui-Eintrag wird beim Boot geladen.".into(),
                details: None,
            }),
            Ok(o) => Ok(setup::StepResult {
                ok: false,
                message: "Konnte Claude Desktop nicht starten.".into(),
                details: Some(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            }),
            Err(e) => Ok(setup::StepResult {
                ok: false,
                message: "Konnte `open -a Claude` nicht ausführen.".into(),
                details: Some(e.to_string()),
            }),
        }
    }
    #[cfg(target_os = "windows")]
    {
        use crate::proc_ext::no_window;
        use std::path::PathBuf;
        use std::process::Command;

        // Best-effort graceful close (no `/F`). If Claude is already gone
        // taskkill returns exit code 128 — we don't surface that.
        let _ = no_window(
            Command::new("taskkill").args(["/IM", "Claude.exe"]),
        )
        .output();

        // Capture the path of the running Claude.exe *before* we tell it
        // to quit — that's the most reliable way to find the right binary
        // (covers MSIX/Store installs, custom install dirs, renames),
        // beating any LOCALAPPDATA-based guess. After taskkill the process
        // is gone and `sysinfo` would no longer see it, so the lookup
        // happens here.
        //
        // Codex review of PR #128: the previous logic just took the first
        // existing candidate path, which silently picks the older binary
        // when both NSIS and MSIX installs coexist (mid-upgrade), and
        // misses Store-installed Claudes entirely.
        let running_exe: Option<PathBuf> = {
            use sysinfo::{ProcessRefreshKind, RefreshKind, System};
            let sys = System::new_with_specifics(
                RefreshKind::new().with_processes(ProcessRefreshKind::everything()),
            );
            sys.processes()
                .values()
                .find(|p| {
                    p.name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case("Claude.exe")
                })
                .and_then(|p| p.exe().map(|e| e.to_path_buf()))
        };

        // Best-effort graceful close (no `/F`). Issued *after* we captured
        // the path so we don't race with Windows tearing the process
        // entry down. If Claude is already gone taskkill returns exit
        // code 128 — we don't surface that.
        let _ = no_window(
            Command::new("taskkill").args(["/IM", "Claude.exe"]),
        )
        .output();

        // Give Claude a moment to actually exit before relaunching.
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        // Hard-coded fallback paths covering the two known per-user
        // installer flavours. Used only when no Claude.exe is currently
        // running (cold-start case) — the live-process lookup above wins
        // whenever it has a result.
        let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let candidates: Vec<PathBuf> = if local_app_data.is_empty() {
            Vec::new()
        } else {
            vec![
                PathBuf::from(&local_app_data)
                    .join("Programs")
                    .join("claude-desktop")
                    .join("Claude.exe"),
                PathBuf::from(&local_app_data)
                    .join("AnthropicClaude")
                    .join("Claude.exe"),
            ]
        };

        // Resolution order: live process path → first existing fallback.
        let exe: Option<PathBuf> = running_exe
            .clone()
            .or_else(|| candidates.iter().find(|p| p.exists()).cloned());

        let Some(exe) = exe else {
            // Surface every path we tried so the user can compare against
            // their actual install location and report it back.
            let mut tried: Vec<String> = candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            tried.insert(0, "(running Claude.exe — none found)".to_string());
            return Ok(setup::StepResult {
                ok: false,
                message: "Claude Desktop nicht gefunden.".into(),
                details: Some(format!(
                    "Gesucht: {}. Bitte Claude Desktop manuell neu starten und den tatsächlichen Pfad melden, damit aiui ihn künftig findet.",
                    tried.join(", "),
                )),
            });
        };

        // Spawn detached — we're the parent of a brand-new Claude process,
        // but we don't want to keep its lifetime tied to ours.
        match no_window(&mut Command::new(&exe)).spawn() {
            Ok(_) => Ok(setup::StepResult {
                ok: true,
                message: "Claude Desktop neu gestartet — neuer aiui-Eintrag wird beim Boot geladen.".into(),
                details: Some(format!("Gestartet: {}", exe.display())),
            }),
            Err(e) => Ok(setup::StepResult {
                ok: false,
                message: "Konnte Claude Desktop nicht starten.".into(),
                details: Some(format!("{}: {e}", exe.display())),
            }),
        }
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        Ok(setup::StepResult {
            ok: false,
            message: "Restart Claude Desktop wird auf dieser Plattform nicht unterstützt.".into(),
            details: None,
        })
    }
}

#[tauri::command]
async fn add_remote(
    host_alias: String,
    cfg: tauri::State<'_, Arc<config::AppConfig>>,
    tm: tauri::State<'_, Arc<tunnel::TunnelManager>>,
) -> Result<Vec<setup::StepResult>, String> {
    // Validate at the API boundary: anything that doesn't pass
    // `is_valid_host_alias` is rejected here, before we spawn ssh or
    // touch persistent state. This is the primary defense against
    // option-injection via `host_alias`. Per-helper validators below
    // are defense-in-depth for callers that bypass this entry.
    if !setup::is_valid_host_alias(&host_alias) {
        return Ok(vec![setup::StepResult {
            ok: false,
            message: format!("Refusing unsafe host alias '{host_alias}'"),
            details: Some(
                "Allowed: letters, digits, '.', '_', '-' (and '+' in the user). \
                 No leading '-', no whitespace, no shell metacharacters."
                    .into(),
            ),
        }]);
    }

    let mut results = Vec::new();

    // Pre-flight: verify `uvx aiui-mcp` actually resolves on the remote
    // before we touch any persistent state. Without this, add_remote
    // silently writes `{"command": "uvx", "args": ["aiui-mcp"]}` to a
    // ~/.claude.json on a host that has no uv installed — every Claude
    // tool call afterwards errors with a confusing "command not found".
    // The probe also surfaces the absolute uvx path discovered on the
    // remote so we can pin the ~/.claude.json entry to that path,
    // sidestepping any PATH-issues at Claude-Code-spawn time.
    let (reach_step, uvx_loc) = setup::check_remote_aiui_mcp(&host_alias);
    let reach_ok = reach_step.ok;
    results.push(reach_step);
    if !reach_ok {
        // Bail before persisting. Token push, ssh-config edit, tunnel
        // start — none of it is useful if the MCP entry won't resolve.
        results.push(setup::StepResult {
            ok: false,
            message: format!(
                "Setup für '{host_alias}' abgebrochen — uvx aiui-mcp ist auf dem Host nicht erreichbar."
            ),
            details: Some(
                "Installiere uv auf dem Remote (https://docs.astral.sh/uv/) und versuche es erneut.".into(),
            ),
        });
        return Ok(results);
    }

    // Legacy cleanup: earlier versions (≤ v0.1.1) patched the user's
    // ~/.ssh/config with a RemoteForward line. aiui now owns the tunnel
    // entirely via its own `ssh -NTR` subprocess; strip any leftover lines
    // from past installs so we don't fight them over port 7777.
    let _ = setup::remove_ssh_forward(&host_alias, cfg.http_port);

    // Run the three setup steps. Treat token push and config patch as
    // *blocking* — without them the remote can't talk to us. Skill
    // install is treated as non-blocking (warn but proceed) since a
    // missing skill only degrades agent UX, not connectivity.
    let token_path = cfg.token_path.display().to_string();
    let token_step = setup::push_token_to_remote(&host_alias, &token_path);
    let token_ok = token_step.ok;
    results.push(token_step);

    let skill_step = skill::install_to_remote(&host_alias);
    results.push(skill_step);

    let (config_step, config_patch) = setup::patch_claude_code_config_remote(
        &host_alias,
        uvx_loc.as_ref().map(|l| l.uvx_path.as_str()),
        env!("CARGO_PKG_VERSION"),
    );
    let config_ok = config_step.ok;
    results.push(config_step);
    // Fresh add — there shouldn't be a running child yet, but a
    // re-add (Remove + Add the same host) leaves stale ones; sweep
    // them so the first tool call respawns clean against the new pin.
    if matches!(config_patch, Some(setup::RemoteConfigPatch::Patched)) {
        let sweep = setup::kill_remote_mcp_stdio(&host_alias);
        if !sweep.ok {
            results.push(sweep);
        }
    }

    if !(token_ok && config_ok) {
        // Don't persist the host or start a tunnel for a half-failed
        // setup. The user sees the per-step error in the log and can
        // retry. Token may already be on the remote — that's harmless.
        results.push(setup::StepResult {
            ok: false,
            message: format!(
                "Setup für '{host_alias}' nicht abgeschlossen — Host nicht eingetragen."
            ),
            details: Some(
                "Token-Push und Config-Patch müssen erfolgreich sein. \
                 Behebe die Ursache und versuche es erneut."
                    .into(),
            ),
        });
        return Ok(results);
    }

    let mut list = setup::load_remotes();
    if !list.contains(&host_alias) {
        list.push(host_alias.clone());
        let _ = setup::save_remotes(&list);
    }
    tm.ensure(host_alias).await;
    Ok(results)
}

#[tauri::command]
async fn reinstall_skill() -> Result<Vec<setup::StepResult>, String> {
    let mut results = vec![skill::install_locally()];
    for host in setup::load_remotes() {
        results.push(skill::install_to_remote(&host));
    }
    Ok(results)
}

/// On-demand resync trigger for a single registered remote — wraps
/// the same patch-pin + kill-stale-mcp-stdio sequence that runs in
/// the background at every aiui-app startup. Surfaced as a per-remote
/// button in Settings so the user can re-invoke it without restarting
/// aiui (and see the StepResult log inline if a sweep fails).
///
/// Why this exists: 0.4.29's auto-resync on GUI-start is silent — if
/// the SSH-side `pkill` fails (remote temporarily unreachable) the
/// stale subprocess keeps running with the previous version. Without
/// a manual trigger, the user would have to close + reopen aiui-app
/// to retry. v0.4.34 adds the on-demand path.
#[tauri::command]
async fn resync_remote(
    host_alias: String,
) -> Result<Vec<setup::StepResult>, String> {
    let our_version = env!("CARGO_PKG_VERSION");
    // Re-pin in `~/.claude.json` on the remote (idempotent — if
    // already pinned, no rewrite, returns AlreadyCurrent).
    let (pin_step, patch) = setup::patch_claude_code_config_remote(
        &host_alias,
        None,
        our_version,
    );
    let mut results = vec![pin_step];
    // Sweep stale aiui-mcp children only when the pin actually
    // changed (or unconditionally? — yes, unconditionally on
    // user-triggered resync, because the user wouldn't click resync
    // unless they suspect drift). On unconditional sweep: kills any
    // running aiui-mcp regardless of pin state, which is what the
    // user wants from a "force fresh" button.
    let _ = patch;  // not used here, but kept for tracing
    results.push(setup::kill_remote_mcp_stdio(&host_alias));
    Ok(results)
}

#[tauri::command]
async fn remove_remote(
    host_alias: String,
    cfg: tauri::State<'_, Arc<config::AppConfig>>,
    tm: tauri::State<'_, Arc<tunnel::TunnelManager>>,
) -> Result<Vec<setup::StepResult>, String> {
    // Stop the tunnel first so the forward port is freed before we touch
    // ssh config and remote token.
    tm.stop(&host_alias).await;
    let results = vec![
        setup::remove_ssh_forward(&host_alias, cfg.http_port),
        setup::remove_token_from_remote(&host_alias),
        setup::remove_claude_code_config_remote(&host_alias),
        skill::remove_from_remote(&host_alias),
    ];
    let list: Vec<String> = setup::load_remotes()
        .into_iter()
        .filter(|h| h != &host_alias)
        .collect();
    let _ = setup::save_remotes(&list);
    Ok(results)
}

/// Uninstall hint shown after the cleanup sweep — tells the user how to
/// remove the app bundle itself, which aiui can't do for itself
/// (a running process can't delete its own binary on either OS).
fn uninstall_app_removal_hint() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Verschiebe /Applications/aiui.app in den Papierkorb, um auch die App zu entfernen."
    }
    #[cfg(target_os = "windows")]
    {
        "Deinstalliere aiui über \"Apps & Features\" in den Windows-Einstellungen, um auch die App zu entfernen."
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        "Entferne das aiui-Binary manuell, um auch die App zu entfernen."
    }
}

#[tauri::command]
async fn uninstall_all(
    cfg: tauri::State<'_, Arc<config::AppConfig>>,
    tm: tauri::State<'_, Arc<tunnel::TunnelManager>>,
) -> Result<Vec<setup::StepResult>, String> {
    tm.stop_all().await;
    let mut results = Vec::new();
    results.push(setup::remove_claude_desktop_config());
    results.push(setup::remove_claude_code_config());
    for host in setup::load_remotes() {
        results.push(setup::remove_ssh_forward(&host, cfg.http_port));
        results.push(setup::remove_token_from_remote(&host));
        results.push(setup::remove_claude_code_config_remote(&host));
        results.push(skill::remove_from_remote(&host));
    }
    results.push(skill::remove_locally());
    let _ = std::fs::remove_file(&cfg.token_path);
    let _ = std::fs::remove_file(cfg.config_dir.join("first_run_done"));
    let _ = setup::save_remotes(&[]);
    results.push(setup::StepResult {
        ok: true,
        message: format!(
            "Lokale Dateien entfernt: {}",
            cfg.config_dir.display()
        ),
        details: Some(uninstall_app_removal_hint().into()),
    });
    Ok(results)
}

fn is_first_run(cfg: &config::AppConfig) -> bool {
    !cfg.config_dir.join("first_run_done").exists()
}

fn mark_first_run_done(cfg: &config::AppConfig) {
    let _ = std::fs::write(cfg.config_dir.join("first_run_done"), b"");
}

fn show_settings_window(app: &tauri::AppHandle) {
    // When the settings window surfaces we are in "user-facing" mode:
    // show a Dock icon and cmd-tab entry.
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    }
    if let Some(win) = app.get_webview_window(SETUP_WINDOW_LABEL) {
        let _ = win.show();
        let _ = win.set_focus();
        let _ = win.unminimize();
        return;
    }
    if let Err(e) = build_setup_window(app) {
        log::error!("[aiui] failed to build setup window: {e}");
    }
}

/// Build the setup (settings) window. Same dimensions as the legacy
/// single-window setup: 520×480, fixed width, height capped at 640.
/// `dragDropEnabled: false` because Sortable.js uses HTML5 DnD and
/// Tauri's window-level file-drop interception steals those events.
pub(crate) fn build_setup_window(
    app: &tauri::AppHandle,
) -> tauri::Result<tauri::WebviewWindow> {
    WebviewWindowBuilder::new(
        app,
        SETUP_WINDOW_LABEL,
        WebviewUrl::App("setup.html".into()),
    )
    .title("aiui")
    .inner_size(520.0, 480.0)
    .min_inner_size(520.0, 380.0)
    .max_inner_size(520.0, 640.0)
    .resizable(false)
    .center()
    // Native, fully-visible title bar so macOS handles window-drag
    // for us. Tauri's `data-tauri-drag-region` HTML attribute and
    // Chromium's `-webkit-app-region: drag` CSS are *both* unreliable
    // on Tauri 2 + WKWebView (macOS 26): the first sometimes drops
    // mousedown depending on z-order, the second is a Chromium-only
    // CSS property that WKWebView doesn't honour at all. The only
    // robust path is to let macOS run its own title-bar drag, which
    // means a visible title bar (the previous "Overlay + hiddenTitle"
    // setup hid the title-bar pixels but kept its drag behaviour
    // half-broken). We accept the slightly-less-flush look in
    // exchange for a window the user can actually move.
    .decorations(true)
    .disable_drag_drop_handler()
    .visible(true)
    .build()
}

/// Build (or surface) the dialog window. Called from the render path
/// when a `confirm` / `ask` / `form` arrives. Same look as the setup
/// window so the user gets a consistent aiui chrome regardless of
/// which view they're seeing — the *content* is what differs.
///
/// Reused across renders: if a dialog window already exists, we surface
/// it and resize it to the new spec's estimated size — small confirm
/// after a wide form shouldn't keep the wide form's geometry, and
/// vice versa. Frontend handles the actual content swap when the
/// `dialog:show` event arrives.
///
/// `size` is the per-spec inner-size estimate from
/// `dialog::estimate_dialog_size`. The window is resizable, so the user
/// can drag past these defaults — we just pick a sensible starting
/// geometry given what the agent asked us to render.
pub(crate) fn ensure_dialog_window(
    app: &tauri::AppHandle,
    size: (f64, f64),
) -> tauri::Result<tauri::WebviewWindow> {
    // Promote the app from Accessory to Regular for the duration of the
    // dialog. In Accessory mode (LSUIElement-style daemon, no Dock icon)
    // macOS won't bring our windows to the front above other apps even
    // with `set_focus()` — the agent renders a dialog and the user
    // doesn't see it because Claude Desktop covers it. Promoting to
    // Regular for the dialog window restores normal front/focus
    // behaviour; we drop back to Accessory in `close_window` once the
    // dialog finishes so we don't permanently grow a Dock icon.
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    }
    if let Some(win) = app.get_webview_window(DIALOG_WINDOW_LABEL) {
        // Resize to fit the new spec before surfacing. Without this,
        // a confirm rendered after a long form would keep the form's
        // tall geometry (and vice versa).
        let _ = win.set_size(tauri::LogicalSize::new(size.0, size.1));
        let _ = win.show();
        let _ = win.set_focus();
        let _ = win.unminimize();
        // Briefly mark the window always-on-top to win against any
        // app that's grabbed focus in the meantime, then lift the
        // flag so the user can naturally Cmd+Tab away later. 800 ms
        // is enough for the activation to settle without leaving a
        // sticky front-most window.
        let _ = win.set_always_on_top(true);
        let app_for_lift = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(800));
            if let Some(w) = app_for_lift.get_webview_window(DIALOG_WINDOW_LABEL) {
                let _ = w.set_always_on_top(false);
            }
        });
        return Ok(win);
    }
    // Window is being built fresh — its frontend listeners aren't up
    // yet. Reset the ready flag so the render path waits for the
    // `dialog_window_ready` signal before emitting `dialog:show`.
    if let Some(tx) = app.try_state::<Arc<tokio::sync::watch::Sender<bool>>>() {
        let _ = tx.inner().send(false);
    }
    WebviewWindowBuilder::new(
        app,
        DIALOG_WINDOW_LABEL,
        WebviewUrl::App("dialog.html".into()),
    )
    .title("aiui")
    // Initial size from `estimate_dialog_size` — we widen for
    // wireframe/mermaid/table and grow vertically for long forms,
    // clamped to (1100, 900). Resizable so the user always has the
    // last word; min size keeps the dialog usable but prevents
    // accidental sub-icon collapse. v0.4.40.
    .inner_size(size.0, size.1)
    .min_inner_size(360.0, 320.0)
    .resizable(true)
    .center()
    // Native, fully-visible title bar so macOS handles window-drag
    // for us. Tauri's `data-tauri-drag-region` HTML attribute and
    // Chromium's `-webkit-app-region: drag` CSS are *both* unreliable
    // on Tauri 2 + WKWebView (macOS 26): the first sometimes drops
    // mousedown depending on z-order, the second is a Chromium-only
    // CSS property that WKWebView doesn't honour at all. The only
    // robust path is to let macOS run its own title-bar drag, which
    // means a visible title bar (the previous "Overlay + hiddenTitle"
    // setup hid the title-bar pixels but kept its drag behaviour
    // half-broken). We accept the slightly-less-flush look in
    // exchange for a window the user can actually move.
    .decorations(true)
    .disable_drag_drop_handler()
    .visible(true)
    .always_on_top(true)
    .build()
    .inspect(|_win| {
        // Fresh dialog windows also get the same lift-after-800 ms
        // treatment as the reused-window branch above. The
        // always_on_top flag from the builder ensures the window
        // appears above everything; we drop it shortly after so
        // Cmd+Tab works normally afterwards.
        let app_for_lift = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(800));
            if let Some(w) = app_for_lift.get_webview_window(DIALOG_WINDOW_LABEL) {
                let _ = w.set_always_on_top(false);
            }
        });
    })
}

/// True when no aiui window is currently visible to the user. Used by
/// the close-event handler to decide whether to keep the app alive
/// (something else is open, e.g. a still-pending dialog) or to quit.
#[allow(dead_code)]
fn no_visible_windows(app: &tauri::AppHandle) -> bool {
    app.webview_windows()
        .values()
        .all(|w| !w.is_visible().unwrap_or(false))
}

fn is_auto_launch() -> bool {
    std::env::args().any(|a| a == "--auto")
}

/// Runs the MCP-stdio side only: NO Tauri GUI, NO HTTP server. This process
/// is spawned by Claude Desktop and talks JSON-RPC on stdin/stdout. It also
/// attaches to the GUI process via the lifetime socket so the GUI knows we're
/// alive (and can self-terminate when we die).
pub fn run_mcp_stdio_only() {
    // Stale-binary self-check (runs before any state is touched). On
    // macOS, an in-place `.app` replacement (in-app updater, manual DMG
    // drop) leaves any already-running mcp-stdio child holding the
    // *previous* binary in memory while the on-disk path now points at
    // the new one. The GUI-side sweep can't see that: the path matches.
    // Result: stale logic answering tool calls until Claude Desktop
    // restarts, manifesting as silent crashes during dispatch.
    //
    // We compare our compile-time `CARGO_PKG_VERSION` with the bundle's
    // on-disk `CFBundleShortVersionString`. Mismatch → exit so Claude
    // Desktop respawns us against the fresh binary. Investigated
    // 2026-04-30: Claude-Desktop child kept v0.4.25 logic alive across
    // a v0.4.26 update, then crashed silently on a Form tool call.
    if let Some(disk_version) = housekeeping::disk_version_if_stale() {
        eprintln!(
            "[aiui] mcp-stdio: in-memory binary v{} != on-disk v{}; \
             exiting so Claude Desktop respawns the fresh build.",
            env!("CARGO_PKG_VERSION"),
            disk_version
        );
        // No `logging::trace` here — the trace path itself might have
        // been moved by the new bundle. eprintln lands in
        // `~/Library/Logs/Claude/mcp-server-aiui.log` exactly where the
        // user is most likely to look.
        std::process::exit(0);
    }

    // Leak cleanup (v0.4.42, rescoped v0.4.46 / Bug A): reap any
    // *orphaned* `aiui --mcp-stdio` children — ones whose MCP-client
    // parent (Claude.app / Cowork wrapper) has died, leaving them
    // reparented to launchd. The earlier version reaped any older
    // mcp-stdio sharing our Claude.app *grandparent*; that tore down
    // live *parallel Cowork sessions* (they all share the one Claude.app
    // grandparent) — the "Server disconnected on first MCP call" reports.
    // Orphan-status is the right discriminator: a leak has lost its
    // parent, a live session has not. The duplicate-child glitch the old
    // rule targeted is already handled by stdin-EOF. Safe no-op when no
    // orphans exist; never touches live siblings.
    let _ = housekeeping::kill_orphaned_mcp_stdio_children();

    let cfg = Arc::new(config::AppConfig::load_or_init().expect("config init"));
    logging::trace(&format!(
        "mcp-stdio: entering run loop, token_path={}",
        cfg.token_path.display()
    ));

    let rt = tokio::runtime::Runtime::new().expect("tokio rt");
    rt.block_on(async move {
        let sock = lifetime::socket_path(&cfg.config_dir);
        // Keep the lifetime socket alive in parallel with stdio.
        tokio::spawn(lifetime::mcp_attach(sock));
        // Periodic stale-binary self-check (v0.4.43, Codex review P1a):
        // every 30 s we re-run disk_version_if_stale. If the on-disk
        // bundle has been replaced (in-app update, manual DMG drop)
        // since we started, we exit so Claude Desktop respawns us
        // against the fresh binary. Without this, a child spawned
        // before the update keeps running its old in-RAM code
        // indefinitely — the exact failure mode that the 2026-05-23
        // 0.4.40-children-survive-update cascade was driven by.
        tokio::spawn(async {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                if let Some(disk_version) = housekeeping::disk_version_if_stale() {
                    eprintln!(
                        "[aiui] mcp-stdio: periodic self-check: in-memory v{} \
                         != on-disk v{}; exiting so Claude Desktop respawns \
                         the fresh build.",
                        env!("CARGO_PKG_VERSION"),
                        disk_version
                    );
                    logging::trace(&format!(
                        "mcp-stdio: periodic self-check fired — disk v{} \
                         differs from in-memory v{}; exiting (clean)",
                        disk_version,
                        env!("CARGO_PKG_VERSION")
                    ));
                    std::process::exit(0);
                }
            }
        });
        mcp::run_stdio(cfg).await;
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let cfg = Arc::new(config::AppConfig::load_or_init().expect("config init"));

    // Process-lifetime advisory lock (v0.4.43). Held from the very
    // first line of run() until the process dies. Two GUIs spawned in
    // the same millisecond used to race for the lifetime-socket bind
    // and the HTTP-port bind (Phase 1+2 of the 2026-05-23 cascade:
    // 10 GUI restarts in 52 s, then `multi-instance-bind-race` +
    // `http-bind-error` exits). With the lock, only the first
    // arrival makes it past this check; subsequent invocations exit
    // immediately and leave the running GUI untouched.
    //
    // Drop is bound to process death via `mem::forget` further down —
    // we don't want it released while the GUI is still alive on a
    // panic-unwind path.
    let lock_path = cfg.config_dir.join("gui.lock");
    let gui_lock = match housekeeping::ProcessLock::try_acquire(&lock_path) {
        Ok(g) => g,
        Err(e) => {
            // Another aiui-GUI is alive and holds the lock. Exit
            // immediately, traced so the post-mortem in /tmp/aiui-trace.log
            // explains the silent disappearance.
            logging::trace(&format!(
                "[aiui] exit (gui-lock-busy): another aiui-GUI holds {} ({e}); \
                 exiting without binding socket/http",
                lock_path.display()
            ));
            // No pre_exit_cleanup here: we never opened tunnels nor
            // mounted the HTTP server, so there's nothing to sweep.
            std::process::exit(0);
        }
    };
    logging::trace(&format!(
        "[aiui] gui-lock acquired: {}",
        gui_lock.path().display()
    ));

    // Pre-GUI sweep (v0.4.43, Codex review P2a): now that we hold the
    // exclusive GUI lock, kill any aiui-mcp-stdio children that started
    // *before* this GUI. They're necessarily from a previous GUI
    // generation and may carry stale in-RAM code (the 2026-05-23
    // 0.4.40-children-survive-update scenario). New GUI = new truth.
    // Race-safe because only the lock-winner reaches this line.
    let pre_gui_killed = housekeeping::kill_mcp_stdio_started_before_self();
    if pre_gui_killed > 0 {
        logging::trace(&format!(
            "[aiui] startup: killed {pre_gui_killed} pre-GUI mcp-stdio child(ren); \
             Claude Desktop will respawn them against the current binary"
        ));
    }

    let dialog_state = Arc::new(dialog::DialogState::new());
    let ui_acks = Arc::new(ack::AckRegistry::new());
    let lifetime_stats = Arc::new(lifetime::LifetimeStats::new());
    let tunnel_mgr = tunnel::TunnelManager::new(cfg.http_port);
    // Shared cell that records a fatal HTTP-server bind/serve failure (e.g.
    // port 7777 held by another process). Read by the `status` command and
    // surfaced as a banner in the Settings UI — without it, a stale
    // squatter would cause every render/health/version request to fail
    // later while the window kept *looking* alive.
    let http_error: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));

    // Pending-update state (v0.4.44). Set by the silent updater path
    // in `updater.ts` whenever the periodic auto-check finds a newer
    // release; read by Settings.svelte to render a non-modal banner
    // ("Update auf v0.4.X verfügbar — Installieren"). Replaces the
    // 0.4.43 transparent-silent-install with a notification-first
    // model: user sees the banner the next time they open Settings,
    // clicks Install to surface the modal confirmation. No more
    // mid-dialog interruptions, no more silent installs the user
    // never knows about.
    //
    // Wrapped in a newtype so Tauri's `State<T>` resolution can tell
    // it apart from `http_error` (same underlying type).
    let pending_update = Arc::new(PendingUpdate::default());

    // Window-ready handshake: the dialog window's frontend signals
    // here (via the `dialog_window_ready` Tauri command) once its
    // listeners are wired up. The render path *waits* on this watch
    // before emitting `dialog:show`, so a freshly-built dialog window
    // never receives an event before its listener is registered. The
    // 0.4.30 fix — without it, a 500 ms ack timeout could fire before
    // the WebView even finished mounting Svelte (especially on the
    // very first render of a session, when the window is built fresh
    // and Vite has to load the bundle).
    let (dialog_ready_tx, _dialog_ready_rx) = tokio::sync::watch::channel(false);
    let dialog_ready_tx = Arc::new(dialog_ready_tx);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio rt");

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // `--auto` means the second invocation came from mcp_attach's
            // auto-resurrect path (`open -a aiui --args --auto`). The GUI
            // is already alive — do nothing, particularly do NOT surface
            // the Settings window. Without this guard, a stuck mcp_attach
            // retry loop (500 ms cadence) pops Settings every half-second
            // until the user force-quits Claude Desktop. Issue #71.
            if args.iter().any(|a| a == "--auto") {
                return;
            }
            show_settings_window(app);
        }))
        .plugin(
            // Persistent TRACE logging for aiui's own modules so a hung
            // dialog leaves a forensic trail. Bumped from Info → Trace
            // for `aiui_lib::*` only; dependencies (tauri, hyper, …)
            // stay at Info to keep the volume manageable. Log rotates
            // at 5 MB, one previous file kept — covers a multi-hour
            // session at TRACE without filling up disk.
            //
            // Investigated 2026-04-29: a 4-minute MCP timeout on a
            // trivial form spec was unrecoverable from logs because
            // the entire render pipeline (`render: …` traces in
            // http.rs / mcp.rs / dialog.rs) only emits at Trace level.
            tauri_plugin_log::Builder::default()
                .level(log::LevelFilter::Info)
                .level_for("aiui_lib", log::LevelFilter::Trace)
                .max_file_size(5_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("aiui".into()),
                    }),
                ])
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .manage(cfg.clone())
        .manage(dialog_state.clone())
        .manage(ui_acks.clone())
        .manage(lifetime_stats.clone())
        .manage(tunnel_mgr.clone())
        .manage(http_error.clone())
        .manage(pending_update.clone())
        .manage(dialog_ready_tx.clone())
        .invoke_handler(tauri::generate_handler![
            dialog_submit,
            dialog_cancel,
            dialog_received,
            ui_pong,
            dialog_window_ready,
            close_window,
            surface_for_dialog,
            is_update_safe_to_install,
            set_pending_update,
            clear_pending_update,
            status,
            add_remote,
            remove_remote,
            resync_remote,
            reinstall_skill,
            repair_skill,
            restart_claude_desktop,
            uninstall_all,
            quit_app,
            dismiss_welcome,
            open_url
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();
            let cfg_http = cfg.clone();
            let cfg_lt = cfg.clone();
            let ds_http = dialog_state.clone();
            let ui_acks_http = ui_acks.clone();
            let lifetime_http = lifetime_stats.clone();
            let lifetime_lt = lifetime_stats.clone();
            let app_handle_http = app_handle.clone();
            let app_handle_lt = app_handle.clone();

            logging::trace(&format!(
                "gui: setup entering, config_dir={}",
                cfg.config_dir.display()
            ));

            // Auto-patch Claude Desktop config — idempotent, GUI mode only.
            let bin = setup::app_binary_path();
            if !setup::is_claude_config_current(&bin) {
                let _ = setup::patch_claude_desktop_config(&bin);
            }

            // Kill any `aiui --mcp-stdio` children left over from an older app
            // version. Without this, a user who drops a new aiui.app over an
            // old one would still have the old MCP-stdio children running
            // under Claude Desktop — which may lack the auto-resurrect loop
            // and won't reconnect to the new GUI. SIGTERMing them forces
            // Claude Desktop to respawn against the freshly patched config.
            let killed = housekeeping::kill_stale_mcp_stdio_children(&bin);
            if killed > 0 {
                logging::trace(&format!(
                    "gui: sent SIGTERM to {killed} stale mcp-stdio child(ren); Claude Desktop will respawn them"
                ));
            }

            // Auto-register aiui as a global MCP server in Claude Code
            // (~/.claude.json) and auto-migrate any legacy `uvx aiui-mcp`
            // entries from ≤ v0.2.x installs to the native app binary, so
            // every session sees aiui without a uv/uvx dependency.
            let _ = setup::patch_claude_code_config(&bin);

            // Auto-install the aiui skill into the local Claude Code skill
            // directory on every GUI launch. Idempotent: overwrites old copies
            // so skill updates ride with app updates.
            let _ = skill::install_locally();

            // HTTP server on localhost:7777. If bind fails the most
            // likely cause is a stale aiui already holding the port —
            // exactly the multi-instance race that produced the
            // 2026-04-29 hung-dialog incident. Rather than letting
            // this instance run as a half-zombie (no server, but a
            // window that *looks* alive), we exit hard. The other
            // instance keeps serving; tauri-plugin-single-instance
            // will surface its setup window if the user retried.
            //
            // The `http_error` cell stays for the rare case where the
            // failure is something other than EADDRINUSE — we still
            // want a banner to fire before exit, and the Settings UI
            // reads this on its first tick.
            let http_error_for_serve = http_error.clone();
            let port_for_error = cfg.http_port;
            let app_handle_for_degraded = app_handle_http.clone();
            rt.spawn(async move {
                if let Err(e) = http::serve(
                    cfg_http,
                    ds_http,
                    ui_acks_http,
                    lifetime_http,
                    app_handle_http,
                )
                .await
                {
                    // v0.4.45 (Issue #55): degraded mode instead of exit(1).
                    // Before the process-lifetime lock (0.4.43), an
                    // EADDRINUSE here meant "another aiui owns the port"
                    // and exiting was correct. Now the lock guarantees
                    // we're the only aiui — so a bind failure means a
                    // *foreign* process holds :7777. Exiting in that case
                    // released the gui-lock, mcp_attach respawned us, we
                    // failed to bind again → respawn loop. Instead we
                    // stay alive in a degraded state: record the error,
                    // surface the Settings window so the user sees *why*
                    // dialogs aren't landing, and leave the lifetime
                    // socket up. No respawn loop, no silent failure.
                    log::error!(
                        "[aiui] http server error on :{port_for_error}: {e} — entering degraded mode (foreign process owns the port)"
                    );
                    logging::trace(&format!(
                        "[aiui] http-bind-error on :{port_for_error}: {e} — degraded mode, surfacing settings banner"
                    ));
                    if let Ok(mut slot) = http_error_for_serve.lock() {
                        *slot = Some(format!(
                            "Konnte localhost:{port_for_error} nicht öffnen — \
                             Port von einem anderen Prozess belegt. Schließe den \
                             Prozess (lsof -i :{port_for_error}) und starte aiui neu. {e}"
                        ));
                    }
                    // Surface the Settings window so the http_error banner
                    // becomes visible. Done on the main thread (Tauri
                    // window ops are main-thread-only).
                    let app_for_banner = app_handle_for_degraded.clone();
                    let _ = app_handle_for_degraded.run_on_main_thread(move || {
                        show_settings_window(&app_for_banner);
                    });
                }
            });

            // Startup orphan sweep: any `ssh -NTR <port>:localhost:<port>`
            // process that's been re-parented to launchd (ppid=1) is a
            // tunnel from a previously-crashed aiui that exited via
            // `app.exit()` / `process::exit()` and skipped Drop. Left
            // alive, it holds the remote-side port and forces the new
            // GUI into shared-forward mode forever — the v0.4.36 loop
            // root cause. Sweep before binding our own tunnels. v0.4.37.
            {
                let port = cfg.http_port;
                let killed = housekeeping::kill_aiui_ssh_ntr(port, true);
                logging::trace(&format!(
                    "[aiui] startup: swept {killed} orphan ssh-NTR tunnel(s) on :{port}"
                ));
            }

            // Lifetime socket — couples GUI lifetime to MCP-stdio children.
            // Counter is shared with `/health` via `LifetimeStats`.
            let lifetime_port = cfg.http_port;
            rt.spawn(async move {
                let sock = lifetime::socket_path(&cfg_lt.config_dir);
                lifetime::gui_serve(sock, app_handle_lt, lifetime_lt.conns.clone(), lifetime_port).await;
            });

            // Auto-start reverse tunnels for every registered remote.
            // Also: legacy-cleanup — strip any RemoteForward lines that
            // previous versions patched into ~/.ssh/config, so they don't
            // compete with our own tunnel manager.
            let tm_for_start = tunnel_mgr.clone();
            let port_for_start = cfg.http_port;
            rt.spawn(async move {
                for host in setup::load_remotes() {
                    let _ = setup::remove_ssh_forward(&host, port_for_start);
                    tm_for_start.ensure(host).await;
                }
            });

            // Re-sync the aiui-mcp version pin in `~/.claude.json` on
            // every registered remote. Without this a remote can drift
            // arbitrarily far behind the local companion — uvx caches
            // the once-installed version of `aiui-mcp` indefinitely
            // unless we pin it. The 2026-04-30 incident: a v0.4.27
            // companion talking to a v0.3.1 mcp-stdio on macmini
            // because the pin was missing.
            //
            // We deliberately spawn this as a background task with a
            // small per-host stagger: setup() returns straight to the
            // UI without waiting on SSH round-trips. If the pin is
            // already correct (steady state), the script reads it and
            // exits without writing — the SSH cost is a single login +
            // one Python invocation. When the pin needs updating, we
            // also pkill any in-flight child so the next tool call
            // respawns clean against the new version.
            rt.spawn(async move {
                let our_version = env!("CARGO_PKG_VERSION");
                for host in setup::load_remotes() {
                    let host_for_task = host.clone();
                    let our_version_owned = our_version.to_string();
                    // Each remote in its own blocking task — the
                    // SSH/Python pipeline is sync. Ordering across
                    // hosts is irrelevant; pin-syncs are independent.
                    tokio::task::spawn_blocking(move || {
                        let (step, patch) = setup::patch_claude_code_config_remote(
                            &host_for_task,
                            None,
                            &our_version_owned,
                        );
                        if step.ok {
                            logging::trace(&format!(
                                "remote-pin: {host_for_task}: {} ({})",
                                step.message,
                                match patch {
                                    Some(setup::RemoteConfigPatch::Patched) => "patched",
                                    Some(setup::RemoteConfigPatch::AlreadyCurrent) => "current",
                                    None => "unknown",
                                }
                            ));
                            if matches!(patch, Some(setup::RemoteConfigPatch::Patched)) {
                                let sweep = setup::kill_remote_mcp_stdio(&host_for_task);
                                logging::trace(&format!(
                                    "remote-pin: {host_for_task}: sweep {}",
                                    if sweep.ok { "ok" } else { "failed" }
                                ));
                            }
                        } else {
                            logging::trace(&format!(
                                "remote-pin: {host_for_task} sync failed: {} ({})",
                                step.message,
                                step.details.as_deref().unwrap_or("no details")
                            ));
                        }
                    });
                }
            });

            // Headless update-check (v0.4.45, Bug #7). The frontend
            // update-check in lifecycle.ts only ran while a window was
            // open — but aiui spends almost all its time headless
            // (Accessory mode, no window), so auto-updates effectively
            // never fired and users stayed weeks behind. This Rust-side
            // task runs regardless of window state: every 6 h it asks
            // the updater for the latest release and, if one is newer,
            // records it in the shared `PendingUpdate` state and
            // broadcasts `update:available`. It deliberately does NOT
            // install — the Settings banner offers a one-click install
            // so we never restart under a live dialog. `app.updater()`
            // is window-independent (it's a Manager extension making
            // plain HTTPS calls), confirmed safe headless.
            let app_handle_update = app_handle.clone();
            let pending_update_task = pending_update.clone();
            rt.spawn(async move {
                use tauri_plugin_updater::UpdaterExt;
                const CHECK_INTERVAL: std::time::Duration =
                    std::time::Duration::from_secs(6 * 60 * 60);
                // Small initial delay so the check doesn't compete with
                // the startup burst (tunnels, remote-pin, skill write).
                tokio::time::sleep(std::time::Duration::from_secs(90)).await;
                loop {
                    match app_handle_update.updater() {
                        Ok(updater) => match updater.check().await {
                            Ok(Some(update)) => {
                                let v = update.version.trim().to_string();
                                logging::trace(&format!(
                                    "update-check: {v} available (headless, deferred to banner)"
                                ));
                                if let Ok(mut slot) = pending_update_task.0.lock() {
                                    *slot = Some(v.clone());
                                }
                                let _ = app_handle_update.emit("update:available", Some(v));
                            }
                            Ok(None) => {
                                logging::trace("update-check: already on latest");
                            }
                            Err(e) => {
                                logging::trace(&format!("update-check: check failed: {e}"));
                            }
                        },
                        Err(e) => {
                            logging::trace(&format!("update-check: updater unavailable: {e}"));
                        }
                    }
                    tokio::time::sleep(CHECK_INTERVAL).await;
                }
            });

            if is_first_run(&cfg) {
                // First-ever launch: surface the settings window so the user
                // sees the welcome / pairing instructions. We deliberately
                // *don't* call `mark_first_run_done` here — that flag stays
                // true until the user explicitly dismisses the welcome
                // section in the UI (via `dismiss_welcome` command). If the
                // user closes the window without dismissing, they'll see
                // the welcome again next launch — better than missing it.
                show_settings_window(&app_handle);
            } else if !is_auto_launch() {
                // Manual launch by user (Finder double-click) → show settings,
                // Dock icon appears. Auto-launch from MCP-stdio stays silent.
                show_settings_window(&app_handle);
            } else {
                #[cfg(target_os = "macos")]
                {
                    let _ = app_handle
                        .set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            }

            std::mem::forget(rt);
            Ok(())
        })
        .on_window_event(|window, event| {
            // Multi-window lifecycle (v0.4.25, revised v0.4.36):
            //
            // The setup window and the dialog window are independent.
            // Closing one shouldn't kill the other — and definitely
            // shouldn't kill the GUI process while the lifetime
            // socket still has attached MCP-stdio children depending
            // on it.
            //
            //  • Red X on setup window: setup goes away. If no other
            //    window is visible AND no MCP-stdio children are
            //    attached, the app quits and `mcp_attach`'s
            //    auto-resurrect path brings it back on the next tool
            //    call. As long as a child is attached, we stay alive
            //    headless — the lifetime grace timer (60s after the
            //    last child detaches) is the only legitimate
            //    "nobody needs aiui anymore" signal.
            //  • Red X on dialog window: the dialog is treated as
            //    cancelled (the frontend's CloseRequested-listener
            //    fires `dialog_cancel` first; this branch runs after).
            //    NEVER quits the app, regardless of any-visible state.
            //    The dialog window is per-call ephemeral — destroyed
            //    after every submit/cancel by `close_window`. Quitting
            //    the GUI here would tear down the HTTP server while
            //    the agent's tool call is still parsing the response,
            //    producing the 8s `wait_for_aiui` timeouts the user
            //    saw on 2026-05-04 (trace 16:11:42.197 "GUI is gone"
            //    20 ms after a successful form submit).
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let app = window.app_handle();
                let closed_label = window.label().to_string();
                if closed_label == DIALOG_WINDOW_LABEL {
                    // User closed the dialog window with the native X (or
                    // ⌘W). Resolve any in-flight `/render` as cancelled
                    // right here in Rust — we no longer depend on a
                    // frontend CloseRequested handler, which in 0.4.45
                    // could `preventDefault()` and then fail to complete
                    // the close, stranding an empty, unclosable window
                    // (Bug B, the 2026-05-29 overnight report). We do NOT
                    // prevent the close: the window is allowed to go away.
                    // The awaiting `/render` will run its end-of-handler
                    // `destroy_dialog_window` (a no-op by then).
                    if let Some(ds) = app.try_state::<Arc<dialog::DialogState>>() {
                        let n = ds.cancel_all("window_closed");
                        if n > 0 {
                            log::debug!(
                                "[aiui] dialog window X-closed — cancelled {n} pending dialog(s)"
                            );
                        }
                    }
                    log::debug!(
                        "[aiui] dialog window closed — staying alive for further tool calls"
                    );
                    return;
                }
                // Setup window: quit only if nothing else needs us.
                let app_for_check = app.clone();
                let _ = app.run_on_main_thread(move || {
                    let any_visible = app_for_check
                        .webview_windows()
                        .iter()
                        .any(|(label, w)| {
                            label.as_str() != closed_label
                                && w.is_visible().unwrap_or(false)
                        });
                    let attached = app_for_check
                        .try_state::<Arc<lifetime::LifetimeStats>>()
                        .map(|s| s.child_count())
                        .unwrap_or(0);
                    if !any_visible && attached == 0 {
                        log::info!(
                            "[aiui] setup window closed and no MCP-stdio children attached — quitting; auto-resurrect will bring us back on next tool call"
                        );
                        let port = app_for_check
                            .try_state::<Arc<config::AppConfig>>()
                            .map(|c| c.http_port)
                            .unwrap_or(7777);
                        housekeeping::pre_exit_cleanup(port, "setup-close-no-children");
                        app_for_check.exit(0);
                    } else {
                        log::debug!(
                            "[aiui] setup window closed, staying alive (visible_others={any_visible}, attached_children={attached})"
                        );
                    }
                });
            }
        })
        .build(tauri::generate_context!())
        .expect("error building tauri application")
        .run(|app, event| {
            // ExitRequested handler (v0.4.43 introduced cleanup; v0.4.44
            // adds the veto for the "headless mode" case). Tauri fires
            // ExitRequested on Cmd-Q, on ⌘W of the last visible
            // window, on OS shutdown, and on `.restart()`. The
            // last-window-close case is the dangerous one: as soon as
            // the agent's dialog window closes after a submit, Tauri
            // wants to terminate the process — but that's wrong while
            // the GUI is meant to live headless serving the lifetime
            // socket. v0.4.42 lost the GUI ~18 ms after every Dialog
            // submit through this path (trace 2026-05-26 17:00:28.181
            // → 17:00:28.199); the dialog-window-close branch of
            // on_window_event already returned without exit, but
            // Tauri's default ExitRequested handler ran *after* it
            // and killed the process anyway.
            //
            // Resolution rule:
            //   • Anyone still depending on us — an attached
            //     mcp-stdio child or a pending dialog — ⇒ veto the
            //     exit via `api.prevent_exit()`. The lifetime-grace
            //     timer (60 s after the last child detaches) remains
            //     the *only* legitimate "everyone's gone, really
            //     exit" signal in normal operation.
            //   • Nobody attached and no pending dialog ⇒ honour the
            //     exit, but run pre_exit_cleanup first so any ssh-NTR
            //     tunnel children get SIGTERM instead of becoming
            //     launchd orphans.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = &event {
                let attached = app
                    .try_state::<Arc<lifetime::LifetimeStats>>()
                    .map(|s| s.child_count())
                    .unwrap_or(0);
                let pending_dialogs = app
                    .try_state::<Arc<dialog::DialogState>>()
                    .map(|s| s.stats().orphan_count)
                    .unwrap_or(0);

                if code.is_none() && (attached > 0 || pending_dialogs > 0) {
                    // Tauri-initiated quit (no explicit exit code).
                    // Someone still needs us — keep the process alive.
                    logging::trace(&format!(
                        "[aiui] veto tauri-exit-requested: attached_children={attached}, \
                         pending_dialogs={pending_dialogs}"
                    ));
                    api.prevent_exit();
                    return;
                }

                let port = app
                    .try_state::<Arc<config::AppConfig>>()
                    .map(|cfg| cfg.http_port)
                    .unwrap_or(7777);
                let reason = if code.is_some() {
                    "tauri-exit-requested-explicit"
                } else {
                    "tauri-exit-requested-no-attached"
                };
                housekeeping::pre_exit_cleanup(port, reason);
            }

            // macOS: Dock-Klick, "open" bei laufender App, File-Assoc etc.
            // → Settings-Fenster nach vorn holen. `RunEvent::Reopen` is
            // a Mac-only variant, so this whole branch is gated.
            //
            // Windows has no analogous "reopen" semantics — clicking the
            // installed `.exe` while it's already running is handled by
            // tauri-plugin-single-instance, which surfaces the existing
            // window through its own callback (wired up at plugin init,
            // not here).
            #[cfg(target_os = "macos")]
            {
                if let tauri::RunEvent::Reopen { .. } = event {
                    // Self-heal: if a stranded empty dialog window is
                    // still up with no pending dialog, sweep it before
                    // surfacing settings — this is exactly when the user
                    // would otherwise be greeted by a leftover empty
                    // frame (v0.4.46, Bug B+).
                    sweep_orphan_dialog_window(app);
                    show_settings_window(app);
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = event;
                let _ = app;
            }
        });
}
