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
//! ## Where the fetch may go
//!
//! Only to publicly-routable addresses. This is the one place in aiui
//! where an agent-supplied string turns into I/O originating on the
//! user's machine, and the far end of the bridge (a registered remote
//! dev host) is explicitly untrusted. Loopback, RFC1918, link-local,
//! CGNAT, ULA and friends are refused before a socket is opened, the
//! resolved addresses are pinned onto the client so a second DNS answer
//! cannot slip past the check, and redirects are not followed. See
//! [`destination_allowed`] (#201).
//!
//! ## Failure mode
//!
//! Fail-soft. If a fetch errors out (timeout, 404, oversized, refused
//! destination, network down), the original URL is left in place and
//! the WebView will show a broken image. The agent will see this
//! through the user, not via a structured error — that's acceptable for
//! v1; surfacing image warnings into the tool response is a separate
//! concern.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use futures::StreamExt;
use serde_json::Value;

const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024; // 10 MB
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const SRC_KEYS: &[&str] = &["src", "thumbnail"];
/// Most image fetches in flight at once. A spec may legitimately carry
/// dozens of URLs (an `image_grid`); an unbounded fan-out would open one
/// socket per URL and buffer one body per URL at the same time.
const MAX_CONCURRENT_FETCHES: usize = 4;

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

    // Vet every destination *before* a socket is opened, and remember the
    // addresses we vetted (#201). Bounded like the fetches themselves: a
    // 50-image grid must not fire 50 concurrent resolver calls either.
    let vetted = run_bounded(
        urls.into_iter()
            .map(|url| async move {
                let outcome = vet_destination(&url).await;
                (url, outcome)
            })
            .collect::<Vec<_>>(),
    )
    .await;

    let mut fetchable = Vec::<String>::new();
    let mut pinned = Vec::<(String, Vec<SocketAddr>)>::new();
    for (url, outcome) in vetted {
        match outcome {
            Ok(Destination::Literal) => fetchable.push(url),
            Ok(Destination::Pinned { host, addrs }) => {
                if !pinned.iter().any(|(h, _)| *h == host) {
                    pinned.push((host, addrs));
                }
                fetchable.push(url);
            }
            Err(e) => eprintln!("imageresolve: refused {url}: {e}"),
        }
    }
    if fetchable.is_empty() {
        return;
    }

    let client = match build_client(&pinned) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("imageresolve: {e}");
            return;
        }
    };

    // Fetch in parallel — each image is independent and the natural
    // unit of latency. Sequential would multiply latency by N for a
    // multi-image grid. Capped, see `MAX_CONCURRENT_FETCHES`.
    let fetches = fetchable
        .into_iter()
        .map(|url| {
            let client = client.clone();
            async move {
                let result = fetch_as_data_url(&client, &url).await;
                (url, result)
            }
        })
        .collect::<Vec<_>>();
    let results = run_bounded(fetches).await;

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

/// Run `tasks` with at most [`MAX_CONCURRENT_FETCHES`] of them in flight.
///
/// Its own function so the cap is exercised by a test instead of being an
/// integer buried in a combinator chain.
async fn run_bounded<F>(tasks: Vec<F>) -> Vec<F::Output>
where
    F: std::future::Future,
{
    futures::stream::iter(tasks)
        .buffer_unordered(MAX_CONCURRENT_FETCHES)
        .collect::<Vec<_>>()
        .await
}

/// What a vetted URL needs from the client before it may be fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Destination {
    /// The URL carries a literal IP, already checked. Nothing to pin —
    /// hyper connects to it directly without consulting a resolver.
    Literal,
    /// A hostname whose every resolved address passed the check. Pinned onto
    /// the client so the connect lands on exactly what we vetted, closing the
    /// DNS-rebinding window between check and connect.
    Pinned { host: String, addrs: Vec<SocketAddr> },
}

