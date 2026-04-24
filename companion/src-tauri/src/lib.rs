mod config;
mod dialog;
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
    claude_config_ok: bool,
    remotes: Vec<String>,
    tunnels: std::collections::HashMap<String, tunnel::TunnelStatus>,
    build_info: &'static str,
}

#[tauri::command]
async fn status(
    cfg: tauri::State<'_, Arc<config::AppConfig>>,
    tm: tauri::State<'_, Arc<tunnel::TunnelManager>>,
) -> Result<StatusReport, String> {
    let bin = setup::app_binary_path();
    Ok(StatusReport {
        app_binary_path: bin.clone(),
        token_path: cfg.token_path.display().to_string(),
        http_port: cfg.http_port,
        claude_config_ok: setup::is_claude_config_current(&bin),
        remotes: setup::load_remotes(),
        tunnels: tm.snapshot().await,
        build_info: logging::BUILD_INFO,
    })
}

#[tauri::command]
async fn add_remote(
    host_alias: String,
    cfg: tauri::State<'_, Arc<config::AppConfig>>,
    tm: tauri::State<'_, Arc<tunnel::TunnelManager>>,
) -> Result<Vec<setup::StepResult>, String> {
    let mut results = Vec::new();

    // Legacy cleanup: earlier versions (≤ v0.1.1) patched the user's
    // ~/.ssh/config with a RemoteForward line. aiui now owns the tunnel
    // entirely via its own `ssh -NTR` subprocess; strip any leftover lines
    // from past installs so we don't fight them over port 7777.
    let _ = setup::remove_ssh_forward(&host_alias, cfg.http_port);

    let token_path = cfg.token_path.display().to_string();
    results.push(setup::push_token_to_remote(&host_alias, &token_path));
    results.push(skill::install_to_remote(&host_alias));
    results.push(setup::patch_claude_code_config_remote(&host_alias));

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
    let tunnel_mgr = tunnel::TunnelManager::new(cfg.http_port);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio rt");

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
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
        .manage(tunnel_mgr.clone())
        .invoke_handler(tauri::generate_handler![
            dialog_submit,
            dialog_cancel,
            close_window,
            surface_for_dialog,
            status,
            add_remote,
            remove_remote,
            reinstall_skill,
            uninstall_all
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();
            let cfg_http = cfg.clone();
            let cfg_lt = cfg.clone();
            let ds_http = dialog_state.clone();
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

            // Auto-register aiui as a global MCP server in Claude Code
            // (~/.claude.json), so every session sees it without a per-project
            // .mcp.json file.
            let _ = setup::patch_claude_code_config();

            // Auto-install the aiui skill into the local Claude Code skill
            // directory on every GUI launch. Idempotent: overwrites old copies
            // so skill updates ride with app updates.
            let _ = skill::install_locally();

            // HTTP server on localhost:7777.
            rt.spawn(async move {
                if let Err(e) = http::serve(cfg_http, ds_http, app_handle_http).await {
                    log::error!("[aiui] http server error: {e}");
                }
            });

            // Lifetime socket — couples GUI lifetime to MCP-stdio children.
            rt.spawn(async move {
                let sock = lifetime::socket_path(&cfg_lt.config_dir);
                lifetime::gui_serve(sock, app_handle_lt).await;
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
                show_settings_window(&app_handle);
                mark_first_run_done(&cfg);
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
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                // Dock icon came up because the user brought the window
                // forward. On close, drop back to Accessory mode so aiui
                // vanishes from the Dock/Cmd-Tab until next time.
                #[cfg(target_os = "macos")]
                {
                    let _ = window
                        .app_handle()
                        .set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error building tauri application")
        .run(|app, event| {
            match event {
                // macOS: Dock-Klick, "open" bei laufender App, File-Assoc etc.
                // → Settings-Fenster nach vorn holen.
                tauri::RunEvent::Reopen { .. } => {
                    show_settings_window(app);
                }
                // User-initiated quit (Cmd-Q, App menu → aiui beenden, dock
                // right-click → Beenden): block it, hide the window, drop to
                // Accessory. aiui must keep serving HTTP so the agent can
                // still open dialogs. The only legitimate exit path is the
                // lifetime-socket watchdog that calls std::process::exit().
                tauri::RunEvent::ExitRequested { api, .. } => {
                    api.prevent_exit();
                    if let Some(win) = app.get_webview_window("main") {
                        let _ = win.hide();
                    }
                    #[cfg(target_os = "macos")]
                    {
                        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                    }
                }
                _ => {}
            }
        });
}
