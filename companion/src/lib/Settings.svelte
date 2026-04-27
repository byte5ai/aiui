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
  };
  let status = $state<Status | null>(null);
  let newHost = $state("");
  let busy = $state(false);
  let log = $state<{ text: string; ok: boolean }[]>([]);
  let confirmUninstall = $state(false);
  let uninstallDone = $state(false);
  let demoCopied = $state(false);
  let step1Expanded = $state(false);
  let timer: number | undefined;

  async function refresh() {
    // The status report carries `http_alive` from a Rust-side TCP
    // self-probe — WebView `fetch()` would be ATS-blocked on macOS for
    // plaintext localhost, which is how v0.4.8 ended up with a permanent
    // false-positive banner. Issue #77.
    status = await invoke<Status>("status");
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
    try {
      const results = await invoke<StepResult[]>("add_remote", { hostAlias: newHost.trim() });
      pushLog(results);
      newHost = "";
      await refresh();
    } finally {
      busy = false;
    }
  }

  async function removeRemote(host: string) {
    busy = true;
    try {
      const results = await invoke<StepResult[]>("remove_remote", { hostAlias: host });
      pushLog(results);
      await refresh();
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
      await refresh();
    } finally {
      busy = false;
    }
  }

  async function copyDemoPrompt() {
    try {
      await navigator.clipboard.writeText($_("settings.welcome.demo.prompt"));
      demoCopied = true;
      window.setTimeout(() => (demoCopied = false), 2000);
    } catch {
      // Clipboard API unavailable in some Tauri/macOS combinations — silent
      // fallback so the UI doesn't lie about success.
      demoCopied = false;
    }
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
      await refresh();
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

  function openIssue() {
    const body = encodeURIComponent(
      `**Version:** ${status?.build_info ?? "unknown"}\n\n` +
        `**Describe the bug:**\n\n\n` +
        `**Steps to reproduce:**\n1.\n2.\n3.\n\n` +
        `**Expected / actual:**\n\n`,
    );
    window.open(`https://github.com/byte5ai/aiui/issues/new?body=${body}`, "_blank");
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

  onMount(() => {
    refresh();
    timer = window.setInterval(refresh, 2000);
  });
  onDestroy(() => {
    if (timer) window.clearInterval(timer);
  });
</script>

{#if status}
  <div class="stack">
    <header class="app-header">
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
        <!-- Reassures the user that closing this window doesn't kill aiui:
          mcp_attach's auto-resurrect path relaunches the GUI on next demand.
          Single dim line, Apple-style, no command-flow vocabulary. -->
        <div class="header-tagline">{$_("app.status.background")}</div>
      </div>
      <div class="build-info" title={status.build_info}>{status.build_info.split(" ")[1]}</div>
    </header>

    <!-- Show the banner only when the live Rust-side TCP self-probe says
      the HTTP server isn't accepting connections. `status.http_error` is
      the explanatory text from the original bind-failure if any — but
      it's not the source of truth for whether to show the banner; that's
      `http_alive`. Issue #77. -->
    {#if !status.http_alive}
      <section class="http-error">
        <strong>{$_("settings.http_error.title")}</strong>
        {#if status.http_error}
          <p>{status.http_error}</p>
        {/if}
        <p class="http-error-hint">{$_("settings.http_error.hint", { values: { port: status.http_port } })}</p>
      </section>
    {/if}

    {#if status.welcome_pending}
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
            <p class="step-tail">{$_("settings.welcome.step3.tail")}</p>
          </div>
        </div>

        <div class="welcome-foot">
          <button class="welcome-cta" onclick={dismissWelcome}>{$_("settings.welcome.cta")}</button>
        </div>
      </section>
    {/if}

    <!-- Skill status row. Replaces the old "Skill installieren" button which
      suggested optionality — the skill is mandatory and auto-installed on
      every GUI launch, so the only meaningful UI is "is it there?" plus a
      repair button when it isn't. -->
    <section class="status-row" class:err={!status.skill_installed}>
      <span class="dot {status.skill_installed ? 'ok' : 'err'}"></span>
      <span class="status-text">
        {status.skill_installed ? $_("settings.skill.status.ok") : $_("settings.skill.status.miss")}
      </span>
      {#if !status.skill_installed}
        <button onclick={repairSkill} disabled={busy}>{$_("settings.skill.repair")}</button>
      {/if}
    </section>

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
            <div class="remote-row">
              <span class="dot {tunnel.tone}"></span>
              <div style="flex: 1; min-width: 0;">
                <code>{h}</code>
                <div class="tunnel-status {tunnel.tone}">{tunnel.text}</div>
              </div>
              <button onclick={() => removeRemote(h)} disabled={busy}
                >{$_("settings.remotes.remove")}</button
              >
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

    <div class="footer">
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
    </div>
  </div>
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
      <p>{$_("settings.uninstall.done.body")}</p>
      <div class="modal-foot">
        <button class="primary danger" onclick={quitApp}>
          {$_("settings.uninstall.done.quit")}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .app-header {
    display: flex;
    align-items: center;
    gap: 12px;
    /* `.container` already pads 44 px down from the window edge for the
     * macOS traffic-light buttons; we only add a hair of breathing room
     * here so the logo doesn't ride directly against the title-area
     * gradient line. */
    padding: 8px 0 12px 0;
    border-bottom: 1px solid var(--border);
  }
  .app-header img.app-icon {
    width: 32px;
    height: 32px;
    border-radius: 7px;
    box-shadow: var(--shadow-sm);
    flex-shrink: 0;
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

  .status-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface-raised);
    box-shadow: var(--shadow-sm);
    font-size: 12.5px;
  }
  .status-row.err { border-color: var(--danger); }
  .status-row .status-text { flex: 1; }

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
