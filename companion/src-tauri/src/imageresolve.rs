//! HTTP(S) → `data:` URL pre-render pass for image fields.
//!
//! ## Why this exists
//!
//! aiui's WebView ships with a strict Content-Security-Policy:
//!
//! ```text
//! img-src 'self' data: asset: http://asset.localhost
//! ```
//!
//! That's deliberate — we don't want a malicious dialog spec to
//! tracking-pixel the user, and we don't want to weaken aiui's
//! "phones never home" promise. Side-effect: an agent that drops a
//! plain `https://example.com/foo.png` into an `image` or
//! `image_grid` field will see the WebView block it silently. That's
//! a confusing failure mode — the agent has no way to know the image
//! never made it to the dialog.
//!
//! This module makes the developer-friendly path actually work:
//! before a render is emitted to the WebView, we walk the spec and
//! rewrite any `http(s)://...` value in a `src` or `thumbnail`
//! property by fetching the bytes on the Mac and re-encoding as a
//! `data:` URL. The WebView only ever sees `data:` URLs — CSP stays
//! strict, the agent gets to use plain HTTP URLs.
//!
//! ## Failure mode
//!
//! Fail-soft. If a fetch errors out (timeout, 404, oversized, network
//! down), the original URL is left in place and the WebView will show
//! a broken image. The agent will see this through the user, not via
//! a structured error — that's acceptable for v1; surfacing image
//! warnings into the tool response is a separate concern.

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine;
use serde_json::Value;

const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024; // 10 MB
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const SRC_KEYS: &[&str] = &["src", "thumbnail"];

/// Walk the spec JSON tree and replace `http(s)://...` values found in
/// `src` or `thumbnail` properties with `data:` URLs by fetching them
/// from this Mac.
///
/// Mutates `spec` in place. Logs failures via `eprintln!` (picked up by
/// the Tauri logger). Never panics on malformed specs — a non-image
/// `src` value is simply ignored.
pub async fn resolve_image_srcs(spec: &mut Value) {
    let urls = collect_external_urls(spec);
    if urls.is_empty() {
        return;
    }

    let client = match reqwest::Client::builder().timeout(FETCH_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("imageresolve: client build failed: {e}");
            return;
        }
    };

    // Fetch in parallel — each image is independent and the natural
    // unit of latency. Sequential would multiply latency by N for a
    // multi-image grid.
    let fetches = urls
        .into_iter()
        .map(|url| {
            let client = client.clone();
            async move {
                let result = fetch_as_data_url(&client, &url).await;
                (url, result)
            }
        })
        .collect::<Vec<_>>();
    let results = futures::future::join_all(fetches).await;

    let mut resolved = HashMap::<String, String>::new();
    for (url, result) in results {
        match result {
            Ok(data_url) => {
                resolved.insert(url, data_url);
            }
            Err(e) => {
                eprintln!("imageresolve: fetch failed for {url}: {e}");
            }
        }
    }

    if resolved.is_empty() {
        return;
    }
    rewrite_urls(spec, &resolved);
}

fn collect_external_urls(spec: &Value) -> Vec<String> {
    let mut out = Vec::<String>::new();
    walk(spec, &mut |key, value| {
        if !SRC_KEYS.contains(&key) {
            return;
        }
        let Some(s) = value.as_str() else { return };
        if s.starts_with("http://") || s.starts_with("https://") {
            out.push(s.to_string());
        }
    });
    out.sort();
    out.dedup();
    out
}

fn rewrite_urls(spec: &mut Value, map: &HashMap<String, String>) {
    walk_mut(spec, &mut |key, value| {
        if !SRC_KEYS.contains(&key) {
            return;
        }
        let Some(s) = value.as_str() else { return };
        if let Some(replacement) = map.get(s) {
            *value = Value::String(replacement.clone());
        }
    });
}

fn walk(value: &Value, f: &mut impl FnMut(&str, &Value)) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                f(k.as_str(), v);
                walk(v, f);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                walk(v, f);
            }
        }
        _ => {}
    }
}

fn walk_mut(value: &mut Value, f: &mut impl FnMut(&str, &mut Value)) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                f(k.as_str(), v);
                walk_mut(v, f);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                walk_mut(v, f);
            }
        }
        _ => {}
    }
}

async fn fetch_as_data_url(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?
        .error_for_status()
        .map_err(|e| format!("status: {e}"))?;

    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or("application/octet-stream")
        .trim()
        .to_string();

    // Pre-flight cap via Content-Length where available.
    if let Some(len) = resp.content_length() {
        if (len as usize) > MAX_IMAGE_BYTES {
            return Err(format!(
                "too large: {} bytes (max {})",
                len, MAX_IMAGE_BYTES
            ));
        }
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("read body: {e}"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("too large after read: {} bytes", bytes.len()));
    }

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collects_src_at_any_depth() {
        let spec = json!({
            "kind": "form",
            "fields": [
                {"kind": "image", "src": "https://a.test/1.png"},
                {"kind": "image_grid", "images": [
                    {"value": "x", "src": "http://b.test/2.png"},
                    {"value": "y", "src": "data:image/png;base64,AAAA"}
                ]},
                {"kind": "list", "items": [
                    {"label": "L", "value": "l", "thumbnail": "https://c.test/3.png"}
                ]}
            ]
        });
        let mut urls = collect_external_urls(&spec);
        urls.sort();
        assert_eq!(
            urls,
            vec![
                "http://b.test/2.png".to_string(),
                "https://a.test/1.png".to_string(),
                "https://c.test/3.png".to_string(),
            ]
        );
    }

    #[test]
    fn ignores_data_and_relative_urls() {
        let spec = json!({
            "src": "data:image/png;base64,AAAA",
            "thumbnail": "/local/thing.png",
            "elsewhere": "https://not-a-src.test/x.png"
        });
        let urls = collect_external_urls(&spec);
        assert!(urls.is_empty(), "got: {urls:?}");
    }

    #[test]
    fn rewrites_in_place() {
        let mut spec = json!({
            "fields": [
                {"src": "https://a.test/1.png"},
                {"images": [{"src": "https://a.test/1.png"}]}
            ]
        });
        let mut map = HashMap::new();
        map.insert(
            "https://a.test/1.png".to_string(),
            "data:image/png;base64,XX".to_string(),
        );
        rewrite_urls(&mut spec, &map);
        let s = serde_json::to_string(&spec).unwrap();
        assert!(!s.contains("https://a.test"), "still contains url: {s}");
        assert!(
            s.matches("data:image/png;base64,XX").count() == 2,
            "expected 2 replacements: {s}"
        );
    }
}
