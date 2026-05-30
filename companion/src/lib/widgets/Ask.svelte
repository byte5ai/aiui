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

<main class="window-shell">
  <div class="window-scroll">
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
      <!-- The text field is a SIBLING of the toggle, never nested inside a
           <button>. Nesting an <input> in a <button> made WebKit treat the
           Space key as button-activation, which flipped `otherActive` off,
           destroyed the field and stole focus on every space typed. -->
      <div class="option" class:selected={otherActive}>
        <div style="flex: 1;">
          <button type="button" class="other-toggle" onclick={toggleOther}>
            <div class="label">{$_("dialog.other_answer")}</div>
          </button>
          {#if otherActive}
            <input
              type="text"
              placeholder={$_("dialog.other_placeholder")}
              bind:value={other}
              style="margin-top: 6px;"
            />
          {/if}
        </div>
      </div>
    {/if}
  </div>

  </div><!-- /.window-scroll -->

  <footer class="window-footer">
    <button onclick={oncancel}>{$_("dialog.cancel")}</button>
    <button class="primary" disabled={!canSubmit} onclick={submit}>{$_("dialog.submit")}</button>
  </footer>
</main>

<style>
  /* The "other answer" label is a plain toggle button so the text field can
     sit beside it (not inside it). Visual is carried by the surrounding
     `.option` card; the button itself is invisible. */
  .other-toggle {
    display: block;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    padding: 0;
    margin: 0;
    font: inherit;
    color: inherit;
    cursor: pointer;
  }
</style>
