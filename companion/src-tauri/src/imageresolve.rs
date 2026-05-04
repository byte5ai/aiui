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
use std::path::{Path, PathBuf};
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

/// Resolve local filesystem paths in `src` / `thumbnail` properties to
/// `data:` URLs.
///
/// This is the *bridge-side* counterpart to [`resolve_image_srcs`].
/// Where [`resolve_image_srcs`] runs at the HTTP server (the Mac) and
/// fetches `http(s)://` URLs, this one runs at the MCP bridge (the host
/// the agent is talking to — Mac for local Claude Code, the remote host
/// for SSH-tunneled remotes). That's the only place the agent's
/// filesystem actually exists.
///
/// Accepted inputs:
/// - absolute path: `/Users/me/foo.png`
/// - tilde-prefixed path: `~/Pictures/foo.png` — expanded to `$HOME`
///
/// Rejected inputs (left untouched, the server-side resolver gets them):
/// - `data:` URLs — already inline
/// - `http://` / `https://` — handled by [`resolve_image_srcs`]
/// - relative paths (`./foo.png`, `foo.png`) — `cwd` is not a stable
///   contract on MCP bridges, especially when launched via `uvx` or as
///   a Tauri subprocess. Demanding absolute paths makes failure mode
///   loud rather than silent-but-wrong.
///
/// Fail-soft like [`resolve_image_srcs`]: read errors and oversize
/// files are logged, the original `src` is left in place (the WebView
/// will eventually show a broken image — not an aiui crash).
pub fn resolve_local_paths(spec: &mut Value) {
    walk_mut(spec, &mut |key, value| {
        if !SRC_KEYS.contains(&key) {
            return;
        }
        let Some(s) = value.as_str() else { return };
        if !looks_like_local_path(s) {
            return;
        }
        match read_path_as_data_url(s) {
            Ok(data_url) => {
                *value = Value::String(data_url);
            }
            Err(e) => {
                eprintln!("imageresolve: local path failed for {s}: {e}");
            }
        }
    });
}

fn looks_like_local_path(s: &str) -> bool {
    if s.starts_with("data:") || s.starts_with("http://") || s.starts_with("https://") {
        return false;
    }
    if s.starts_with('/') || s.starts_with('~') {
        return true;
    }
    // Windows drive-letter absolute paths: `C:\foo`, `D:/bar`, including
    // long-path prefixes `\\?\C:\…` and UNC `\\server\share\…`. Strict
    // enough to avoid catching strings like `data:image/...` which never
    // reach here anyway because of the early `data:` exit above.
    if cfg!(windows) {
        let bytes = s.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/')
        {
            return true;
        }
        if s.starts_with(r"\\") {
            return true;
        }
    }
    false
}

fn expand_tilde(s: &str) -> Option<PathBuf> {
    if let Some(rest) = s.strip_prefix("~/") {
        let home = dirs::home_dir()?;
        Some(home.join(rest))
    } else if s == "~" {
        dirs::home_dir()
    } else {
        Some(PathBuf::from(s))
    }
}

