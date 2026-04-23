use crate::config::AppConfig;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Minimal MCP stdio handler. Purpose: keep the companion alive while Claude
/// Desktop is running. Exposes one introspection tool (`aiui_info`) so the
/// integration is visible in Claude Desktop's UI; future tools slot in here.
pub async fn run_stdio(cfg: Arc<AppConfig>) {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Ok(Some(line)) = reader.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            // notification, ignore
            continue;
        };
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");

        let result: Value = match method {
            "initialize" => json!({
                "protocolVersion": "2025-06-18",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "aiui-local", "version": env!("CARGO_PKG_VERSION") }
            }),
            "tools/list" => json!({
                "tools": [
                    {
                        "name": "aiui_info",
                        "description": "Returns aiui companion status and pairing info.",
                        "inputSchema": { "type": "object", "properties": {} }
                    }
                ]
            }),
            "tools/call" => {
                let name = msg
                    .get("params")
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                match name {
                    "aiui_info" => json!({
                        "content": [{
                            "type": "text",
                            "text": format!(
                                "aiui companion v{} running on localhost:{}\nToken path: {}",
                                env!("CARGO_PKG_VERSION"),
                                cfg.http_port,
                                cfg.token_path.display()
                            )
                        }]
                    }),
                    _ => json!({ "content": [{"type":"text","text":"unknown tool"}], "isError": true }),
                }
            }
            _ => {
                // unsupported method → JSON-RPC error
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": "method not found" }
                });
                let _ = stdout.write_all(format!("{err}\n").as_bytes()).await;
                let _ = stdout.flush().await;
                continue;
            }
        };

        let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let _ = stdout.write_all(format!("{response}\n").as_bytes()).await;
        let _ = stdout.flush().await;
    }
}