/// True when `ip` is a destination aiui is willing to fetch an image from:
/// a publicly-routable unicast address, and nothing else.
///
/// The companion is the LAN-side end of a channel whose far end — a
/// registered remote dev host holding the bridge token — is explicitly
/// untrusted. Without this filter any token holder can make the user's
/// machine `GET` an arbitrary router, IoT panel or metadata endpoint, and
/// time the response as a port-scan oracle (#201).
///
/// Implemented by hand on the octets rather than through `Ipv4Addr::is_global`
/// and friends: `is_global`, `is_shared`, `is_benchmarking`,
/// `Ipv6Addr::is_unique_local` and `is_unicast_link_local` are all still
/// nightly-only, and this is not worth a new dependency.
fn destination_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_allowed(v4),
        IpAddr::V6(v6) => match unwrap_mapped_v4(v6) {
            Some(v4) => ipv4_allowed(v4),
            None => ipv6_allowed(v6),
        },
    }
}

/// `::ffff:a.b.c.d` (IPv4-mapped) and `::a.b.c.d` (IPv4-compatible) reach the
/// same host as `a.b.c.d` — unwrap before judging, or `[::ffff:127.0.0.1]`
/// walks straight through the IPv6 rules.
fn unwrap_mapped_v4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    // `::1` and `::` are not IPv4-compatible addresses; let the IPv6 rules
    // reject them rather than mapping them to 0.0.0.1 / 0.0.0.0.
    if v6.is_loopback() || v6.is_unspecified() {
        return None;
    }
    let s = v6.segments();
    if s[0..5] != [0u16; 5] {
        return None;
    }
    match s[5] {
        0xffff | 0 => Some(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            (s[6] & 0xff) as u8,
            (s[7] >> 8) as u8,
            (s[7] & 0xff) as u8,
        )),
        _ => None,
    }
}

fn ipv4_allowed(ip: Ipv4Addr) -> bool {
    // 127.0.0.0/8; 10/8 + 172.16/12 + 192.168/16; 169.254/16 (the metadata
    // endpoint lives here); 224/4; 255.255.255.255; 0.0.0.0; and the three
    // documentation ranges.
    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_documentation()
    {
        return false;
    }
    let o = ip.octets();
    if o[0] == 0 {
        return false; // 0.0.0.0/8 "this network"
    }
    if o[0] == 100 && (o[1] & 0b1100_0000) == 64 {
        return false; // 100.64.0.0/10 carrier-grade NAT
    }
    if o[0] == 198 && (o[1] & 0b1111_1110) == 18 {
        return false; // 198.18.0.0/15 benchmarking
    }
    if o[0] >= 240 {
        return false; // 240.0.0.0/4 reserved
    }
    true
}

fn ipv6_allowed(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    let s = ip.segments();
    if (s[0] & 0xfe00) == 0xfc00 {
        return false; // fc00::/7 unique-local
    }
    if (s[0] & 0xffc0) == 0xfe80 {
        return false; // fe80::/10 link-local
    }
    if s[0] == 0x2001 && s[1] == 0x0db8 {
        return false; // 2001:db8::/32 documentation
    }
    true
}

/// An IP literal spelled in the URL's host position, or `None` for a name.
///
/// Deliberately reads the *parsed* host rather than the raw URL text: `Url`
/// has already normalised the exotic spellings, so `http://2130706433/` and
/// `http://127.1/` both arrive here as `127.0.0.1`. String-matching the URL
/// for `localhost` / `127.` / `10.` would miss every one of them.
fn parse_host_ip(host: &str) -> Option<IpAddr> {
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<Ipv6Addr>().ok().map(IpAddr::V6);
    }
    host.parse::<IpAddr>().ok()
}

/// Decide whether `url` may be fetched, resolving its host if needed.
///
/// `Err` means "do not fetch": the caller logs it and leaves the original
/// URL in the spec, exactly like any other fetch failure.
async fn vet_destination(url: &str) -> Result<Destination, String> {
    record_fetch_attempt();
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("bad url: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported scheme: {other}")),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "no host in url".to_string())?;
    let port = parsed.port_or_known_default().unwrap_or(80);

    // A literal IP never reaches a resolver — hyper connects to it directly —
    // so the check has to happen here as well as on resolved names.
    if let Some(ip) = parse_host_ip(host) {
        return if destination_allowed(ip) {
            Ok(Destination::Literal)
        } else {
            Err(format!("destination not publicly routable: {ip}"))
        };
    }

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("resolve {host}: {e}"))?
        .collect::<Vec<SocketAddr>>();
    if addrs.is_empty() {
        return Err(format!("resolve {host}: no addresses"));
    }
    // Every answer, not just the first: a hostname with one public and one
    // RFC1918 A record must not be fetchable at all.
    for addr in &addrs {
        if !destination_allowed(addr.ip()) {
            return Err(format!(
                "destination not publicly routable: {host} → {}",
                addr.ip()
            ));
        }
    }
    Ok(Destination::Pinned {
        host: host.to_string(),
        addrs,
    })
}

