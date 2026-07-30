fn main() {
    tauri_build::build();

    // --- Windows: `aiui_lib` test-harness loader crash (#141) — NOT fixed here ---
    //
    // Symptom: on windows-latest CI, `cargo test --lib --release` compiles
    // and links `aiui_lib-*.exe` cleanly, but the binary crashes *at process
    // load* with STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) before any test
    // runs — while the real `aiui.exe` (built via `tauri build`, e.g. the
    // NSIS bundle step in CI) links and runs fine.
    //
    // Leading hypothesis for *why*: this matches a long-documented, still-
    // open Tauri limitation (tauri-apps/tauri#13419, #6926, discussion
    // #11179). `tauri_build::build()` above embeds the app's Windows
    // manifest — which pulls in the Microsoft.Windows.Common-Controls v6
    // side-by-side assembly that wry/WebView2 and tauri-plugin-dialog's
    // native dialogs depend on — only into the primary `aiui` bin target,
    // with no way to reach the separate `aiui_lib-*.exe` test-harness
    // binary `cargo test --lib` links from the same crate. Without that
    // manifest, the loader resolves the harness's statically imported
    // Common-Controls-v6 entry points against the default (unredirected,
    // older) comctl32.dll, which lacks them — hence STATUS_ENTRYPOINT_NOT_FOUND
    // at load.
    //
    // A first attempt embedded the same manifest via
    // `cargo:rustc-link-arg-tests=/MANIFEST:EMBED` (mirroring Tauri's own
    // workspace workaround, `examples/api/src-tauri/build.rs`
    // `embed_manifest_for_tests()`). That shipped in an earlier revision of
    // this file and was WRONG — not a colon-syntax issue (`cargo::` vs
    // `cargo:`), but architectural: `rustc-link-arg-tests` (in either
    // syntax) only applies to actual integration-test targets under
    // `tests/`, not to `--lib` unit-test harnesses — a still-open Cargo bug
    // (rust-lang/cargo#10937, filed 2022, a docs-only patch for it closed
    // stale in 2026-07 without a real fix landing). `companion/src-tauri`
    // has no `tests/` directory (all ~148 tests are `#[cfg(test)]` unit
    // tests in `src/*.rs`), so Cargo rejects the instruction outright:
    // "invalid instruction `cargo:rustc-link-arg-tests` ... does not have a
    // test target" — which is exactly what broke CI on PR #147. The
    // unqualified `cargo:rustc-link-arg` doesn't help either: per the same
    // Cargo issue, its "tests" eligibility is gated on the same
    // `TargetKind::Test` check, so it would silently no-op for our
    // `--lib` harness (no fix) while *also* applying to the shipped `aiui`
    // bin (risking a duplicate-manifest linker conflict with the manifest
    // `tauri_build::build()` already embeds there) — worse on both counts.
    //
    // Net result: there is currently no documented Cargo build-script
    // mechanism that can scope a linker/manifest change to *only* the
    // `--lib` unit-test harness in a package like this one. Real next
    // steps (need Windows hardware, out of scope for a build.rs patch):
    //   (a) run the failing `aiui_lib-*.exe` under a dependency
    //       walker / `dumpbin /imports` on a Windows box to name the
    //       actual missing export and confirm/refute the Common-Controls-v6
    //       hypothesis above, or
    //   (b) split the Tauri/wry-dependent code out of `aiui_lib` into a
    //       separate crate so the `--lib` unit-test harness never links
    //       wry/WebView2 at all — a real architectural change, not
    //       attempted here.
    // Until one of those lands, keep the existing CI mitigation: Windows
    // runs `cargo test --lib --no-run` (compile+link only, no execution;
    // see the `run_tests` matrix flag in `.github/workflows/ci.yml`,
    // introduced in 1e2e061).

    // Build timestamp (ISO 8601, UTC)
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    println!("cargo:rustc-env=AIUI_BUILD_TIMESTAMP={now}");

    // Git SHA, shortened, "nogit" if not in a git repo
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "nogit".into());
    println!("cargo:rustc-env=AIUI_GIT_SHA={sha}");

    // Rebuild when HEAD moves
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
