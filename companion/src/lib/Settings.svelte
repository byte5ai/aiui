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
  };
  let status = $state<Status | null>(null);
  let newHost = $state("");
  let busy = $state(false);
  let log = $state<{ text: string; ok: boolean }[]>([]);
  let confirmUninstall = $state(false);
  let uninstallDone = $state(false);
  let demoCopied = $state(false);
  /** Live result of the last `/ping` probe. The Rust-side `http_error`
   *  field in the status report is set only at startup and never reset,
   *  so it can lie if the server later recovered (or if a transient
   *  squatter is gone now). We therefore probe every refresh and treat
   *  THIS as the source of truth for "is the server actually alive?".
   *  Issue #74. */
  let httpAlive = $state(true);
  let timer: number | undefined;

  async function probeHttp(port: number): Promise<boolean> {
    try {
      const resp = await fetch(`http://127.0.0.1:${port}/ping`, {
        // /ping is unauthenticated and returns "pong" plain-text; a 200
        // proves the HTTP server is bound and responsive.
        signal: AbortSignal.timeout(800),
      });
      return resp.ok;
    } catch {
      return false;
    }
  }

  async function refresh() {
    const next = await invoke<Status>("status");
    httpAlive = await probeHttp(next.http_port);
    status = next;
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
        <span class="status-dot" class:ok={status.claude_config_ok}></span>
        {#if status.claude_config_ok}
          {$_("app.status.connected", { values: { port: status.http_port } })}
        {:else}
          {$_("app.status.not_connected")}
        {/if}
      </div>
      <div class="build-info" title={status.build_info}>{status.build_info.split(" ")[1]}</div>
    </header>

    <!-- Show the banner only when the LIVE probe says the HTTP server is
      actually unreachable. The Rust-side `status.http_error` is the
      original bind-failure reason if any — useful as the explanatory
      hint, but on its own it's a stale flag once the server later
      recovers. Issue #74. -->
    {#if !httpAlive}
      <section class="http-error">
        <strong>{$_("settings.http_error.title")}</strong>
        {#if status.http_error}
          <p>{status.http_error}</p>
        {/if}
        <p class="http-error-hint">{$_("settings.http_error.hint", { values: { port: status.http_port } })}</p>
      </section>
    {/if}

    {#if status.welcome_pending}
      <!-- Welcome banner doubles as the live setup health-check. Each row
        reflects a real condition checked at refresh-time, not just a static
        promise — fixes the v0.4.x bug where "everything ready" appeared
        even if the MCP entry hadn't actually been written. -->
      <section class="welcome">
        <div class="welcome-head">
          <strong>{$_("settings.welcome.title")}</strong>
          <button class="welcome-dismiss" onclick={dismissWelcome} aria-label={$_("settings.welcome.dismiss")}>×</button>
        </div>
        <p class="welcome-body">{$_("settings.welcome.body")}</p>
        <ul class="check-list">
          <li class:ok={status.claude_config_ok} class:miss={!status.claude_config_ok}>
            <span class="check-mark"></span>
            {status.claude_config_ok
              ? $_("settings.welcome.check.desktop.ok")
              : $_("settings.welcome.check.desktop.miss")}
          </li>
          <li class:ok={status.claude_code_config_ok} class:miss={!status.claude_code_config_ok}>
            <span class="check-mark"></span>
            {status.claude_code_config_ok
              ? $_("settings.welcome.check.code.ok")
              : $_("settings.welcome.check.code.miss")}
          </li>
          <li class:ok={status.skill_installed} class:miss={!status.skill_installed}>
            <span class="check-mark"></span>
            {status.skill_installed
              ? $_("settings.welcome.check.skill.ok")
              : $_("settings.welcome.check.skill.miss")}
          </li>
          <li class:ok={!status.http_error} class:miss={!!status.http_error}>
            <span class="check-mark"></span>
            {status.http_error
              ? $_("settings.welcome.check.http.miss", { values: { port: status.http_port } })
              : $_("settings.welcome.check.http.ok")}
          </li>
        </ul>

        <div class="welcome-action">
          <span class="welcome-action-label">{$_("settings.welcome.next.restart")}</span>
          <button onclick={restartClaude} disabled={busy} title={$_("settings.restart.hint")}>
            {status.claude_desktop_running
              ? $_("settings.welcome.next.restart_button.restart")
              : $_("settings.welcome.next.restart_button.start")}
          </button>
        </div>

        <!-- Demo section. Replaces the old "Test-Dialog jetzt"-button — that
          one looped back through aiui's own /render endpoint and proved
          nothing the user couldn't already see (this very window is rendered
          by the same WebView). What new users actually need is a concrete
          way to see aiui in action *inside Claude*. Issue #70. -->
        <div class="demo-block">
          <div class="demo-title">{$_("settings.welcome.demo.title")}</div>
          <p class="demo-slash">
            {$_("settings.welcome.demo.slash_intro")}<code>{$_("settings.welcome.demo.slash_code")}</code>{$_("settings.welcome.demo.slash_outro")}<code>{$_("settings.welcome.demo.slash_teach")}</code>{$_("settings.welcome.demo.slash_after")}
          </p>
          <p class="demo-prompt-intro">{$_("settings.welcome.demo.prompt_intro")}</p>
          <div class="demo-prompt-row">
            <textarea readonly class="demo-prompt-text" rows="3">{$_("settings.welcome.demo.prompt")}</textarea>
            <button class="demo-copy" onclick={copyDemoPrompt}>
              {demoCopied ? $_("settings.welcome.demo.copied") : $_("settings.welcome.demo.copy")}
            </button>
          </div>
        </div>

        <div class="welcome-foot">
          <button class="primary" onclick={dismissWelcome}>{$_("settings.welcome.cta")}</button>
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
    align-items: center;
    gap: 6px;
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
  .welcome-body {
    margin: 0;
    font-size: 12.5px;
    color: var(--muted);
    line-height: 1.5;
  }
  .check-list {
    list-style: none;
    margin: 4px 0 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12.5px;
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

  .welcome-action {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 6px;
  }
  .welcome-action-label {
    font-size: 12.5px;
    color: var(--muted);
    flex: 1;
  }

  .demo-block {
    border: 1px solid var(--border);
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 4px;
  }
  .demo-title {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--fg);
  }
  .demo-slash {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
    line-height: 1.5;
  }
  .demo-slash code {
    background: color-mix(in srgb, var(--fg) 8%, transparent);
    padding: 1px 5px;
    border-radius: 4px;
    font-size: 11.5px;
  }
  .demo-prompt-intro {
    margin: 4px 0 0 0;
    font-size: 12px;
    color: var(--muted);
  }
  .demo-prompt-row {
    display: flex;
    gap: 6px;
    align-items: stretch;
  }
  .demo-prompt-text {
    flex: 1;
    font-family: inherit;
    font-size: 12px;
    line-height: 1.45;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    color: var(--fg);
    resize: none;
  }
  .demo-copy {
    flex-shrink: 0;
    align-self: flex-start;
  }

  .welcome-foot {
    display: flex;
    justify-content: flex-end;
    margin-top: 6px;
  }

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
