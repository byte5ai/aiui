<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { _ } from "svelte-i18n";
  import { onMount, onDestroy } from "svelte";
  import { checkForUpdates } from "./updater";
  import logoUrl from "../assets/logo.png";

  type StepResult = { ok: boolean; message: string; details: string | null };
  type TunnelStatus =
    | { state: "connecting" }
    | { state: "connected" }
    | { state: "failed"; reason: string }
    | { state: "stopped" };
  type Status = {
    app_binary_path: string;
    token_path: string;
    http_port: number;
    claude_config_ok: boolean;
    remotes: string[];
    tunnels: Record<string, TunnelStatus>;
    build_info: string;
  };

  let status = $state<Status | null>(null);
  let newHost = $state("");
  let busy = $state(false);
  let log = $state<{ text: string; ok: boolean }[]>([]);
  let confirmUninstall = $state(false);
  let timer: number | undefined;

  async function refresh() {
    status = await invoke<Status>("status");
  }

  function pushLog(results: StepResult[]) {
    log = [
      ...results.map((r) => ({ text: r.message + (r.details ? ` — ${r.details}` : ""), ok: r.ok })),
      ...log,
    ].slice(0, 8);
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
      await refresh();
    } finally {
      busy = false;
    }
  }

  let confirmQuit = $state(false);

  async function doQuit() {
    await invoke("force_quit");
    // Never returns; app exits.
  }

  async function reinstallSkill() {
    busy = true;
    try {
      const results = await invoke<StepResult[]>("reinstall_skill");
      pushLog(results);
    } finally {
      busy = false;
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
      <img src={logoUrl} alt="aiui" />
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

    <section>
      <label>{$_("settings.remotes.title")}</label>
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
      <label>{$_("settings.remotes.add.title")}</label>
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
        <label>{$_("settings.log.title")}</label>
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
      {:else if confirmQuit}
        <span class="subtitle" style="margin-right: auto; align-self: center;">
          {$_("settings.quit.confirm")}
        </span>
        <button onclick={() => (confirmQuit = false)} disabled={busy}
          >{$_("settings.uninstall.back")}</button
        >
        <button class="danger" onclick={doQuit} disabled={busy}
          >{$_("settings.quit.do")}</button
        >
      {:else}
        <button onclick={openIssue} title={$_("settings.report.hint")}>
          {$_("settings.report.button")}
        </button>
        <button onclick={reinstallSkill} disabled={busy} title={$_("settings.skill.hint")}>
          {$_("settings.skill.button")}
        </button>
        <button onclick={() => checkForUpdates({ silent: false })} disabled={busy}>
          {$_("settings.updates.check")}
        </button>
        <button onclick={() => (confirmQuit = true)} disabled={busy}
          title={$_("settings.quit.hint")}>{$_("settings.quit.button")}</button
        >
        <button onclick={() => (confirmUninstall = true)} disabled={busy}
          >{$_("settings.uninstall.button")}</button
        >
      {/if}
    </div>
  </div>
{/if}

<style>
  .app-header {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 4px 0 12px 0;
    border-bottom: 1px solid var(--border);
  }
  .app-header img {
    height: 28px;
    width: auto;
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
</style>
