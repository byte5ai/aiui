<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { _ } from "svelte-i18n";
  import { onMount, onDestroy } from "svelte";

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
    <div>
      <p class="title" style="margin-bottom: 2px;">{$_("app.title")}</p>
      <p class="subtitle" style="margin: 0;">
        {#if status.claude_config_ok}
          {$_("app.status.connected", { values: { port: status.http_port } })}
        {:else}
          {$_("app.status.not_connected")}
        {/if}
      </p>
      <p class="subtitle" style="margin: 4px 0 0 0; opacity: 0.6; font-size: 11px;">
        {status.build_info}
      </p>
    </div>

    <div>
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
                <code style="display: block; overflow: hidden; text-overflow: ellipsis;">{h}</code>
                <div class="tunnel-status {tunnel.tone}">{tunnel.text}</div>
              </div>
              <button onclick={() => removeRemote(h)} disabled={busy}
                >{$_("settings.remotes.remove")}</button
              >
            </div>
          {/each}
        </div>
      {/if}
    </div>

    <div>
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
    </div>

    {#if log.length > 0}
      <div>
        <label>{$_("settings.log.title")}</label>
        <div class="stack" style="gap: 3px; margin-top: 4px;">
          {#each log as entry}
            <div class="log-line" class:err={!entry.ok}>
              <span class="dot-small" class:err={!entry.ok}></span>
              {entry.text}
            </div>
          {/each}
        </div>
      </div>
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
        <button onclick={() => (confirmUninstall = true)} disabled={busy}
          >{$_("settings.uninstall.button")}</button
        >
      {/if}
    </div>
  </div>
{/if}

<style>
  .remote-row {
    display: flex;
    gap: 10px;
    align-items: center;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
  }
  .tunnel-status {
    font-size: 11.5px;
    margin-top: 2px;
  }
  .tunnel-status.ok { color: #22c55e; }
  .tunnel-status.warn { color: #f59e0b; }
  .tunnel-status.err { color: var(--danger); }
  .tunnel-status.dim { color: var(--muted); }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .dot.ok { background: #22c55e; }
  .dot.warn { background: #f59e0b; }
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
  .log-line.err {
    color: var(--danger);
  }
  .dot-small {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #22c55e;
    margin-top: 6px;
    flex-shrink: 0;
  }
  .dot-small.err {
    background: var(--danger);
  }
</style>
