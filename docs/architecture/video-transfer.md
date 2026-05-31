# Video transfer (media cache)

Status: implemented in v0.7.0.

## Problem

The gallery/form widgets can show video. Three constraints collide:

1. **data: doesn't scale.** Inlining a clip as `data:video/...;base64` is
   ~33 % larger than the file and lands in the render spec — it chokes the
   `get_dialog_spec` IPC and pins that much memory in the dialog registry.
   The image resolvers cap inlining at 10 MB precisely to avoid this.
2. **The Mac can't read a remote file.** For an SSH-tunneled session the
   agent (and its files) live on the remote. The companion runs on the Mac.
3. **There is no Mac → remote channel.** aiui owns a single SSH **reverse**
   tunnel, `ssh -N -T -R 7777:localhost:7777`, established Mac-side. Claude
   Desktop provides no forward (verified empirically, 2026-05-30). So the
   Mac cannot `scp`/pull from the remote — the only path is remote → Mac.

## Design: push over the existing :7777 channel

The bridge runs on whichever host holds the file. It **pushes** the bytes to
the companion and gets back a playback URL:

```
bridge ──POST /media (bytes, ?ext=mp4, bearer)──▶ companion
                                                  stores <uuid>.mp4 in cache
       ◀── { url: http://127.0.0.1:7777/media/blob/<uuid>.mp4, ttl_secs } ──
```

Playback: the dialog WebView loads that URL. `GET /media/blob/<file>` is
served by `tower_http::services::ServeDir` (HTTP range support → video
seeking), **unauthenticated** — the filename is a v4 UUID, an unguessable
capability, and the server binds loopback (+ the user's own reverse tunnel).
Uploads require the bearer token like every other mutating endpoint.

### Why the same URL works on both ends

The reverse tunnel maps `remote:7777 → mac:7777`. The companion's own HTTP
server *is* `mac:7777`. So `http://127.0.0.1:7777/media/blob/<id>` resolves
to the companion from the remote (where the bridge POSTed it) **and** from
the Mac (where the WebView plays it). No host rewriting, no per-side URL.

### Where it runs

- **Local Mac session** → the bundled Rust bridge (`aiui --mcp-stdio`) reads
  the file and POSTs over loopback. Ships in the app; works as soon as the
  user updates.
- **Remote session** → the Python bridge (`uvx aiui-mcp`) reads the remote
  file and POSTs over the tunnel. Requires the PyPI release to be promoted
  (the validate-first pre-release flow does not publish to PyPI).

Both detect a local video by extension (`.mp4/.mov/.m4v/.webm`), upload
*before* the image inliner runs (so it never base64s a video), and swap the
`src`/`thumbnail` for the returned URL. `http(s)://` video URLs are left
alone and streamed directly by the WebView (CSP `media-src` allows `https:`).

## Cache lifecycle

`companion/src-tauri/src/media.rs`. Cache dir: `<app-cache-dir>/media`.

- **Per-file TTL** = 2 h (matches the dialog TTL — a clip is only needed
  while its dialog is open).
- **Total-size cap** = 1 GiB, oldest-first eviction.
- **Per-upload cap** = 512 MiB (enforced at the HTTP body limit + handler).
- Swept on every upload and once at startup. The cache is disposable: a
  missing file renders as a broken `<video>`, never a crash, so eviction is
  best-effort and never blocks a render.

## CSP

`media-src` was absent (fell back to `default-src 'self'`, which blocked the
loopback origin). Added:

```
media-src 'self' data: blob: http://127.0.0.1:* http://localhost:* https:
connect-src … http://127.0.0.1:* http://localhost:*
```

`127.0.0.1` is a potentially-trustworthy origin (W3C secure-contexts), so
WebKit permits the loopback http media subresource from the app's secure
origin without a mixed-content block.

## Failure modes (all non-fatal)

| Condition | Result |
|---|---|
| File unreadable on the bridge host | path left as-is; logged; broken player |
| Upload > 512 MiB | companion returns 413; path left as-is |
| Old companion without `/media` (404) | path left as-is; falls back to inline (fails >10 MB) |
| Cache file evicted before playback | broken `<video>`; re-render to re-push |
