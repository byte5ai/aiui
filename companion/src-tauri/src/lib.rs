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
mod setup;
mod skill;
mod tunnel;

use std::sync::Arc;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

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
}

#[tauri::command]
async fn status(
    cfg: tauri::State<'_, Arc<config::AppConfig>>,
    tm: tauri::State<'_, Arc<tunnel::TunnelManager>>,
    http_err: tauri::State<'_, Arc<std::sync::Mutex<Option<String>>>>,
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
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    // Sanity-check: only allow http(s) so a compromised renderer can't
    // smuggle file:// or shell URIs through this command.
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("refusing non-http(s) URL: {url}"));
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("open {url}: {e}"))?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url; // silence unused on non-macos until ports land
    }
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
    app.exit(0);
    Ok(())
}

/// Quit + relaunch Claude Desktop so it re-reads `claude_desktop_config.json`
/// and picks up the freshly-patched aiui MCP server entry. This is the
/// "after-Setup nudge" the user otherwise has to figure out themselves.
///
/// Uses AppleScript for the quit (so Claude gets a chance to close cleanly,
/// not SIGKILL) and `open -a` for the relaunch. If Claude Desktop isn't
/// installed or isn't running, the quit step quietly no-ops and we just
/// launch fresh.
#[tauri::command]
async fn restart_claude_desktop() -> Result<setup::StepResult, String> {
    use std::process::Command;

    // Best-effort quit. Status of `osascript` is non-fatal — if Claude isn't
    // running, AppleScript returns an error that we treat as a no-op.
    let _ = Command::new("osascript")
        .args(["-e", "tell application \"Claude\" to quit"])
        .output();

    // Give Claude a moment to actually shut down before relaunching, so
    // `open -a` doesn't race with a still-quitting instance.
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
        details: Some(
            "Verschiebe /Applications/aiui.app in den Papierkorb, um auch die App zu entfernen."
                .into(),
        ),
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
        WebviewUrl::App("index.html".into()),
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
/// Reused across renders: if a dialog window already exists, we just
/// surface it. The frontend handles the actual content swap when the
/// `dialog:show` event arrives.
pub(crate) fn ensure_dialog_window(
    app: &tauri::AppHandle,
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
        WebviewUrl::App("index.html".into()),
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
        mcp::run_stdio(cfg).await;
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let cfg = Arc::new(config::AppConfig::load_or_init().expect("config init"));
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
        .manage(dialog_ready_tx.clone())
        .invoke_handler(tauri::generate_handler![
            dialog_submit,
            dialog_cancel,
            dialog_received,
            ui_pong,
            dialog_window_ready,
            close_window,
            surface_for_dialog,
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
            let app_handle_for_exit = app_handle_http.clone();
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
                    log::error!(
                        "[aiui] http server error on :{port_for_error}: {e} — exiting (other instance owns the port)"
                    );
                    if let Ok(mut slot) = http_error_for_serve.lock() {
                        *slot = Some(format!(
                            "Konnte localhost:{port_for_error} nicht öffnen — Port wahrscheinlich belegt. {e}"
                        ));
                    }
                    // Hop to main thread to call exit cleanly.
                    let app_for_exit = app_handle_for_exit.clone();
                    let _ = app_handle_for_exit.run_on_main_thread(move || {
                        app_for_exit.exit(1);
                    });
                }
            });

            // Lifetime socket — couples GUI lifetime to MCP-stdio children.
            // Counter is shared with `/health` via `LifetimeStats`.
            rt.spawn(async move {
                let sock = lifetime::socket_path(&cfg_lt.config_dir);
                lifetime::gui_serve(sock, app_handle_lt, lifetime_lt.conns.clone()).await;
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
            // Multi-window lifecycle (v0.4.25):
            //
            // The setup window and the dialog window are independent.
            // Closing one shouldn't kill the other — and definitely
            // shouldn't kill an in-flight dialog the agent is still
            // waiting on.
            //
            //  • Red X on setup window: setup goes away. If a dialog is
            //    visible *or* something is pending, the app stays alive.
            //    Otherwise the app quits, and `mcp_attach`'s
            //    auto-resurrect path brings it back on the next tool
            //    call. Same UX promise as before, just without the
            //    cross-window damage.
            //  • Red X on dialog window: the dialog is treated as
            //    cancelled (the frontend's CloseRequested-listener fires
            //    `dialog_cancel` first; this branch runs after).
            //    Otherwise identical to setup-window close.
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let app = window.app_handle();
                let closed_label = window.label().to_string();
                // Tauri lets the close proceed unless we set
                // api.prevent_close(); we want the close to happen.
                // Schedule the quit-check on the next tick so it runs
                // *after* this window is actually destroyed.
                let app_for_check = app.clone();
                let _ = app.run_on_main_thread(move || {
                    // Filter out the just-closed window — at this point
                    // it may still appear in the registry briefly.
                    let any_visible = app_for_check
                        .webview_windows()
                        .iter()
                        .any(|(label, w)| {
                            label.as_str() != closed_label
                                && w.is_visible().unwrap_or(false)
                        });
                    if !any_visible {
                        log::info!(
                            "[aiui] last visible window ({closed_label}) closed — quitting; auto-resurrect will bring us back on next tool call"
                        );
                        app_for_check.exit(0);
                    } else {
                        log::debug!(
                            "[aiui] {closed_label} closed, but other windows still visible — staying alive"
                        );
                    }
                });
            }
        })
        .build(tauri::generate_context!())
        .expect("error building tauri application")
        .run(|app, event| {
            // macOS: Dock-Klick, "open" bei laufender App, File-Assoc etc.
            // → Settings-Fenster nach vorn holen.
            if let tauri::RunEvent::Reopen { .. } = event {
                show_settings_window(app);
            }
            // Cmd-Q and window-close both just let the app terminate;
            // auto-resurrect in mcp_attach brings aiui back when needed.
        });
}
