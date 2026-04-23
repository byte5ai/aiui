#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|a| a == "--mcp-stdio") {
        aiui_lib::run_mcp_stdio_only();
    } else {
        aiui_lib::run();
    }
}
