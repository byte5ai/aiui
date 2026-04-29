<script lang="ts">
  import { _ } from "svelte-i18n";

  type Option = {
    label: string;
    description?: string;
    value?: string;
    thumbnail?: string; // data: URL, http(s) URL, or absolute / `~/` local path — bridge-resolved
  };
  type Spec = {
    kind: "ask";
    question: string;
    header?: string;
    options: Option[];
    multiSelect?: boolean;
    allowOther?: boolean;
  };

  let { spec, onsubmit, oncancel }: { spec: Spec; onsubmit: (r: any) => void; oncancel: () => void } = $props();

  let selected = $state<Set<number>>(new Set());
  let other = $state("");
  let otherActive = $state(false);

  function toggle(i: number) {
    if (spec.multiSelect) {
      const s = new Set(selected);
      s.has(i) ? s.delete(i) : s.add(i);
      selected = s;
      otherActive = false;
    } else {
      selected = new Set([i]);
      otherActive = false;
    }
  }

  function toggleOther() {
    otherActive = !otherActive;
    if (otherActive && !spec.multiSelect) selected = new Set();
  }

  function submit() {
    const picks = [...selected].map((i) => spec.options[i].value ?? spec.options[i].label);
    const payload: any = { answers: picks };
    if (otherActive && other.trim()) payload.other = other.trim();
    onsubmit(payload);
  }

  let canSubmit = $derived(selected.size > 0 || (otherActive && other.trim().length > 0));
</script>

<div class="stack">
  {#if spec.header}<span class="chip">{spec.header}</span>{/if}
  <p class="title">{spec.question}</p>

  <div class="stack" style="gap: 8px;">
    {#each spec.options as opt, i}
      <button
        type="button"
        class="option"
        class:selected={selected.has(i)}
        class:has-thumbnail={!!opt.thumbnail}
        onclick={() => toggle(i)}
      >
        {#if opt.thumbnail}
          <img class="option-thumb" src={opt.thumbnail} alt="" />
        {/if}
        <div>
          <div class="label">{opt.label}</div>
          {#if opt.description}<div class="description">{opt.description}</div>{/if}
        </div>
      </button>
    {/each}

    {#if spec.allowOther ?? true}
      <button
        type="button"
        class="option"
        class:selected={otherActive}
        onclick={toggleOther}
      >
        <div style="flex: 1;">
          <div class="label">{$_("dialog.other_answer")}</div>
          {#if otherActive}
            <input
              type="text"
              placeholder={$_("dialog.other_placeholder")}
              bind:value={other}
              style="margin-top: 6px;"
              onclick={(e) => e.stopPropagation()}
            />
          {/if}
        </div>
      </button>
    {/if}
  </div>

  <div class="footer">
    <button onclick={oncancel}>{$_("dialog.cancel")}</button>
    <button class="primary" disabled={!canSubmit} onclick={submit}>{$_("dialog.submit")}</button>
  </div>
</div>
