mod ack;
mod config;
mod dialog;
mod fsutil;
mod housekeeping;
mod http;
mod lifetime;
mod logging;
mod mcp;
mod setup;
mod skill;
mod tunnel;

use std::sync::Arc;
use tauri::Manager;

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

#[tauri::command]
async fn close_window(window: tauri::WebviewWindow) -> Result<(), String> {
    let _ = window.hide();
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
    if let Some(win) = app.get_webview_window("main") {
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
    // Issue #H-3 in v0.4.10 review (also part of #81 Linux-devhost).
    let reach_step = setup::check_remote_aiui_mcp(&host_alias);
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

    let config_step = setup::patch_claude_code_config_remote(&host_alias);
    let config_ok = config_step.ok;
    results.push(config_step);

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
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

fn is_auto_launch() -> bool {
    std::env::args().any(|a| a == "--auto")
}

/// Runs the MCP-stdio side only: NO Tauri GUI, NO HTTP server. This process
/// is spawned by Claude Desktop and talks JSON-RPC on stdin/stdout. It also
/// attaches to the GUI process via the lifetime socket so the GUI knows we're
/// alive (and can self-terminate when we die).
pub fn run_mcp_stdio_only() {
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
            tauri_plugin_log::Builder::default()
                .level(log::LevelFilter::Info)
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
        .invoke_handler(tauri::generate_handler![
            dialog_submit,
            dialog_cancel,
            dialog_received,
            ui_pong,
            close_window,
            surface_for_dialog,
            status,
            add_remote,
            remove_remote,
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

            // HTTP server on localhost:7777. If bind fails (port held by
            // a stale aiui or unrelated process) we record the error in
            // the shared `http_error` cell so the Settings UI can surface
            // a banner — silently logging it left users staring at a
            // window that *looked* alive but answered no requests.
            let http_error_for_serve = http_error.clone();
            let port_for_error = cfg.http_port;
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
                    log::error!("[aiui] http server error: {e}");
                    if let Ok(mut slot) = http_error_for_serve.lock() {
                        *slot = Some(format!(
                            "Konnte localhost:{port_for_error} nicht öffnen — Port wahrscheinlich belegt. {e}"
                        ));
                    }
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
            // Red X quits the app outright — the MCP-stdio child's
            // lifetime-socket loop will resurrect aiui on the next agent
            // call, so there is no reason to keep a headless process
            // running once the user asks for it to go away.
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                window.app_handle().exit(0);
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