/// Build the fetch client, pinning each vetted hostname to the addresses we
/// checked so the client cannot re-resolve it to a different answer.
fn build_client(pinned: &[(String, Vec<SocketAddr>)]) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        // No redirects. Even with a filtered resolver, an allowed public host
        // can bounce us to http://169.254.169.254/, and each hop re-opens the
        // gap between the address we vetted and the one we connect to. Nobody
        // has yet named an image URL that needs a hop.
        .redirect(reqwest::redirect::Policy::none());
    for (host, addrs) in pinned {
        builder = builder.resolve_to_addrs(host, addrs);
    }
    builder
        .build()
        .map_err(|e| format!("client build failed: {e}"))
}

#[cfg(test)]
thread_local! {
    /// Destination-guard entries made on this thread. Thread-local rather
    /// than a global counter so tests running in parallel cannot see each
    /// other's fetches.
    static FETCH_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test-only: how many URLs this thread has put through the destination
/// guard. Used by the `/render` ordering test in `http.rs` to prove an
/// invalid spec never reaches the network (#201).
#[cfg(test)]
pub(crate) fn fetch_attempts() -> usize {
    FETCH_ATTEMPTS.with(|c| c.get())
}

#[cfg(test)]
fn record_fetch_attempt() {
    FETCH_ATTEMPTS.with(|c| c.set(c.get() + 1));
}

#[cfg(not(test))]
fn record_fetch_attempt() {}

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

/// Platform-parameterised core of [`looks_like_local_path`].
///
/// `windows` is a parameter rather than `cfg!(windows)` on purpose: CI
/// compiles but does not *run* the unit tests on the Windows leg
/// (`.github/workflows/ci.yml`), so a `#[cfg(windows)]` test would never
/// execute. Taking the platform as an argument means the Windows branch is
/// covered on the macOS runner, where tests do run (#201).
fn looks_like_local_path_on(s: &str, windows: bool) -> bool {
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
    if windows {
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

fn looks_like_local_path(s: &str) -> bool {
    looks_like_local_path_on(s, cfg!(windows))
}

/// Platform-parameterised core of [`expand_tilde`], with `home` injected so
/// the whole thing is a pure function of its arguments.
///
/// Accepts `~`, `~/rest` and — when `windows` — `~\rest`. A `~someone/rest`
/// path is `None`: other users' home directories are not something we can
/// expand, and handing the literal string to `stat` reports a missing file
/// for a path that was never meant to be literal.
fn expand_tilde_with(s: &str, home: &Path, windows: bool) -> Option<PathBuf> {
    if s == "~" {
        return Some(home.to_path_buf());
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    if windows {
        if let Some(rest) = s.strip_prefix(r"~\") {
            return Some(home.join(rest));
        }
    }
    if s.starts_with('~') {
        return None;
    }
    Some(PathBuf::from(s))
}

fn expand_tilde(s: &str) -> Option<PathBuf> {
    if !s.starts_with('~') {
        return Some(PathBuf::from(s));
    }
    let home = dirs::home_dir()?;
    expand_tilde_with(s, &home, cfg!(windows))
}

fn read_path_as_data_url(raw: &str) -> Result<String, String> {
    let path = expand_tilde(raw)
        .ok_or_else(|| "cannot expand ~user paths (or no home directory)".to_string())?;
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
        // Video — for the gallery widget's `<video controls>`. Small clips
        // inline as data: here; large ones exceed MAX_IMAGE_BYTES and are
        // left as-is (the scp/push transfer path handles those).
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        // Audio — for the form `audio` field's `<audio controls>` (#25). Like
        // video, local audio is routed through the /media cache before this
        // function ever sees it (see `is_local_audio_path`), so these
        // branches mainly document the contract / cover the data:-URL and
        // http(s) resolver paths, which reuse this mime table too.
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("aac") => "audio/aac",
        Some("ogg") => "audio/ogg",
        Some("flac") => "audio/flac",
        // Unknown extension: hand it to the WebView as octet-stream.
        // It will likely fail to render, but that's a clear "your file
        // isn't an image" signal rather than a misleading mime guess.
        _ => "application/octet-stream",
    }
}

/// True for a local-filesystem path that points at a video by extension.
/// Used to route video through the push-to-cache `/media` path instead of
/// the (10 MB-capped, base64-bloating) `data:` inliner. Mirrors the
/// `isVideo` extension check in `Gallery.svelte` and the Python bridge.
pub fn is_local_video_path(s: &str) -> bool {
    is_local_video_path_on(s, cfg!(windows))
}

/// Platform-parameterised core of [`is_local_video_path`] — see
/// [`looks_like_local_path_on`] for why the platform is a parameter.
fn is_local_video_path_on(s: &str, windows: bool) -> bool {
    if !looks_like_local_path_on(s, windows) {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    // Strip any query/fragment a path-ish string might carry before matching.
    let stem = lower.split(['?', '#']).next().unwrap_or(&lower);
    stem.ends_with(".mp4")
        || stem.ends_with(".mov")
        || stem.ends_with(".m4v")
        || stem.ends_with(".webm")
}

/// File extension (lowercase, no dot) of a local video path — for naming the
/// uploaded cache file. Defaults to `mp4` if somehow absent.
pub fn video_ext(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let stem = lower.split(['?', '#']).next().unwrap_or(&lower);
    stem.rsplit('.').next().filter(|e| !e.is_empty()).unwrap_or("mp4").to_string()
}

/// Collect every distinct local video path referenced in a `src`/`thumbnail`
/// slot anywhere in the spec. The bridge uploads each to the Mac's `/media`
/// endpoint, then calls [`replace_srcs`] to swap the paths for the returned
/// playback URLs — all *before* [`resolve_local_paths`] runs, so the image
/// inliner never sees (and never tries to base64 a 200 MB) video.
pub fn collect_local_video_paths(spec: &Value) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    walk(spec, &mut |key, value| {
        if !SRC_KEYS.contains(&key) {
            return;
        }
        if let Some(s) = value.as_str() {
            if is_local_video_path(s) && !found.iter().any(|f| f == s) {
                found.push(s.to_string());
            }
        }
    });
    found
}

/// True for a local-filesystem path that points at an audio file by
/// extension. Used to route audio through the push-to-cache `/media` path
/// instead of the (10 MB-capped, base64-bloating) `data:` inliner — mirrors
/// [`is_local_video_path`] above, the Python bridge's `_is_local_audio`, and
/// the analogous check that will land in the `form` widget's `audio` field.
/// Covers the common lossy/lossless formats a TTS sample, voice memo, or
/// generated sound clip is likely to arrive in (#25).
pub fn is_local_audio_path(s: &str) -> bool {
    is_local_audio_path_on(s, cfg!(windows))
}

/// Platform-parameterised core of [`is_local_audio_path`] — see
/// [`looks_like_local_path_on`] for why the platform is a parameter.
fn is_local_audio_path_on(s: &str, windows: bool) -> bool {
    if !looks_like_local_path_on(s, windows) {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    // Strip any query/fragment a path-ish string might carry before matching.
    let stem = lower.split(['?', '#']).next().unwrap_or(&lower);
    stem.ends_with(".mp3")
        || stem.ends_with(".m4a")
        || stem.ends_with(".wav")
        || stem.ends_with(".aac")
        || stem.ends_with(".ogg")
        || stem.ends_with(".flac")
}

/// File extension (lowercase, no dot) of a local audio path — for naming the
/// uploaded cache file. Defaults to `mp3` if somehow absent. Mirrors
/// [`video_ext`].
pub fn audio_ext(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let stem = lower.split(['?', '#']).next().unwrap_or(&lower);
    stem.rsplit('.')
        .next()
        .filter(|e| !e.is_empty())
        .unwrap_or("mp3")
        .to_string()
}

/// Collect every distinct local audio path referenced in a `src`/`thumbnail`
/// slot anywhere in the spec. Mirrors [`collect_local_video_paths`] — the
/// bridge uploads each to the Mac's `/media` endpoint, then calls
/// [`replace_srcs`] to swap the paths for the returned playback URLs — all
/// *before* [`resolve_local_paths`] runs, so the image inliner never tries to
/// base64 a large audio file.
pub fn collect_local_audio_paths(spec: &Value) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    walk(spec, &mut |key, value| {
        if !SRC_KEYS.contains(&key) {
            return;
        }
        if let Some(s) = value.as_str() {
            if is_local_audio_path(s) && !found.iter().any(|f| f == s) {
                found.push(s.to_string());
            }
        }
    });
    found
}

/// Replace every `src`/`thumbnail` string that appears as a key in `map`
/// with its mapped value. Used to swap uploaded local video paths for their
/// `/media/blob/...` playback URLs.
pub fn replace_srcs(spec: &mut Value, map: &std::collections::HashMap<String, String>) {
    if map.is_empty() {
        return;
    }
    walk_mut(spec, &mut |key, value| {
        if !SRC_KEYS.contains(&key) {
            return;
        }
        if let Some(s) = value.as_str() {
            if let Some(url) = map.get(s) {
                *value = Value::String(url.clone());
            }
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
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?
        .error_for_status()
        .map_err(|e| format!("status: {e}"))?;

    // The client follows no redirects (see `build_client`), so a 3xx arrives
    // here as a normal response with an empty body. Fail it rather than
    // inlining nothing and calling it an image.
    if resp.status().is_redirection() {
        return Err(format!("redirect not followed: {}", resp.status()));
    }

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

    // Stream, and hang up the moment the cap is passed. `Content-Length` is
    // absent on a chunked response, so buffering first and checking after
    // let any server grow the companion's RSS for the whole timeout window,
    // once per URL, on every render (#201).
    let mut buf = Vec::<u8>::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("read body: {e}"))? {
        if buf.len() + chunk.len() > MAX_IMAGE_BYTES {
            return Err(format!("too large: >{MAX_IMAGE_BYTES} bytes"));
        }
        buf.extend_from_slice(&chunk);
    }
    let bytes = buf;

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn is_local_video_path_classifies_correctly() {
        assert!(is_local_video_path("/Users/me/clip.mp4"));
        assert!(is_local_video_path("~/Movies/take.MOV"));
        assert!(is_local_video_path("/tmp/a.webm"));
        assert!(is_local_video_path("/tmp/a.m4v"));
        // Not local, or not video.
        assert!(!is_local_video_path("https://x.test/clip.mp4"));
        assert!(!is_local_video_path("data:video/mp4;base64,AAAA"));
        assert!(!is_local_video_path("/Users/me/photo.png"));
        assert!(!is_local_video_path("relative/clip.mp4"));
    }

    #[test]
    fn collect_and_replace_local_videos() {
        let spec = json!({
            "kind": "gallery",
            "items": [
                {"value": "a", "src": "/Users/me/one.mp4"},
                {"value": "b", "src": "https://x.test/two.mp4"},
                {"value": "c", "src": "/Users/me/pic.png"},
                {"value": "d", "thumbnail": "/Users/me/one.mp4"}
            ]
        });
        let mut found = collect_local_video_paths(&spec);
        found.sort();
        // De-duplicated: the same path in two slots appears once.
        assert_eq!(found, vec!["/Users/me/one.mp4".to_string()]);

        let mut spec = spec;
        let mut map = std::collections::HashMap::new();
        map.insert(
            "/Users/me/one.mp4".to_string(),
            "http://127.0.0.1:7777/media/blob/x.mp4".to_string(),
        );
        replace_srcs(&mut spec, &map);
        assert_eq!(
            spec["items"][0]["src"].as_str().unwrap(),
            "http://127.0.0.1:7777/media/blob/x.mp4"
        );
        assert_eq!(
            spec["items"][3]["thumbnail"].as_str().unwrap(),
            "http://127.0.0.1:7777/media/blob/x.mp4"
        );
        // Untouched: https video and the image.
        assert_eq!(spec["items"][1]["src"].as_str().unwrap(), "https://x.test/two.mp4");
        assert_eq!(spec["items"][2]["src"].as_str().unwrap(), "/Users/me/pic.png");
    }

    #[test]
    fn video_ext_extracts_lowercase_extension() {
        assert_eq!(video_ext("/a/b.MP4"), "mp4");
        assert_eq!(video_ext("~/x.webm"), "webm");
        assert_eq!(video_ext("/a/take.mov"), "mov");
    }

    #[test]
    fn is_local_audio_path_classifies_correctly() {
        assert!(is_local_audio_path("/Users/me/sample.mp3"));
        assert!(is_local_audio_path("~/Music/voice.M4A"));
        assert!(is_local_audio_path("/tmp/a.wav"));
        assert!(is_local_audio_path("/tmp/a.aac"));
        assert!(is_local_audio_path("/tmp/a.ogg"));
        assert!(is_local_audio_path("/tmp/a.flac"));
        // Not local, or not audio.
        assert!(!is_local_audio_path("https://x.test/clip.mp3"));
        assert!(!is_local_audio_path("data:audio/mpeg;base64,AAAA"));
        assert!(!is_local_audio_path("/Users/me/photo.png"));
        assert!(!is_local_audio_path("/Users/me/clip.mp4")); // video, not audio
        assert!(!is_local_audio_path("relative/clip.mp3"));
    }

    #[test]
    fn audio_ext_extracts_lowercase_extension() {
        assert_eq!(audio_ext("/a/b.MP3"), "mp3");
        assert_eq!(audio_ext("~/x.flac"), "flac");
        assert_eq!(audio_ext("/a/take.WAV"), "wav");
    }

    #[test]
    fn collect_and_replace_local_audio() {
        let spec = json!({
            "kind": "form",
            "fields": [
                {"kind": "audio", "src": "/Users/me/sample.mp3"},
                {"kind": "audio", "src": "https://x.test/two.mp3"},
                {"kind": "image", "src": "/Users/me/pic.png"},
                {"kind": "list", "items": [
                    {"label": "L", "value": "l", "thumbnail": "/Users/me/sample.mp3"}
                ]}
            ]
        });
        let mut found = collect_local_audio_paths(&spec);
        found.sort();
        // De-duplicated: the same path in two slots appears once.
        assert_eq!(found, vec!["/Users/me/sample.mp3".to_string()]);

        let mut spec = spec;
        let mut map = std::collections::HashMap::new();
        map.insert(
            "/Users/me/sample.mp3".to_string(),
            "http://127.0.0.1:7777/media/blob/x.mp3".to_string(),
        );
        replace_srcs(&mut spec, &map);
        assert_eq!(
            spec["fields"][0]["src"].as_str().unwrap(),
            "http://127.0.0.1:7777/media/blob/x.mp3"
        );
        assert_eq!(
            spec["fields"][3]["items"][0]["thumbnail"].as_str().unwrap(),
            "http://127.0.0.1:7777/media/blob/x.mp3"
        );
        // Untouched: https audio and the image.
        assert_eq!(spec["fields"][1]["src"].as_str().unwrap(), "https://x.test/two.mp3");
        assert_eq!(spec["fields"][2]["src"].as_str().unwrap(), "/Users/me/pic.png");
    }

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

        // Platform-independent on both settings.
        for windows in [true, false] {
            assert!(looks_like_local_path_on("/Users/me/foo.png", windows));
            assert!(looks_like_local_path_on("~/Pictures/foo.png", windows));
            assert!(looks_like_local_path_on(r"~\Pictures\foo.png", windows));
            assert!(!looks_like_local_path_on("data:image/png;base64,AAAA", windows));
            assert!(!looks_like_local_path_on("https://a.test/x.png", windows));
            assert!(!looks_like_local_path_on("relative.png", windows));
            assert!(!looks_like_local_path_on("", windows));
        }

        // Windows-only shapes. CI compiles but does not run the tests on the
        // Windows leg, so these have to go through the parameterised helper
        // to be exercised at all (#201).
        assert!(looks_like_local_path_on(r"C:\Users\me\x.png", true));
        assert!(looks_like_local_path_on("D:/renders/x.png", true));
        assert!(looks_like_local_path_on(r"\\?\C:\Users\me\x.png", true));
        assert!(looks_like_local_path_on(r"\\srv\share\x.png", true));
        // …and the matching counter-cases: on POSIX none of them is a path.
        assert!(!looks_like_local_path_on(r"C:\Users\me\x.png", false));
        assert!(!looks_like_local_path_on("D:/renders/x.png", false));
        assert!(!looks_like_local_path_on(r"\\?\C:\Users\me\x.png", false));
        assert!(!looks_like_local_path_on(r"\\srv\share\x.png", false));
    }

    #[test]
    fn expand_tilde_handles_windows_separator() {
        let home = Path::new("/home/me");

        // `~\…` is a home-relative path on Windows — before #201 it fell
        // through unexpanded and was stat'd against the process cwd.
        let got = expand_tilde_with(r"~\Pictures\x.png", home, true).unwrap();
        assert!(got.starts_with(home), "not under home: {}", got.display());
        assert!(got.to_string_lossy().contains("Pictures"));

        // The POSIX spelling keeps working on Windows too.
        let got = expand_tilde_with("~/Pictures/x.png", home, true).unwrap();
        assert_eq!(got, home.join("Pictures/x.png"));

        // Bare `~` is the home directory itself, on either platform.
        assert_eq!(expand_tilde_with("~", home, false).unwrap(), home);
        assert_eq!(expand_tilde_with("~", home, true).unwrap(), home);

        // `~user` is not expandable — explicit `None` rather than a literal
        // path that then fails `stat` with a misleading message.
        assert_eq!(expand_tilde_with("~alice/x.png", home, false), None);
        assert_eq!(expand_tilde_with("~alice/x.png", home, true), None);
        // `~\…` on POSIX is not a home-relative path either.
        assert_eq!(expand_tilde_with(r"~\Pictures\x.png", home, false), None);

        // Non-tilde input passes through verbatim — that is how the Windows
        // drive-letter and UNC shapes reach the filesystem.
        assert_eq!(
            expand_tilde_with(r"C:\Users\me\x.png", home, true).unwrap(),
            PathBuf::from(r"C:\Users\me\x.png")
        );
        assert_eq!(
            expand_tilde_with("/Users/me/x.png", home, false).unwrap(),
            PathBuf::from("/Users/me/x.png")
        );
    }

    #[test]
    fn windows_paths_route_video_and_audio_to_media() {
        // Under `windows = true` these are local media and must be collected
        // for the `/media` upload instead of falling through to the image
        // inliner (which would then fail to read them).
        assert!(is_local_video_path_on(r"C:\Users\me\clip.mp4", true));
        assert!(is_local_video_path_on(r"~\Movies\take.MOV", true));
        assert!(is_local_audio_path_on(r"~\Music\memo.m4a", true));
        assert!(is_local_audio_path_on(r"D:/audio/voice.wav", true));
        // On POSIX a drive letter is not a path — and must not be treated
        // as one, or garbage gets pushed at the file reader.
        assert!(!is_local_video_path_on(r"C:\Users\me\clip.mp4", false));
        assert!(!is_local_audio_path_on(r"D:/audio/voice.wav", false));
        // Still not media, on either platform.
        assert!(!is_local_video_path_on(r"C:\Users\me\photo.png", true));
        assert!(!is_local_audio_path_on(r"C:\Users\me\clip.mp4", true));
    }

    #[test]
    fn destination_predicate_rejects_non_public_ips() {
        let blocked = [
            "127.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "fd00::1",
            "fe80::1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "100.64.0.1",
            "198.18.0.1",
            // Extras the acceptance list implies: mapped private v4, the
            // documentation ranges, and reserved space.
            "::ffff:10.0.0.1",
            "192.0.2.1",
            "2001:db8::1",
            "240.0.0.1",
        ];
        for s in blocked {
            let ip: IpAddr = s.parse().unwrap();
            assert!(!destination_allowed(ip), "should be refused: {s}");
        }

        let allowed = ["93.184.216.34", "2606:2800:220:1::248:1893", "8.8.8.8"];
        for s in allowed {
            let ip: IpAddr = s.parse().unwrap();
            assert!(destination_allowed(ip), "should be allowed: {s}");
        }
    }

    #[tokio::test]
    async fn ip_literal_urls_are_checked_without_dns() {
        // The guard runs on the parsed host, not on the URL text — so the
        // decimal, dotted-short and IPv4-mapped spellings of loopback /
        // RFC1918 are all refused, and none of them needs a resolver.
        for url in [
            "http://2130706433/x.png",
            "http://127.1/x.png",
            "http://[::ffff:10.0.0.1]/x.png",
            "http://[::1]:8080/x.png",
            "http://192.168.1.1/cgi-bin/reboot",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            let err = vet_destination(url).await.unwrap_err();
            assert!(
                err.contains("not publicly routable"),
                "{url}: unexpected error {err}"
            );
        }

        // A public literal passes the guard with nothing to pin.
        assert_eq!(
            vet_destination("http://93.184.216.34/x.png").await.unwrap(),
            Destination::Literal
        );
    }

    #[tokio::test]
    async fn refused_destinations_leave_the_spec_untouched() {
        // End to end through the real entry point: a LAN URL is not fetched
        // and the original value survives, fail-soft like any other failure.
        let mut spec = json!({
            "kind": "ask",
            "question": "x",
            "options": [
                {"label": "a", "value": "a", "thumbnail": "http://192.168.1.1/cgi-bin/reboot"}
            ]
        });
        resolve_image_srcs(&mut spec).await;
        assert_eq!(
            spec["options"][0]["thumbnail"].as_str(),
            Some("http://192.168.1.1/cgi-bin/reboot")
        );
    }

    #[tokio::test]
    async fn fetch_does_not_follow_redirects() {
        // Serve a canned 302 from loopback and call the fetch layer directly,
        // below the destination guard — the guard would (correctly) refuse a
        // loopback listener, and what's under test here is the client policy.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut scratch = [0u8; 1024];
            let _ = sock.read(&mut scratch).await;
            sock.write_all(
                b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
            let _ = sock.flush().await;
        });

        let client = build_client(&[]).unwrap();
        let err = fetch_as_data_url(&client, &format!("http://{addr}/x.png"))
            .await
            .unwrap_err();
        assert!(err.contains("redirect not followed"), "got: {err}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn chunked_response_without_content_length_is_capped() {
        // No Content-Length, so the pre-flight cap cannot fire. The read has
        // to abort mid-body, not buffer the whole thing and check after.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let written = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let written_srv = written.clone();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut scratch = [0u8; 1024];
            let _ = sock.read(&mut scratch).await;
            if sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n\
                      Transfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .is_err()
            {
                return;
            }
            // 64 KiB chunks, four times the cap in total — or until the
            // client hangs up, which is the behaviour under test.
            let payload = vec![b'a'; 64 * 1024];
            let header = format!("{:x}\r\n", payload.len());
            let total = MAX_IMAGE_BYTES * 4;
            while written_srv.load(std::sync::atomic::Ordering::SeqCst) < total {
                if sock.write_all(header.as_bytes()).await.is_err()
                    || sock.write_all(&payload).await.is_err()
                    || sock.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
                written_srv.fetch_add(payload.len(), std::sync::atomic::Ordering::SeqCst);
            }
            let _ = sock.write_all(b"0\r\n\r\n").await;
        });

        let client = build_client(&[]).unwrap();
        let err = fetch_as_data_url(&client, &format!("http://{addr}/big.png"))
            .await
            .unwrap_err();
        assert!(err.contains("too large"), "got: {err}");
        server.abort();
        let _ = server.await;

        // The whole payload was never buffered: the server got to write only
        // a little past the cap before the client dropped the connection.
        let wrote = written.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            wrote < MAX_IMAGE_BYTES * 2,
            "server wrote {wrote} bytes — body was not capped while streaming"
        );
    }

    #[tokio::test]
    async fn fetches_respect_concurrency_cap() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let tasks = (0..50)
            .map(|i| {
                let in_flight = in_flight.clone();
                let peak = peak.clone();
                async move {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    // Yield so the executor has every chance to start more.
                    tokio::task::yield_now().await;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    i
                }
            })
            .collect::<Vec<_>>();

        let out = run_bounded(tasks).await;
        assert_eq!(out.len(), 50);
        let observed = peak.load(Ordering::SeqCst);
        assert!(
            observed <= MAX_CONCURRENT_FETCHES,
            "peak in-flight {observed} exceeds cap {MAX_CONCURRENT_FETCHES}"
        );
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
        assert_eq!(guess_mime_from_extension(Path::new("a.mp3")), "audio/mpeg");
        assert_eq!(guess_mime_from_extension(Path::new("a.M4A")), "audio/mp4");
        assert_eq!(guess_mime_from_extension(Path::new("a.wav")), "audio/wav");
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
