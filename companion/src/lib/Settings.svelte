<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { _ } from "svelte-i18n";
  import { onMount, onDestroy } from "svelte";
  import { checkForUpdates } from "./updater";
  import iconUrl from "../assets/icon.png";

  type StepResult = { ok: boolean; message: string; details: string | null };
  type TunnelStatus =
    | { state: "connecting" }
    | { state: "connected" }
    | { state: "connected_shared" }
    | { state: "failed"; reason: string }
    | { state: "stopped" };
  type Status = {
    app_binary_path: string;
    token_path: string;
    http_port: number;
    claude_config_ok: boolean;
    claude_code_config_ok: boolean;
    skill_installed: boolean;
    claude_desktop_running: boolean;
    remotes: string[];
    tunnels: Record<string, TunnelStatus>;
    build_info: string;
    welcome_pending: boolean;
    http_error: string | null;
    http_alive: boolean;
    /** Why `http_alive` says what it says — `ProbeOutcome::as_str` on the
     * Rust side (`alive`, `token_unreadable`, `unreachable`,
     * `wrong_service`, `client_build_failed`). Issue #208. */
    http_probe_reason: string;
    /** The i18n key for the banner's hint line, picked in Rust by
     * `health_hint_key` from the probe reason, `http_error` and the OS.
     * Not assembled here: the mapping is the thing that was wrong (the
     * pane claimed a port squatter for all four failure modes), so it
     * lives where it can be unit-tested. Issue #208. */
    http_hint_key: string;
    /** Lower-case OS identifier from the backend. Used to pick the right
     * variant of OS-specific UI copy (e.g. uninstall instructions). */
    os: "macos" | "windows" | "linux" | "other";
    /** When the periodic auto-check found a newer release (set by
     * `set_pending_update` in updater.ts silent path). Settings shows a
     * non-modal banner with this version + an "Installieren" button.
     * Cleared once the user installs (clear_pending_update) or once
     * the on-disk version catches up. v0.4.44. */
    pending_update: string | null;
  };
  let status = $state<Status | null>(null);
  let newHost = $state("");
  let busy = $state(false);
  let log = $state<{ text: string; ok: boolean }[]>([]);
  let confirmUninstall = $state(false);
  let uninstallDone = $state(false);
  let demoCopied = $state(false);
  /** Both clipboard routes failed — the prompt is rendered inline in a
   *  read-only textarea instead, so step 3 of the wizard always has a way
   *  forward. Issue #208. */
  let demoFallbackVisible = $state(false);
  let step1Expanded = $state(false);
  /** The remote whose Remove button was clicked once. While set, that row
   *  shows "Really remove <host>? [Back] [Remove]" instead of firing
   *  `remove_remote` — which deletes the token, the MCP entry and the skill
   *  directory on the host, with no undo. Issue #208. */
  let confirmRemoveHost = $state<string | null>(null);
  /** Consecutive `http_alive: false` samples. The banner waits for
   *  {@link BANNER_MIN_CONSECUTIVE_FAILURES} of them so one slow probe
   *  (machine under load, wake from sleep) can't flash a red port-conflict
   *  claim. Reset by any success. Issue #208. */
  let failedProbes = $state(0);
  /** When add_remote fails, we keep the input populated and surface the
   *  failure inline so the user can fix and retry without scrolling.
   *  Cleared on next attempt or successful add. */
  let addRemoteError = $state<{ message: string; details: string | null } | null>(null);
  let timer: number | undefined;

  const POLL_INTERVAL_MS = 2000;
  /** Three consecutive failures ≈ 6 s of a genuinely unreachable server —
   *  short enough that a real outage still surfaces promptly, long enough
   *  that one timed-out probe doesn't. Issue #208. */
  const BANNER_MIN_CONSECUTIVE_FAILURES = 3;

  /**
   * Pull a fresh status report.
   *
   * `force` bypasses the Rust-side 15 s cache over the two expensive
   * probes (the authenticated HTTP self-probe and the `pgrep`/`tasklist`
   * spawn). The poll never forces; a user action that is supposed to
   * change one of them does. Issue #208.
   */
  async function refresh(force = false) {
    // The status report carries `http_alive` from a Rust-side TCP
    // self-probe — WebView `fetch()` would be ATS-blocked on macOS for
    // plaintext localhost, which is how v0.4.8 ended up with a permanent
    // false-positive banner. Issue #77.
    const next = await invoke<Status>("status", { force });
    failedProbes = next.http_alive ? 0 : failedProbes + 1;
    status = next;
  }

  /**
   * Start the status poll, unless it is already running or there is
   * nothing left to poll for.
   *
   * The setup window is hidden on close, never destroyed (Invariant I2 in
   * `lib.rs`), so this component stays mounted for the life of the process
   * and `onDestroy` never runs. Before #208 that meant one `setInterval`
   * survived forever: ~43k HTTP round-trips and child-process spawns a day
   * for a window nobody was looking at, against the project's own
   * "no background polling" principle
   * (`docs/architecture/self-healing.md`).
   */
  function startPolling() {
    if (timer !== undefined || uninstallDone) return;
    timer = window.setInterval(() => void refresh(), POLL_INTERVAL_MS);
  }

  function stopPolling() {
    if (timer !== undefined) {
      window.clearInterval(timer);
      timer = undefined;
    }
  }

  function onWindowHidden() {
    stopPolling();
  }

  function onWindowVisible() {
    if (uninstallDone) return;
    void refresh();
    startPolling();
  }

  function onVisibilityChange() {
    if (document.hidden) onWindowHidden();
    else onWindowVisible();
  }

  function pushLog(results: StepResult[]) {
    log = [
      ...results.map((r) => ({ text: r.message + (r.details ? ` — ${r.details}` : ""), ok: r.ok })),
      ...log,
    ].slice(0, 8);
  }

  function pushSingle(result: StepResult) {
    pushLog([result]);
  }

  async function addRemote() {
    if (!newHost.trim() || busy) return;
    busy = true;
    addRemoteError = null;
    try {
      confirmRemoveHost = null;
      const results = await invoke<StepResult[]>("add_remote", { hostAlias: newHost.trim() });
      pushLog(results);
      // Clear the input only on full success — otherwise keep what the
      // user typed so they can fix and retry. Surface the first failing
      // step inline so they don't have to hunt through the log.
      const firstFailure = results.find((r) => !r.ok);
      if (firstFailure) {
        addRemoteError = { message: firstFailure.message, details: firstFailure.details };
      } else {
        newHost = "";
      }
      await refresh(true);
    } finally {
      busy = false;
    }
  }

  /**
   * Second click on a remote's Remove button. The first click only arms
   * `confirmRemoveHost` — this command stops the tunnel, strips the
   * `RemoteForward` from `~/.ssh/config` and, when the host is reachable,
   * deletes the auth token, the `aiui` MCP entry and the skill directory
   * *on that host*. There is no undo and no UI path back short of a full
   * re-setup, and the button used to sit flush against the ⟳ icon in a
   * 520 px window. Issue #208.
   */
  async function removeRemote(host: string) {
    busy = true;
    try {
      const results = await invoke<StepResult[]>("remove_remote", { hostAlias: host });
      pushLog(results);
      await refresh(true);
    } finally {
      confirmRemoveHost = null;
      busy = false;
    }
  }

  async function resyncRemote(host: string) {
    busy = true;
    try {
      const results = await invoke<StepResult[]>("resync_remote", { hostAlias: host });
      pushLog(results);
      await refresh(true);
    } finally {
      busy = false;
    }
  }

  async function doUninstall() {
    busy = true;
    try {
      const results = await invoke<StepResult[]>("uninstall_all");
      pushLog(results);
      confirmUninstall = false;
      uninstallDone = true;
      // #208: uninstall deletes the token file and `first_run_done`. With
      // the poll still running behind the done-modal, the next tick's probe
      // failed on the token read and `welcome_pending` flipped back to
      // true — so dismissing the modal greeted the user with a red "another
      // process is holding port 7777" banner and a re-armed onboarding
      // wizard, while aiui's server was in fact still listening. Both
      // statements false, both deterministic. Nothing here is worth polling
      // for any more.
      stopPolling();
      await refresh();
    } finally {
      busy = false;
    }
  }

  async function repairSkill() {
    busy = true;
    try {
      const result = await invoke<StepResult>("repair_skill");
      pushSingle(result);
      await refresh(true);
    } finally {
      busy = false;
    }
  }

  /**
   * Copy the demo prompt, and if that is impossible, show it.
   *
   * #208: this is the only route to the prompt — the text is never
   * rendered otherwise — and its old catch block did nothing at all, so a
   * first-run user clicked the wizard's primary button twice, saw nothing
   * happen either time, and had no way to reach step 3. The Rust command
   * comes first because `navigator.clipboard` needs a secure context that
   * WKWebView does not always grant; the inline textarea is what makes the
   * failure recoverable rather than terminal.
   */
  async function copyDemoPrompt() {
    const text = $_("settings.welcome.demo.prompt");
    const failures: string[] = [];
    for (const attempt of [
      () => invoke("copy_to_clipboard", { text }),
      () => navigator.clipboard.writeText(text),
    ]) {
      try {
        await attempt();
        demoCopied = true;
        demoFallbackVisible = false;
        window.setTimeout(() => (demoCopied = false), 2000);
        return;
      } catch (e) {
        failures.push(String(e));
      }
    }
    demoCopied = false;
    demoFallbackVisible = true;
    pushSingle({
      ok: false,
      message: $_("settings.welcome.demo.failed"),
      details: failures.join(" · "),
    });
  }

  async function quitApp() {
    try {
      await invoke("quit_app");
      // app.exit(0) tears the WebView down; nothing to do here.
    } catch (e) {
      pushSingle({
        ok: false,
        message: `Quit failed: ${String(e)}`,
        details: null,
      });
    }
  }

  async function restartClaude() {
    busy = true;
    try {
      const result = await invoke<StepResult>("restart_claude_desktop");
      pushSingle(result);
      // Forced: `claude_desktop_running` is one of the cached probes, and
      // being told for another 15 s that Claude isn't running reads as the
      // button not having worked (#208).
      await refresh(true);
    } finally {
      busy = false;
    }
  }

  async function dismissWelcome() {
    try {
      await invoke("dismiss_welcome");
    } finally {
      // Either way, hide locally — server-side state will catch up on next refresh.
      if (status) status = { ...status, welcome_pending: false };
    }
  }

  async function openIssue() {
    const body = encodeURIComponent(
      `**Version:** ${status?.build_info ?? "unknown"}\n\n` +
        `**Describe the bug:**\n\n\n` +
        `**Steps to reproduce:**\n1.\n2.\n3.\n\n` +
        `**Expected / actual:**\n\n`,
    );
    // `window.open(url)` is blocked in Tauri's WebView (security). Round
    // through a Rust command that hands the URL to macOS `open`.
    await invoke("open_url", {
      url: `https://github.com/byte5ai/aiui/issues/new?body=${body}`,
    });
  }

  function statusLabel(t: TunnelStatus | undefined): { text: string; tone: "ok" | "warn" | "err" | "dim" } {
    if (!t) return { text: $_("settings.tunnel.unknown"), tone: "dim" };
    switch (t.state) {
      case "connected":
        return { text: $_("settings.tunnel.connected"), tone: "ok" };
      case "connected_shared":
        return { text: $_("settings.tunnel.connected_shared"), tone: "ok" };
      case "connecting":
        return { text: $_("settings.tunnel.connecting"), tone: "warn" };
      case "stopped":
        return { text: $_("settings.tunnel.stopped"), tone: "dim" };
      case "failed":
        return { text: $_("settings.tunnel.failed", { values: { reason: t.reason } }), tone: "err" };
    }
  }

  /** Triggered by the "Installieren" button on the pending-update
   * banner. Routes through the manual `checkForUpdates` path so the
   * user sees the native modal confirmation ("install v0.4.X?") and
   * we don't bypass the explicit-consent step. v0.4.44. */
  async function installPendingUpdate() {
    if (busy) return;
    busy = true;
    try {
      await checkForUpdates({ silent: false });
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    void refresh(true);
    startPolling();
    // #208: gate the poll on whether anyone can actually see the pane.
    // `visibilitychange` is the standard signal; the window-level
    // focus/blur pair is the belt-and-braces path for a hidden WKWebView
    // that keeps reporting `visibilityState: "visible"`.
    document.addEventListener("visibilitychange", onVisibilityChange);
    window.addEventListener("blur", onWindowHidden);
    window.addEventListener("focus", onWindowVisible);
    void import("@tauri-apps/api/event").then(({ listen }) => {
      // Listen for the cross-window `update:available` broadcast from
      // Rust so the banner refreshes immediately when the silent
      // updater detects a new version, not only on the 2 s status poll.
      void listen<string | null>("update:available", (e) => {
        if (status) status = { ...status, pending_update: e.payload ?? null };
      });
      // #208: closing the setup window hides it rather than destroying it
      // (Invariant I2), so the DOM never learns the window is gone. Rust
      // says so explicitly from the `CloseRequested` and reopen paths.
      void listen<boolean>("setup:visibility", (e) => {
        if (e.payload) onWindowVisible();
        else onWindowHidden();
      });
    });
  });
  onDestroy(() => {
    stopPolling();
    document.removeEventListener("visibilitychange", onVisibilityChange);
    window.removeEventListener("blur", onWindowHidden);
    window.removeEventListener("focus", onWindowVisible);
  });
