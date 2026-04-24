<script lang="ts">
  import { _ } from "svelte-i18n";
  import TreeNode from "./TreeNode.svelte";

  type SelectOption = { label: string; value: string; description?: string };

  type TreeItem = {
    label: string;
    value: string;
    description?: string;
    children?: TreeItem[];
  };

  type Field =
    | { kind: "text"; name: string; label: string; placeholder?: string; default?: string; multiline?: boolean; required?: boolean }
    | { kind: "password"; name: string; label: string; placeholder?: string; required?: boolean }
    | { kind: "number"; name: string; label: string; default?: number; min?: number; max?: number; step?: number; required?: boolean }
    | { kind: "select"; name: string; label: string; options: SelectOption[]; default?: string; required?: boolean }
    | { kind: "checkbox"; name: string; label: string; default?: boolean }
    | { kind: "slider"; name: string; label: string; min: number; max: number; step?: number; default?: number }
    | { kind: "date"; name: string; label: string; default?: string; required?: boolean }
    | { kind: "date_range"; name: string; label: string; default?: { from?: string; to?: string }; required?: boolean }
    | { kind: "color"; name: string; label: string; default?: string }
    | { kind: "static_text"; text: string; tone?: "info" | "warn" | "muted" }
    | {
        kind: "list";
        name: string;
        label?: string;
        items: { label: string; value: string; description?: string }[];
        selectable?: boolean;
        multi_select?: boolean;
        sortable?: boolean;
        default_selected?: string[];
      }
    | {
        kind: "tree";
        name: string;
        label?: string;
        items: TreeItem[];
        multi_select?: boolean;
        default_selected?: string[];
        default_expanded?: string[];
      };

  type Action = {
    label: string;
    value: string;
    primary?: boolean;
    destructive?: boolean;
    /** Positive-outcome styling (green). For "success"-type semantics like "Approve", "Accept", "Publish". */
    success?: boolean;
    /** If true, field-level required-validation is skipped when this action fires (e.g. "defer"). */
    skip_validation?: boolean;
  };

  interface Spec {
    kind: "form";
    title: string;
    description?: string;
    header?: string;
    fields: Field[];
    actions?: Action[];
    /** @deprecated legacy fallback */
    submitLabel?: string;
    /** @deprecated legacy fallback */
    cancelLabel?: string;
  }

  interface Props {
    spec: Spec;
    onsubmit: (r: any) => void;
    oncancel: () => void;
  }

  let { spec, onsubmit, oncancel }: Props = $props();

  function collectTreeValues(items: TreeItem[]): string[] {
    return items.flatMap((it) => [it.value, ...collectTreeValues(it.children ?? [])]);
  }

  function initialValue(f: Field): any {
    switch (f.kind) {
      case "static_text":
        return undefined;
      case "checkbox":
        return f.default ?? false;
      case "slider":
        return f.default ?? f.min;
      case "color":
        return f.default ?? "#000000";
      case "date_range":
        return { from: f.default?.from ?? "", to: f.default?.to ?? "" };
      case "list":
        return {
          selected: [...(f.default_selected ?? [])],
          order: f.items.map((it) => it.value),
        };
      case "tree":
        return {
          selected: [...(f.default_selected ?? [])],
          expanded: new Set(f.default_expanded ?? collectTreeValues(f.items)),
        };
      default:
        return (f as any).default ?? "";
    }
  }

  function valueFields(s: Spec): Field[] {
    return s.fields.filter((f) => f.kind !== "static_text");
  }

  let values = $state<Record<string, any>>(
    Object.fromEntries(
      valueFields(spec).map((f) => [(f as any).name, initialValue(f)])
    )
  );

  // List sorting helpers
  let dragFrom = $state<{ name: string; idx: number } | null>(null);

  function moveItem(name: string, from: number, to: number) {
    const list = values[name] as { selected: string[]; order: string[] };
    if (from === to || to < 0 || to >= list.order.length) return;
    const order = [...list.order];
    const [moved] = order.splice(from, 1);
    order.splice(to, 0, moved);
    values[name] = { ...list, order };
  }

  function toggleListItem(name: string, value: string, multi: boolean) {
    const list = values[name] as { selected: string[]; order: string[] };
    let selected = list.selected;
    if (multi) {
      selected = selected.includes(value)
        ? selected.filter((v) => v !== value)
        : [...selected, value];
    } else {
      selected = selected.includes(value) ? [] : [value];
    }
    values[name] = { ...list, selected };
  }

  function toggleTreeExpand(name: string, value: string) {
    const t = values[name] as { selected: string[]; expanded: Set<string> };
    const expanded = new Set(t.expanded);
    expanded.has(value) ? expanded.delete(value) : expanded.add(value);
    values[name] = { ...t, expanded };
  }

  function toggleTreeSelect(name: string, value: string, multi: boolean) {
    const t = values[name] as { selected: string[]; expanded: Set<string> };
    let selected = t.selected;
    if (multi) {
      selected = selected.includes(value)
        ? selected.filter((v) => v !== value)
        : [...selected, value];
    } else {
      selected = selected.includes(value) ? [] : [value];
    }
    values[name] = { ...t, selected };
  }

  // Validation
  function isFieldComplete(f: Field): boolean {
    if (f.kind === "static_text") return true;
    if (f.kind === "checkbox" || f.kind === "slider") return true;
    if (f.kind === "list") {
      if (!f.selectable || !f.multi_select) {
        // selecting nothing is valid unless required — treat list without
        // required flag as never required for now.
      }
      return true;
    }
    if (!("required" in f) || !f.required) return true;
    const v = values[(f as any).name];
    return v !== undefined && v !== null && String(v).length > 0;
  }

  let canSubmit = $derived(spec.fields.every(isFieldComplete));

  // Actions — default to submit/cancel if none provided (backwards compat)
  let actions = $derived<Action[]>(
    spec.actions && spec.actions.length > 0
      ? spec.actions
      : [
          { label: spec.cancelLabel ?? $_("dialog.cancel"), value: "__cancel__", skip_validation: true },
          { label: spec.submitLabel ?? $_("dialog.submit"), value: "__submit__", primary: true },
        ]
  );

  function serialisableValues(): Record<string, any> {
    // Sets don't JSON-serialize; strip tree `expanded` and keep just `selected`.
    const out: Record<string, any> = {};
    for (const [k, v] of Object.entries(values)) {
      if (v && typeof v === "object" && "expanded" in v && v.expanded instanceof Set) {
        out[k] = { selected: v.selected };
      } else {
        out[k] = v;
      }
    }
    return out;
  }

  function runAction(a: Action) {
    if (a.value === "__cancel__") {
      oncancel();
      return;
    }
    if (!a.skip_validation && !canSubmit) return;
    onsubmit({
      action: a.value === "__submit__" ? null : a.value,
      values: serialisableValues(),
    });
  }