fn read_path_as_data_url(raw: &str) -> Result<String, String> {
    let path = expand_tilde(raw).ok_or_else(|| "no $HOME for ~ expansion".to_string())?;
    let metadata =
        std::fs::metadata(&path).map_err(|e| format!("stat {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("not a file: {}", path.display()));
    }
    if metadata.len() as usize > MAX_IMAGE_BYTES {
        return Err(format!(
            "too large: {} bytes (max {MAX_IMAGE_BYTES})",
            metadata.len()
        ));
    }
    let bytes =
        std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mime = guess_mime_from_extension(&path);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{b64}"))
}

fn guess_mime_from_extension(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/x-icon",
        Some("avif") => "image/avif",
        Some("heic") => "image/heic",
        // Unknown extension: hand it to the WebView as octet-stream.
        // It will likely fail to render, but that's a clear "your file
        // isn't an image" signal rather than a misleading mime guess.
        _ => "application/octet-stream",
    }
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

    #[test]
    fn looks_like_local_path_classifies_correctly() {
        assert!(looks_like_local_path("/Users/me/foo.png"));
        assert!(looks_like_local_path("~/Pictures/foo.png"));
        assert!(!looks_like_local_path("data:image/png;base64,AAAA"));
        assert!(!looks_like_local_path("https://a.test/x.png"));
        assert!(!looks_like_local_path("http://a.test/x.png"));
        assert!(!looks_like_local_path("./relative.png"));
        assert!(!looks_like_local_path("relative.png"));
        assert!(!looks_like_local_path(""));
    }

    #[test]
    fn resolve_local_paths_inlines_real_file_and_skips_others() {
        // Write a tiny PNG-ish file. Content doesn't have to be a real
        // PNG — we only assert the resolver wraps it in `data:image/png;base64,…`.
        let tmpdir = std::env::temp_dir();
        let f = tmpdir.join(format!("aiui-imageresolve-test-{}.png", std::process::id()));
        std::fs::write(&f, b"\x89PNG\r\n\x1a\nfake bytes").unwrap();
        let path_str = f.to_string_lossy().to_string();

        let mut spec = json!({
            "fields": [
                {"kind": "image", "src": path_str},
                {"kind": "image", "src": "https://leave.me/alone.png"},
                {"kind": "image", "src": "data:image/png;base64,UNCHANGED"},
                {"kind": "list", "items": [
                    {"label": "L", "value": "l", "thumbnail": path_str}
                ]}
            ]
        });
        resolve_local_paths(&mut spec);

        let s = serde_json::to_string(&spec).unwrap();
        // The local path got rewritten — original string should be gone
        // from both the image src and the list-item thumbnail.
        assert!(
            !s.contains(&path_str),
            "path string survived in spec: {s}"
        );
        // It got rewritten to a data: URL with image/png mime.
        assert!(
            s.matches("data:image/png;base64,").count() >= 2,
            "expected ≥ 2 data: URLs (image + thumbnail): {s}"
        );
        // HTTPS URL was left untouched (server-side resolver's job).
        assert!(s.contains("https://leave.me/alone.png"));
        // Pre-existing data: URL untouched.
        assert!(s.contains("data:image/png;base64,UNCHANGED"));

        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn resolve_local_paths_walks_confirm_image_and_ask_thumbnail() {
        // confirm.image.src and ask.options[].thumbnail are new image
        // slots in 0.4.23. The resolver is shape-agnostic — it walks
        // any `src`/`thumbnail` key regardless of which tool spec it
        // sits under — but pin that down with a test so a future
        // refactor can't accidentally narrow it.
        let tmpdir = std::env::temp_dir();
        let f = tmpdir.join(format!("aiui-confirm-ask-test-{}.png", std::process::id()));
        std::fs::write(&f, b"\x89PNG\r\n\x1a\nfake bytes").unwrap();
        let path_str = f.to_string_lossy().to_string();

        let mut spec = json!({
            "kind": "confirm",
            "title": "OK?",
            "image": {"src": path_str.clone()}
        });
        resolve_local_paths(&mut spec);
        assert!(spec["image"]["src"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));

        let mut spec = json!({
            "kind": "ask",
            "question": "Which?",
            "options": [
                {"label": "A", "thumbnail": path_str.clone()},
                {"label": "B", "thumbnail": "https://leave.me/b.png"},
                {"label": "C"},
            ]
        });
        resolve_local_paths(&mut spec);
        assert!(spec["options"][0]["thumbnail"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert_eq!(
            spec["options"][1]["thumbnail"].as_str(),
            Some("https://leave.me/b.png")
        );
        assert!(spec["options"][2].get("thumbnail").is_none());

        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn resolve_local_paths_fails_soft_on_missing_file() {
        let original = "/this/path/should/not/exist/aiui-test-missing.png";
        let mut spec = json!({"src": original});
        // Should not panic; should leave the value as-is.
        resolve_local_paths(&mut spec);
        assert_eq!(spec["src"].as_str(), Some(original));
    }

    #[test]
    fn guess_mime_handles_common_extensions() {
        assert_eq!(guess_mime_from_extension(Path::new("a.png")), "image/png");
        assert_eq!(guess_mime_from_extension(Path::new("a.JPG")), "image/jpeg");
        assert_eq!(guess_mime_from_extension(Path::new("a.svg")), "image/svg+xml");
        assert_eq!(
            guess_mime_from_extension(Path::new("a.unknown")),
            "application/octet-stream"
        );
        assert_eq!(
            guess_mime_from_extension(Path::new("noext")),
            "application/octet-stream"
        );
    }
}