</script>

{#if status}
  <main class="window-shell">
    <!-- Outer `<header>` owns the shell padding (alignment with
      .window-scroll / .window-footer); the inner `.app-header` owns
      its component-internal flex layout and bottom border. Codex review
      of PR #129: keeping both classes on the same element let
      `.app-header`'s `padding: 4px 0 12px 0` overwrite the shell's
      horizontal padding, so the header drifted left of the scroll
      content. -->
    <header class="window-header">
    <div class="app-header">
      <img src={iconUrl} alt="aiui" class="app-icon" />
      <div class="header-meta">
        <div class="header-status-line">
          <span class="status-dot" class:ok={status.claude_config_ok}></span>
          {#if status.claude_config_ok}
            {$_("app.status.connected", { values: { port: status.http_port } })}
          {:else}
            {$_("app.status.not_connected")}
          {/if}
        </div>
        <!-- Skill status sits next to the connection status: both are
          "is aiui plumbed in correctly?" signals, both are dot+text, and
          neither needs a full-width row. Repair button only surfaces
          when the skill is actually missing — which is the only time
          the user can act on it. -->
        <div class="header-status-line">
          <span class="status-dot" class:ok={status.skill_installed}></span>
          <span>
            {status.skill_installed
              ? $_("settings.skill.status.ok")
              : $_("settings.skill.status.miss")}
          </span>
          {#if !status.skill_installed}
            <button class="header-action" onclick={repairSkill} disabled={busy}>
              {$_("settings.skill.repair")}
            </button>
          {/if}
        </div>
        <!-- Reassures the user that closing this window doesn't kill aiui:
          mcp_attach's auto-resurrect path relaunches the GUI on next demand.
          Single dim line, Apple-style, no command-flow vocabulary. -->
        <div class="header-tagline">{$_("app.status.background")}</div>
      </div>
      <div class="build-info" title={status.build_info}>{status.build_info.split(" ")[1]}</div>
    </div><!-- /.app-header -->
    </header>

    <!-- Pending-update banner (v0.4.44). Non-modal, dismiss-free: it
         sits above the scroll region until the user clicks Install or
         the on-disk version catches up (Rust clears the flag after
         relaunch). No timer-driven popups, no mid-dialog disruption —
         the user opens Settings and the banner is just there. -->
    {#if status.pending_update}
      <div class="update-banner" role="status" aria-live="polite">
        <span class="update-banner-text">
          {$_("settings.pending_update.text", { values: { version: status.pending_update } })}
        </span>
        <button
          class="update-banner-button"
          onclick={installPendingUpdate}
          disabled={busy}
        >
          {$_("settings.pending_update.install")}
        </button>
      </div>
    {/if}

    <div class="window-scroll">
    <!-- Show the banner only when the live Rust-side TCP self-probe says
      the HTTP server isn't accepting connections. `status.http_error` is
      the explanatory text from the original bind-failure if any — but
      it's not the source of truth for whether to show the banner; that's
      `http_alive`. Issue #77.

      #208 adds two gates on *when*, not on *what decides*: three
      consecutive failures (one slow probe during wake-from-sleep used to
      flash the banner), and never after an uninstall — which deletes the
      token file the probe reads, so every post-uninstall sample reports
      dead while the server is still listening. -->
    {#if !status.http_alive && !uninstallDone && failedProbes >= BANNER_MIN_CONSECUTIVE_FAILURES}
      <section class="http-error">
        <strong>{$_("settings.http_error.title")}</strong>
        {#if status.http_error}
          <p>{status.http_error}</p>
        {/if}
        <!-- Key chosen in Rust by `health_hint_key`: the OS-specific
          port-squatter text only when a bind actually failed or a non-aiui
          service answered, otherwise a hint that names the real cause.
          OS-specific because the diagnostic command differs: `lsof` on
          macOS, `Get-NetTCPConnection` on Windows, `ss` on Linux. -->
        <p class="http-error-hint">{$_(status.http_hint_key, { values: { port: status.http_port } })}</p>
      </section>
    {/if}

    <!-- `!uninstallDone` (#208): `uninstall_all` deletes `first_run_done`,
      so `welcome_pending` flips back to true the moment the user removes
      aiui — re-arming the onboarding wizard on a machine they are walking
      away from. -->
    {#if status.welcome_pending && !uninstallDone}
      <!-- Welcome banner is a 3-step wizard on a single pane. Each step
        is its own visually-distinct row with a numbered marker, a title,
        a one-line body, and (for steps 2-3) a primary CTA button.
        Step 1 collapses to a one-liner when all four checks pass —
        avoids vertical bloat in the common case. Issue raised by tester
        2026-04-27: "viel zu scrollen … vielleicht wäre ein Wizard". -->
      {@const checks = [
        { ok: status.claude_config_ok, key: "desktop" },
        { ok: status.claude_code_config_ok, key: "code" },
        { ok: status.skill_installed, key: "skill" },
        { ok: !status.http_error, key: "http" },
      ]}
      {@const failingCount = checks.filter((c) => !c.ok).length}
      {@const allOk = failingCount === 0}
      <section class="welcome">
        <div class="welcome-head">
          <strong>{$_("settings.welcome.title")}</strong>
          <button class="welcome-dismiss" onclick={dismissWelcome} aria-label={$_("settings.welcome.dismiss")}>×</button>
        </div>
        <p class="welcome-intro">{$_("settings.welcome.intro")}</p>

        <!-- Step 1 — setup checks. Collapsed to summary when all green. -->
        <div class="step" class:step-ok={allOk} class:step-fail={!allOk}>
          <div class="step-marker">1</div>
          <div class="step-content">
            <div class="step-title-row">
              <span class="step-title">{$_("settings.welcome.step1.title")}</span>
              <span class="step-summary">
                {allOk
                  ? $_("settings.welcome.step1.summary_ok")
                  : $_("settings.welcome.step1.summary_fail", { values: { n: failingCount } })}
              </span>
              {#if !allOk || step1Expanded}
                <button class="step-toggle" onclick={() => (step1Expanded = !step1Expanded)}>
                  {step1Expanded
                    ? $_("settings.welcome.step1.collapse")
                    : $_("settings.welcome.step1.expand")}
                </button>
              {:else}
                <button class="step-toggle" onclick={() => (step1Expanded = true)}>
                  {$_("settings.welcome.step1.expand")}
                </button>
              {/if}
            </div>
            {#if step1Expanded || !allOk}
              <ul class="check-list">
                {#each checks as c}
                  <li class:ok={c.ok} class:miss={!c.ok}>
                    <span class="check-mark"></span>
                    {#if c.key === "desktop"}
                      {c.ok ? $_("settings.welcome.check.desktop.ok") : $_("settings.welcome.check.desktop.miss")}
                    {:else if c.key === "code"}
                      {c.ok ? $_("settings.welcome.check.code.ok") : $_("settings.welcome.check.code.miss")}
                    {:else if c.key === "skill"}
                      {c.ok ? $_("settings.welcome.check.skill.ok") : $_("settings.welcome.check.skill.miss")}
                    {:else}
                      {c.ok
                        ? $_("settings.welcome.check.http.ok")
                        : $_("settings.welcome.check.http.miss", { values: { port: status.http_port } })}
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
          </div>
        </div>

        <!-- Step 2 — Claude Desktop restart. Imperative, primary blue
          button. This is the must-do action after fresh install; tester
          missed it on v0.4.13 because it looked optional. -->
        <div class="step">
          <div class="step-marker">2</div>
          <div class="step-content">
            <div class="step-title">{$_("settings.welcome.step2.title")}</div>
            <p class="step-body">{$_("settings.welcome.step2.body")}</p>
            <button class="primary" onclick={restartClaude} disabled={busy} title={$_("settings.restart.hint")}>
              {status.claude_desktop_running
                ? $_("settings.welcome.step2.button.restart")
                : $_("settings.welcome.step2.button.start")}
            </button>
          </div>
        </div>

        <!-- Step 3 — copy demo prompt and paste in Claude. Verification
          step; primary blue button, matches step 2 hierarchy. -->
        <div class="step">
          <div class="step-marker">3</div>
          <div class="step-content">
            <div class="step-title">{$_("settings.welcome.step3.title")}</div>
            <p class="step-body">{$_("settings.welcome.step3.body")}</p>
            <button class="primary" onclick={copyDemoPrompt}>
              {demoCopied ? $_("settings.welcome.demo.copied") : $_("settings.welcome.demo.copy")}
            </button>
            <!-- #208: when neither clipboard route works, the prompt itself
              is the fallback. Without it a failed copy left the wizard with
              no way forward at all — the text is rendered nowhere else. -->
            {#if demoFallbackVisible}
              <textarea
                class="demo-fallback"
                readonly
                rows="6"
                aria-label={$_("settings.welcome.demo.copy")}
                onfocus={(e) => e.currentTarget.select()}
                value={$_("settings.welcome.demo.prompt")}
              ></textarea>
              <p class="step-tail">{$_("settings.welcome.demo.manual")}</p>
            {/if}
            <p class="step-tail">{$_("settings.welcome.step3.tail")}</p>
          </div>
        </div>

        <div class="welcome-foot">
          <button class="welcome-cta" onclick={dismissWelcome}>{$_("settings.welcome.cta")}</button>
        </div>
      </section>
    {/if}

    <!-- Skill status moved into the header (`.header-status-line`)
      alongside the connection dot. Both are "is aiui correctly
      plumbed?" signals; presenting them side-by-side instead of in a
      full-width row keeps the chrome compact and saves a row of
      vertical scroll for the remotes list. -->

    <section>
      <span class="section-label">{$_("settings.remotes.title")}</span>
      {#if status.remotes.length === 0}
        <p class="subtitle" style="margin: 4px 0 0 0;">
          {$_("settings.remotes.empty.hint")}
        </p>
      {:else}
        <div class="stack" style="gap: 6px; margin-top: 6px;">
          {#each status.remotes as h}
            {@const tunnel = statusLabel(status.tunnels[h])}
            <!-- #208: Remove is two-step, the same shape as the footer's
              uninstall confirm. One click used to stop the tunnel, strip
              the ssh forward and delete the token, the MCP entry and the
              skill directory *on the host* — over the network, with no
              undo — from a button sitting flush against the ⟳ icon in a
              520 px window. The confirm takes over the whole row (the host
              name is right there in the question) and `.button-gap` keeps a
              mis-aimed resync click off the destructive button. -->
            <div class="remote-row">
              <span class="dot {tunnel.tone}"></span>
              {#if confirmRemoveHost === h}
                <span class="remote-confirm"
                  >{$_("settings.remotes.remove.confirm", { values: { host: h } })}</span
                >
                <button onclick={() => (confirmRemoveHost = null)} disabled={busy}
                  >{$_("settings.remotes.remove.back")}</button
                >
                <button class="danger" onclick={() => removeRemote(h)} disabled={busy}
                  >{$_("settings.remotes.remove.do")}</button
                >
              {:else}
                <div style="flex: 1; min-width: 0;">
                  <code>{h}</code>
                  <div class="tunnel-status {tunnel.tone}">{tunnel.text}</div>
                </div>
                <button
                  class="icon-button"
                  onclick={() => resyncRemote(h)}
                  disabled={busy}
                  title={$_("settings.remotes.resync.tooltip")}
                  aria-label={$_("settings.remotes.resync.tooltip")}
                >⟳</button>
                <span class="button-gap"></span>
                <button onclick={() => (confirmRemoveHost = h)} disabled={busy}
                  >{$_("settings.remotes.remove")}</button
                >
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </section>

    <section>
      <span class="section-label">{$_("settings.remotes.add.title")}</span>
      <div class="row" style="margin-top: 4px;">
        <input
          type="text"
          placeholder={$_("settings.remotes.add.placeholder")}
          bind:value={newHost}
          onkeydown={(e) => e.key === "Enter" && addRemote()}
        />
        <button class="primary" onclick={addRemote} disabled={busy || !newHost.trim()}>
          {$_("settings.remotes.add.button")}
        </button>
      </div>
      <!-- Inline failure banner. Stays put under the input the user just
        typed into, so they don't have to scroll down to the log to find
        out what went wrong. Tester on v0.4.15: input field cleared on
        failure, error landed only in the scroll-buried log block. -->
      {#if addRemoteError}
        <div class="add-remote-error">
          <strong>{addRemoteError.message}</strong>
          {#if addRemoteError.details}
            <pre>{addRemoteError.details}</pre>
          {/if}
          <button
            class="add-remote-error-dismiss"
            onclick={() => (addRemoteError = null)}
            aria-label="Schließen">×</button>
        </div>
      {/if}
      <p class="subtitle" style="margin: 6px 0 0 0; font-size: 11.5px;">
        {$_("settings.remotes.add.hint")}
      </p>
    </section>

    {#if log.length > 0}
      <section>
        <span class="section-label">{$_("settings.log.title")}</span>
        <div class="stack" style="gap: 3px; margin-top: 4px;">
          {#each log as entry}
            <div class="log-line" class:err={!entry.ok}>
              <span class="dot-small" class:err={!entry.ok}></span>
              {entry.text}
            </div>
          {/each}
        </div>
      </section>
    {/if}

    </div><!-- /.window-scroll -->

    <footer class="window-footer">
      {#if confirmUninstall}
        <span class="subtitle" style="margin-right: auto; align-self: center;">
          {$_("settings.uninstall.confirm")}
        </span>
        <button onclick={() => (confirmUninstall = false)} disabled={busy}
          >{$_("settings.uninstall.back")}</button
        >
        <button class="danger" onclick={doUninstall} disabled={busy}
          >{$_("settings.uninstall.do")}</button
        >
      {:else}
        <button onclick={openIssue} title={$_("settings.report.hint")}>
          {$_("settings.report.button")}
        </button>
        <button onclick={() => checkForUpdates({ silent: false })} disabled={busy}>
          {$_("settings.updates.check")}
        </button>
        <button onclick={() => (confirmUninstall = true)} disabled={busy}
          >{$_("settings.uninstall.button")}</button
        >
      {/if}
    </footer>
  </main>
{/if}

{#if uninstallDone}
  <!-- Modal overlay confirming the cleanup ran and pointing the user at the
    Finder for the actual app removal. We deliberately do NOT auto-trash the
    .app — see RFC discussion: a running app moving its own bundle to Trash
    is fragile, and the user expectation set by the "Uninstall" button is
    "configuration removed", not "self-destruct". -->
  <div class="modal-backdrop" role="presentation" onclick={() => (uninstallDone = false)}>
    <div class="modal" role="dialog" aria-modal="true" onclick={(e) => e.stopPropagation()}>
      <h2>{$_("settings.uninstall.done.title")}</h2>
      <!-- The uninstall-removal hint is OS-specific: drag to Trash on macOS,
        Apps & Features on Windows, etc. The backend reports `status.os` so
        we don't need a separate platform-detection plugin. -->
      <p>{$_(`settings.uninstall.done.body.${status?.os ?? "other"}`)}</p>
      <div class="modal-foot">
        <button class="primary danger" onclick={quitApp}>
          {$_("settings.uninstall.done.quit")}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  /* Pending-update banner (v0.4.44). Lives between .window-header and
     .window-scroll as a flex sibling so it inherits the shell's
     horizontal padding. Yellow accent for "informational, action
     available" without screaming. */
  .update-banner {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 12px;
    margin: 8px 20px 0 20px;
    padding: 8px 12px;
    border: 1px solid color-mix(in srgb, var(--warning, #f3c623) 60%, var(--border));
    background: color-mix(in srgb, var(--warning, #f3c623) 18%, var(--bg, #fff));
    color: color-mix(in srgb, var(--warning, #f3c623) 70%, var(--fg, #000));
    border-radius: 8px;
    font-size: 12.5px;
    line-height: 1.4;
  }
  .update-banner-text {
    flex: 1 1 auto;
    min-width: 0;
  }
  .update-banner-button {
    flex: 0 0 auto;
    background: var(--warning, #f3c623);
    color: var(--bg, #fff);
    border: none;
    border-radius: 6px;
    padding: 4px 10px;
    font-weight: 600;
    cursor: pointer;
  }
  .update-banner-button:hover {
    filter: brightness(0.95);
  }
  .update-banner-button:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .app-header {
    display: flex;
    align-items: center;
    gap: 12px;
    /* Vertical-only padding: the outer `<header class="window-header">`
       owns the horizontal padding so this header stays flush with
       `.window-scroll` and `.window-footer`. Vertical numbers tuned to
       leave space for the title-bar boundary above and the bottom
       border of the section below. Codex review of PR #129. */
    padding: 4px 0 12px 0;
    border-bottom: 1px solid var(--border);
  }
  .app-header .app-icon {
    /* 80×80 — well past 2× the original 32 px. Tester noted that
       the previous 64×80 attempt did not visibly enlarge the icon
       in the rendered window; using `display:block` + explicit
       min-width/min-height defends against inline-image layout
       quirks that can squish the rendered size below the spec. */
    display: block;
    width: 80px;
    height: 80px;
    min-width: 80px;
    min-height: 80px;
    border-radius: 18px;
    box-shadow: var(--shadow-sm);
    flex-shrink: 0;
  }
  .header-action {
    /* Inline action that only surfaces when a status line needs
       fixing (today: skill repair). Smaller than a full button so it
       fits in the header-status-line without reflowing the column. */
    margin-left: 6px;
    padding: 2px 8px;
    font-size: 11px;
  }
  .header-meta {
    flex: 1;
    font-size: 12px;
    color: var(--muted);
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
  }
  .header-status-line {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .header-tagline {
    font-size: 11px;
    color: var(--muted);
    opacity: 0.75;
  }
  .status-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--muted);
    flex-shrink: 0;
  }
  .status-dot.ok { background: var(--success); }
  .build-info {
    font-family: "SF Mono", Menlo, monospace;
    font-size: 10px;
    color: var(--muted);
    background: var(--surface);
    padding: 2px 6px;
    border-radius: 4px;
    border: 1px solid var(--border);
  }

  section {
    display: flex;
    flex-direction: column;
  }
  .section-label {
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  /* `.status-row` styles removed — the skill status moved into the
     header-status-line stack. Kept the comment for future-history. */

  .remote-row {
    display: flex;
    gap: 10px;
    align-items: center;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface-raised);
    box-shadow: var(--shadow-sm);
  }
  .remote-row code {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    background: transparent;
    border: none;
    padding: 0;
  }
  /* Inline confirm for a remote removal (#208). Takes the place of the
     host/tunnel column while armed, so the question has room in a 520 px
     window without the row wrapping. */
  .remote-confirm {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    color: var(--danger);
    line-height: 1.35;
  }
  /* Separates the ⟳ resync icon from the destructive Remove button. They
     used to sit in the same 10 px flex gap, which is how a mis-aimed
     resync click cost a full re-setup of a host (#208). */
  .button-gap {
    flex: 0 0 auto;
    width: 12px;
  }
  /* Read-only copy of the demo prompt, revealed when both clipboard routes
     fail (#208). Selectable, pre-selected on focus — the last resort that
     keeps the welcome wizard from dead-ending. */
  .demo-fallback {
    width: 100%;
    box-sizing: border-box;
    resize: vertical;
    font-family: "SF Mono", Menlo, monospace;
    font-size: 11px;
    line-height: 1.45;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    color: var(--fg);
  }
  .tunnel-status {
    font-size: 11px;
    margin-top: 2px;
  }
  .tunnel-status.ok { color: var(--success); }
  .tunnel-status.warn { color: var(--warning); }
  .tunnel-status.err { color: var(--danger); }
  .tunnel-status.dim { color: var(--muted); }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .dot.ok { background: var(--success); }
  .dot.warn { background: var(--warning); }
  .dot.err { background: var(--danger); }
  .dot.dim { background: var(--muted); }
  .log-line {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    font-size: 12px;
    color: var(--muted);
    padding: 2px 0;
  }
  .log-line.err { color: var(--danger); }
  .dot-small {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--success);
    margin-top: 6px;
    flex-shrink: 0;
  }
  .dot-small.err { background: var(--danger); }

  /* --- inline error banner under add-remote input --- */
  .add-remote-error {
    position: relative;
    margin-top: 6px;
    padding: 8px 32px 8px 10px;
    border: 1px solid var(--danger);
    background: color-mix(in srgb, var(--danger) 10%, var(--surface));
    border-radius: 8px;
    font-size: 12px;
    color: var(--fg);
    line-height: 1.45;
  }
  .add-remote-error strong {
    display: block;
    color: var(--danger);
    font-size: 12.5px;
    margin-bottom: 2px;
  }
  .add-remote-error pre {
    margin: 4px 0 0 0;
    padding: 6px 8px;
    background: var(--surface);
    font-size: 11px;
    line-height: 1.4;
    white-space: pre-wrap;
    word-break: break-word;
    border: 1px solid var(--border);
    border-radius: 4px;
    color: var(--muted);
  }
  .add-remote-error-dismiss {
    position: absolute;
    top: 4px;
    right: 4px;
    background: transparent;
    border: none;
    color: var(--muted);
    font-size: 16px;
    line-height: 1;
    padding: 2px 6px;
    cursor: pointer;
    box-shadow: none;
    border-radius: 4px;
  }
  .add-remote-error-dismiss:hover { color: var(--danger); background: color-mix(in srgb, var(--danger) 8%, transparent); }

  /* --- HTTP error banner --- */
  .http-error {
    border: 1px solid var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, var(--surface));
    border-radius: 10px;
    padding: 10px 14px;
    color: var(--fg);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .http-error strong { font-size: 13px; color: var(--danger); }
  .http-error p { margin: 0; font-size: 12.5px; line-height: 1.5; }
  .http-error-hint { color: var(--muted); }

  /* --- first-run welcome --- */
  .welcome {
    border: 1px solid color-mix(in srgb, var(--accent) 35%, var(--border));
    background: color-mix(in srgb, var(--accent) 8%, var(--surface));
    border-radius: 10px;
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .welcome-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }
  .welcome-head strong {
    font-size: 14px;
    color: var(--fg);
  }
  .welcome-dismiss {
    background: transparent;
    border: none;
    color: var(--muted);
    font-size: 18px;
    line-height: 1;
    padding: 0 4px;
    cursor: pointer;
    box-shadow: none;
    border-radius: 4px;
  }
  .welcome-dismiss:hover { color: var(--fg); background: color-mix(in srgb, var(--fg) 6%, transparent); }
  .welcome-intro {
    margin: 0 0 4px 0;
    font-size: 12.5px;
    color: var(--muted);
    line-height: 1.5;
  }

  /* --- numbered step rows --- */
  .step {
    display: flex;
    gap: 12px;
    align-items: flex-start;
  }
  .step-marker {
    flex-shrink: 0;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: var(--accent);
    color: white;
    font-size: 12px;
    font-weight: 600;
    display: flex;
    align-items: center;
    justify-content: center;
    margin-top: 1px;
  }
  .step-ok .step-marker { background: var(--success); }
  .step-fail .step-marker { background: var(--warning); }
  .step-content {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .step-title-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .step-title {
    font-size: 13px;
    font-weight: 600;
    color: var(--fg);
  }
  .step-summary {
    flex: 1;
    font-size: 12px;
    color: var(--muted);
  }
  .step-toggle {
    background: transparent;
    border: none;
    box-shadow: none;
    color: var(--accent);
    font-size: 11.5px;
    padding: 0;
    cursor: pointer;
  }
  .step-toggle:hover { text-decoration: underline; }
  .step-body {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
    line-height: 1.5;
  }
  .step-tail {
    margin: 4px 0 0 0;
    font-size: 11.5px;
    color: var(--muted);
    line-height: 1.45;
  }

  .check-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }
  .check-list li {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .check-list li.ok { color: var(--fg); }
  .check-list li.miss { color: var(--muted); }
  .check-mark {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
    background: var(--muted);
  }
  .check-list li.ok .check-mark { background: var(--success); }
  .check-list li.miss .check-mark { background: var(--warning); }

  .welcome-foot {
    display: flex;
    justify-content: flex-end;
    margin-top: 4px;
  }
  .welcome-cta {
    background: transparent;
    border: none;
    box-shadow: none;
    color: var(--muted);
    font-size: 11.5px;
    padding: 4px 8px;
    cursor: pointer;
  }
  .welcome-cta:hover { color: var(--fg); text-decoration: underline; }

  /* --- modal (uninstall-done) --- */
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, black 50%, transparent);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .modal {
    background: var(--surface-raised);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 18px 20px;
    max-width: 420px;
    width: calc(100% - 48px);
    box-shadow: var(--shadow-lg, 0 10px 30px rgba(0,0,0,0.35));
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .modal h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }
  .modal p {
    margin: 0;
    font-size: 12.5px;
    line-height: 1.55;
    color: var(--muted);
  }
  .modal p :global(code) {
    background: color-mix(in srgb, var(--fg) 8%, transparent);
    padding: 1px 5px;
    border-radius: 4px;
    font-size: 11.5px;
  }
  .modal-foot {
    display: flex;
    justify-content: flex-end;
    margin-top: 4px;
  }
</style>