</script>

<div class="stack">
  {#if spec.header}<span class="chip">{spec.header}</span>{/if}
  <div>
    <p class="title">{spec.title}</p>
    {#if spec.description}<p class="subtitle">{spec.description}</p>{/if}
  </div>

  <div class="stack" style="gap: 12px;">
    {#each spec.fields as f}
      {#if f.kind === "static_text"}
        <div class="static-text {f.tone ?? 'info'}">{f.text}</div>
      {:else if f.kind === "tree"}
        {@const treeValue = values[f.name] as { selected: string[]; expanded: Set<string> }}
        <div>
          {#if f.label}<label>{f.label}</label>{/if}
          <div class="tree-widget">
            {#each f.items as root (root.value)}
              <TreeNode
                item={root}
                depth={0}
                selected={treeValue.selected}
                expanded={treeValue.expanded}
                multiSelect={!!f.multi_select}
                onToggleExpand={(v) => toggleTreeExpand(f.name, v)}
                onToggleSelect={(v) => toggleTreeSelect(f.name, v, !!f.multi_select)}
              />
            {/each}
          </div>
        </div>
      {:else if f.kind === "list"}
        {@const listValue = values[f.name] as { selected: string[]; order: string[] }}
        <div>
          {#if f.label}<label>{f.label}</label>{/if}
          <div class="list-widget" class:sortable={f.sortable}>
            {#each listValue.order as itemValue, idx (itemValue)}
              {@const item = f.items.find((x: { label: string; value: string; description?: string }) => x.value === itemValue)}
              {#if item}
                <div
                  class="list-item"
                  class:selected={f.selectable && listValue.selected.includes(item.value)}
                  class:clickable={f.selectable}
                  draggable={f.sortable}
                  ondragstart={(e) => {
                    if (!f.sortable) return;
                    dragFrom = { name: f.name, idx };
                    if (e.dataTransfer) {
                      // required by Firefox; Safari needs a non-empty payload too
                      e.dataTransfer.effectAllowed = "move";
                      e.dataTransfer.setData("text/plain", itemValue);
                    }
                  }}
                  ondragover={(e) => {
                    if (f.sortable && dragFrom?.name === f.name) {
                      e.preventDefault();
                      if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
                    }
                  }}
                  ondrop={(e) => {
                    const from = dragFrom;
                    if (!f.sortable || !from || from.name !== f.name) return;
                    e.preventDefault();
                    moveItem(f.name, from.idx, idx);
                    dragFrom = null;
                  }}
                  ondragend={() => {
                    dragFrom = null;
                  }}
                  onclick={() =>
                    f.selectable && toggleListItem(f.name, item.value, !!f.multi_select)}
                  role={f.selectable ? "button" : undefined}
                  tabindex={f.selectable ? 0 : -1}
                >
                  {#if f.sortable}<span class="drag-handle" aria-hidden="true">⋮⋮</span>{/if}
                  {#if f.selectable}
                    <span class="check" class:on={listValue.selected.includes(item.value)}>
                      {#if listValue.selected.includes(item.value)}✓{/if}
                    </span>
                  {/if}
                  <div style="flex: 1; min-width: 0;">
                    <div class="item-label">{item.label}</div>
                    {#if item.description}<div class="item-desc">{item.description}</div>{/if}
                  </div>
                </div>
              {/if}
            {/each}
          </div>
        </div>
      {:else}
        <div>
          <label>{f.label}{"required" in f && f.required ? " *" : ""}</label>
          {#if f.kind === "text"}
            {#if f.multiline}
              <textarea placeholder={f.placeholder ?? ""} bind:value={values[f.name]} rows="4"></textarea>
            {:else}
              <input type="text" placeholder={f.placeholder ?? ""} bind:value={values[f.name]} />
            {/if}
          {:else if f.kind === "password"}
            <input type="password" placeholder={f.placeholder ?? ""} bind:value={values[f.name]} autocomplete="off" />
          {:else if f.kind === "number"}
            <input type="number" min={f.min} max={f.max} step={f.step ?? 1} bind:value={values[f.name]} />
          {:else if f.kind === "select"}
            <select bind:value={values[f.name]}>
              {#each f.options as opt}
                <option value={opt.value}>{opt.label}</option>
              {/each}
            </select>
          {:else if f.kind === "checkbox"}
            <div class="row">
              <input type="checkbox" bind:checked={values[f.name]} id={`f-${f.name}`} />
              <label for={`f-${f.name}`} style="margin: 0; text-transform: none; font-size: 14px; color: var(--fg);"
                >{f.label}</label>
            </div>
          {:else if f.kind === "slider"}
            <div class="row">
              <input type="range" min={f.min} max={f.max} step={f.step ?? 1} bind:value={values[f.name]} style="flex: 1;" />
              <code>{values[f.name]}</code>
            </div>
          {:else if f.kind === "date"}
            <input type="date" bind:value={values[f.name]} />
          {:else if f.kind === "date_range"}
            <div class="row">
              <input type="date" bind:value={values[f.name].from} style="flex: 1;" />
              <span style="color: var(--muted); font-size: 12px;">—</span>
              <input type="date" bind:value={values[f.name].to} style="flex: 1;" />
            </div>
          {:else if f.kind === "color"}
            <div class="row">
              <input type="color" bind:value={values[f.name]} style="width: 50px; height: 34px; padding: 2px;" />
              <code>{values[f.name]}</code>
            </div>
          {/if}
        </div>
      {/if}
    {/each}
  </div>

  <div class="footer">
    {#each actions as a}
      <button
        class:primary={a.primary}
        class:danger={a.destructive}
        class:success={a.success}
        disabled={!a.skip_validation && !canSubmit && !a.destructive}
        onclick={() => runAction(a)}
      >
        {a.label}
      </button>
    {/each}
  </div>
</div>

<style>
  .static-text {
    padding: 10px 12px;
    border-radius: 8px;
    font-size: 13px;
    line-height: 1.5;
    border: 1px solid var(--border);
    background: var(--surface);
    white-space: pre-wrap;
  }
  .static-text.info { color: var(--fg); }
  .static-text.warn { border-color: #f59e0b; background: color-mix(in srgb, #f59e0b 10%, var(--surface)); }
  .static-text.muted { color: var(--muted); font-size: 12px; }

  .list-widget {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 4px;
  }
  .list-item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    transition: border-color 0.12s, background 0.12s;
  }
  .list-item.clickable { cursor: pointer; }
  .list-item.clickable:hover { border-color: var(--accent); }
  .list-item.selected {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, var(--surface));
  }
  .drag-handle {
    color: var(--muted);
    font-size: 12px;
    letter-spacing: -2px;
    cursor: grab;
    user-select: none;
  }
  .check {
    display: inline-flex;
    justify-content: center;
    align-items: center;
    width: 18px;
    height: 18px;
    border: 1px solid var(--border);
    border-radius: 4px;
    font-size: 12px;
    flex-shrink: 0;
  }
  .check.on { background: var(--accent); color: var(--accent-fg); border-color: var(--accent); }
  .item-label { font-size: 14px; font-weight: 500; }
  .item-desc { font-size: 12px; color: var(--muted); margin-top: 2px; }
</style>
