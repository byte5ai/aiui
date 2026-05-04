<script lang="ts">
  import { _ } from "svelte-i18n";
  import { marked } from "marked";
  import DOMPurify from "dompurify";
  import TreeNode from "./TreeNode.svelte";
  import MermaidView from "./MermaidView.svelte";
  import WireframeView from "./WireframeView.svelte";

  type SelectOption = { label: string; value: string; description?: string };

  type TreeItem = {
    label: string;
    value: string;
    description?: string;
    children?: TreeItem[];
  };

  type ListItem = {
    label: string;
    value: string;
    description?: string;
    thumbnail?: string; // data: URL or absolute path
  };

  type ImageGridItem = {
    value: string;
    src: string; // data: URL or path
    label?: string;
  };

  type TableColumn = {
    key: string;
    label: string;
    align?: "left" | "right" | "center";
  };
  type TableRow = {
    value: string;
    values: Record<string, string | number | null>;
  };

  type Field =
    | { kind: "text"; name: string; label: string; placeholder?: string; default?: string; multiline?: boolean; required?: boolean }
    | { kind: "password"; name: string; label: string; placeholder?: string; required?: boolean }
    | { kind: "number"; name: string; label: string; default?: number; min?: number; max?: number; step?: number; required?: boolean }
    | { kind: "select"; name: string; label: string; options: SelectOption[]; default?: string; required?: boolean }
    | { kind: "checkbox"; name: string; label: string; default?: boolean }
    | { kind: "slider"; name: string; label: string; min: number; max: number; step?: number; default?: number }
    | { kind: "date"; name: string; label: string; default?: string; required?: boolean }
    | { kind: "datetime"; name: string; label: string; default?: string; required?: boolean }
    | { kind: "date_range"; name: string; label: string; default?: { from?: string; to?: string }; required?: boolean }
    | { kind: "color"; name: string; label: string; default?: string }
    | { kind: "static_text"; text: string; tone?: "info" | "warn" | "muted" }
    | { kind: "markdown"; text: string }
    | { kind: "image"; src: string; label?: string; alt?: string; max_height?: number }
    | { kind: "mermaid"; source: string; label?: string; max_height?: number }
    | {
        kind: "wireframe";
        panels: Array<{
          title?: string;
          content?: string;
          col_span?: number;
          row_span?: number;
          tone?: "default" | "muted" | "highlight";
        }>;
        columns?: number;
        gap?: number;
        label?: string;
        max_height?: number;
      }
    | {
        kind: "image_grid";
        name: string;
        label?: string;
        images: ImageGridItem[];
        multi_select?: boolean;
        columns?: number;
        default_selected?: string[];
        required?: boolean;
      }
    | {
        kind: "list";
        name: string;
        label?: string;
        items: ListItem[];
        selectable?: boolean;
        multi_select?: boolean;
        sortable?: boolean;
        default_selected?: string[];
      }
    | {
        kind: "table";
        name: string;
        label?: string;
        columns: TableColumn[];
        rows: TableRow[];
        multi_select?: boolean;
        sortable_by_column?: boolean;
        default_selected?: string[];
        required?: boolean;
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

  type Tab = { label: string; fields: Field[] };

  interface Spec {
    kind: "form";
    title: string;
    description?: string;
    header?: string;
    /** Either flat fields, or tabs. If both are set, `tabs` wins. */
    fields?: Field[];
    tabs?: Tab[];
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

  // --- tab handling -------------------------------------------------------
  // If `tabs` is set, fields are the union across tabs; we render only the
  // active tab's fields, but validate over all of them.
  let activeTab = $state(0);
  let allFields = $derived<Field[]>(
    spec.tabs && spec.tabs.length > 0
      ? spec.tabs.flatMap((t) => t.fields)
      : spec.fields ?? []
  );
  let visibleFields = $derived<Field[]>(
    spec.tabs && spec.tabs.length > 0
      ? spec.tabs[activeTab]?.fields ?? []
      : spec.fields ?? []
  );

  function collectTreeValues(items: TreeItem[]): string[] {
    return items.flatMap((it) => [it.value, ...collectTreeValues(it.children ?? [])]);
  }

  // Be forgiving when an MCP caller hands us list items as plain strings
  // (`items: ["A", "B", "C"]`) instead of the documented
  // `items: [{label, value}]` shape. Without this, the agent's first
  // attempt at a sortable list often produces an empty render — the
  // documented shape isn't easy to discover from the tool's input
  // schema. Normalize once and use everywhere.
  function listItems(f: Extract<Field, { kind: "list" }>): ListItem[] {
    return (f.items as unknown as Array<ListItem | string>).map((it) =>
      typeof it === "string" ? { label: it, value: it } : it,
    );
  }

  function initialValue(f: Field): any {
    switch (f.kind) {
      case "static_text":
      case "markdown":
      case "image":
      case "mermaid":
      case "wireframe":
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
          order: listItems(f).map((it) => it.value),
        };
      case "table":
        return {
          selected: [...(f.default_selected ?? [])],
          order: f.rows.map((r) => r.value),
          sort: { column: null as string | null, dir: "asc" as "asc" | "desc" },
        };
      case "image_grid":
        return { selected: [...(f.default_selected ?? [])] };
      case "tree":
        return {
          selected: [...(f.default_selected ?? [])],
          expanded: new Set(f.default_expanded ?? collectTreeValues(f.items)),
        };
      default:
        return (f as any).default ?? "";
    }
  }

  function valueFields(fs: Field[]): Field[] {
    return fs.filter(
      (f) =>
        f.kind !== "static_text" &&
        f.kind !== "markdown" &&
        f.kind !== "image" &&
        f.kind !== "mermaid" &&
        f.kind !== "wireframe"
    );
  }

  let values = $state<Record<string, any>>(
    Object.fromEntries(
      valueFields(allFields).map((f) => [(f as any).name, initialValue(f)])
    )
  );

  // --- list sorting -------------------------------------------------------
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

  function toggleTableRow(name: string, value: string, multi: boolean) {
    const t = values[name] as { selected: string[]; order: string[]; sort: any };
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

  function sortTableBy(field: { name: string; rows: TableRow[] }, key: string) {
    const t = values[field.name] as {
      selected: string[];
      order: string[];
      sort: { column: string | null; dir: "asc" | "desc" };
    };
    const dir = t.sort.column === key && t.sort.dir === "asc" ? "desc" : "asc";
    const rowMap = new Map(field.rows.map((r) => [r.value, r]));
    const order = [...t.order].sort((a, b) => {
      const av = rowMap.get(a)?.values[key];
      const bv = rowMap.get(b)?.values[key];
      const cmp =
        av === null || av === undefined
          ? 1
          : bv === null || bv === undefined
          ? -1
          : typeof av === "number" && typeof bv === "number"
          ? av - bv
          : String(av).localeCompare(String(bv));
      return dir === "asc" ? cmp : -cmp;
    });
    values[field.name] = { ...t, order, sort: { column: key, dir } };
  }

  function toggleImageGrid(name: string, value: string, multi: boolean) {
    const g = values[name] as { selected: string[] };
    let selected = g.selected;
    if (multi) {
      selected = selected.includes(value)
        ? selected.filter((v) => v !== value)
        : [...selected, value];
    } else {
      selected = selected.includes(value) ? [] : [value];
    }
    values[name] = { ...g, selected };
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

  // --- validation ---------------------------------------------------------
  function isFieldComplete(f: Field): boolean {
    if (
      f.kind === "static_text" ||
      f.kind === "markdown" ||
      f.kind === "image" ||
      f.kind === "mermaid" ||
      f.kind === "wireframe"
    )
      return true;
    if (f.kind === "checkbox" || f.kind === "slider") return true;
    if (f.kind === "list" || f.kind === "tree") return true;
    if (f.kind === "table") {
      if (!("required" in f) || !f.required) return true;
      const v = values[f.name] as { selected: string[] };
      return v.selected.length > 0;
    }
    if (f.kind === "image_grid") {
      if (!("required" in f) || !f.required) return true;
      const v = values[f.name] as { selected: string[] };
      return v.selected.length > 0;
    }
    if (!("required" in f) || !f.required) return true;
    const v = values[(f as any).name];
    return v !== undefined && v !== null && String(v).length > 0;
  }

  let canSubmit = $derived(allFields.every(isFieldComplete));

  /** Find the first tab (index) containing an incomplete required field, or
   *  null when all tabs validate. Used to surface the first invalid tab in
   *  the validation hint. */
  function firstIncompleteTab(): { tabIndex: number; tabLabel: string } | null {
    if (!spec.tabs) return null;
    for (let i = 0; i < spec.tabs.length; i++) {
      if (!spec.tabs[i].fields.every(isFieldComplete)) {
        return { tabIndex: i, tabLabel: spec.tabs[i].label };
      }
    }
    return null;
  }

  // --- actions ------------------------------------------------------------
  let actions = $derived<Action[]>(
    spec.actions && spec.actions.length > 0
      ? spec.actions
      : [
          { label: spec.cancelLabel ?? $_("dialog.cancel"), value: "__cancel__", skip_validation: true },
          { label: spec.submitLabel ?? $_("dialog.submit"), value: "__submit__", primary: true },
        ]
  );

  function serialisableValues(): Record<string, any> {
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
    if (!a.skip_validation && !canSubmit) {
      // Surface the first invalid tab if we have tabs.
      const bad = firstIncompleteTab();
      if (bad) activeTab = bad.tabIndex;
      return;
    }
    onsubmit({
      action: a.value === "__submit__" ? null : a.value,
      values: serialisableValues(),
    });
  }

  // --- markdown rendering -------------------------------------------------
  // Configured once, used by all `markdown` fields. We keep it sync (no
  // remote includes, no async resolvers) so reactivity is straightforward.
  // Output is piped through DOMPurify before `{@html}` so an MCP caller
  // (potentially a compromised remote host reaching us through the
  // SSH-reverse-tunnel) cannot inject `<script>` or event handlers.
  // Issue #H-2 in v0.4.10 review.
  marked.setOptions({ gfm: true, breaks: true });
  function renderMd(src: string): string {
    let raw: string;
    try {
      raw = marked.parse(src, { async: false }) as string;
    } catch {
      raw = `<pre>${src.replace(/[<>&]/g, (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;" }[c]!))}</pre>`;
    }
    return DOMPurify.sanitize(raw, {
      // No <script>, no event handlers, no javascript: URLs, no <iframe>.
      // Keep links + basic markup. Defaults are conservative; we explicitly
      // forbid form-related tags to prevent autofill-driven exfiltration.
      FORBID_TAGS: ["script", "iframe", "form", "input", "button"],
      FORBID_ATTR: ["onerror", "onload", "onclick", "onmouseover", "onfocus"],
    });
  }
</script>

<div class="stack">
  {#if spec.header}<span class="chip">{spec.header}</span>{/if}
  <div>
    <p class="title">{spec.title}</p>
    {#if spec.description}<p class="subtitle">{spec.description}</p>{/if}
  </div>

  {#if spec.tabs && spec.tabs.length > 0}
    <div class="tab-bar" role="tablist">
      {#each spec.tabs as t, i (t.label)}
        <button
          type="button"
          role="tab"
          class="tab"
          class:active={activeTab === i}
          aria-selected={activeTab === i}
          onclick={() => (activeTab = i)}
        >
          {t.label}
        </button>
      {/each}
    </div>
  {/if}

  <div class="stack" style="gap: 12px;">
    {#each visibleFields as f}
      {#if f.kind === "static_text"}
        <div class="static-text {f.tone ?? 'info'}">{f.text}</div>
      {:else if f.kind === "markdown"}
        <div class="markdown-field">
          <!-- eslint-disable-next-line svelte/no-at-html-tags -->
          {@html renderMd(f.text)}
        </div>
      {:else if f.kind === "image"}
        <figure class="image-field" style={f.max_height ? `max-height: ${f.max_height}px` : ""}>
          <img src={f.src} alt={f.alt ?? f.label ?? ""} />
          {#if f.label}<figcaption>{f.label}</figcaption>{/if}
        </figure>
      {:else if f.kind === "mermaid"}
        <MermaidView source={f.source} label={f.label} max_height={f.max_height} />
      {:else if f.kind === "wireframe"}
        <WireframeView
          panels={f.panels}
          columns={f.columns}
          gap={f.gap}
          label={f.label}
          max_height={f.max_height}
        />
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
              {@const item = listItems(f).find((x: ListItem) => x.value === itemValue)}
              {#if item}
                <div
                  class="list-item"
                  class:selected={f.selectable && listValue.selected.includes(item.value)}
                  class:clickable={f.selectable}
                  class:has-thumbnail={!!item.thumbnail}
                  draggable={f.sortable}
                  ondragstart={(e) => {
                    if (!f.sortable) return;
                    dragFrom = { name: f.name, idx };
                    if (e.dataTransfer) {
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
                  {#if item.thumbnail}
                    <img class="list-thumb" src={item.thumbnail} alt="" />
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
      {:else if f.kind === "image_grid"}
        {@const gridValue = values[f.name] as { selected: string[] }}
        <div>
          {#if f.label}<label>{f.label}{f.required ? " *" : ""}</label>{/if}
          <div class="image-grid" style={`grid-template-columns: repeat(${f.columns ?? 3}, 1fr)`}>
            {#each f.images as img (img.value)}
              <button
                type="button"
                class="image-cell"
                class:selected={gridValue.selected.includes(img.value)}
                onclick={() => toggleImageGrid(f.name, img.value, !!f.multi_select)}
              >
                <img src={img.src} alt={img.label ?? ""} />
                {#if img.label}<span class="image-cell-label">{img.label}</span>{/if}
                {#if gridValue.selected.includes(img.value)}<span class="image-cell-check">✓</span>{/if}
              </button>
            {/each}
          </div>
        </div>
      {:else if f.kind === "table"}
        {@const tableValue = values[f.name] as {
          selected: string[];
          order: string[];
          sort: { column: string | null; dir: "asc" | "desc" };
        }}
        <div>
          {#if f.label}<label>{f.label}{f.required ? " *" : ""}</label>{/if}
          <div class="table-wrap">
            <table class="data-table">
              <thead>
                <tr>
                  {#if f.multi_select}<th class="row-pick" aria-label="select"></th>{/if}
                  {#each f.columns as col}
                    <th
                      class="col-head {col.align ?? 'left'}"
                      class:sortable={f.sortable_by_column}
                      onclick={() => f.sortable_by_column && sortTableBy(f, col.key)}
                    >
                      {col.label}
                      {#if f.sortable_by_column && tableValue.sort.column === col.key}
                        <span class="sort-marker">{tableValue.sort.dir === "asc" ? "▲" : "▼"}</span>
                      {/if}
                    </th>
                  {/each}
                </tr>
              </thead>
              <tbody>
                {#each tableValue.order as rowValue (rowValue)}
                  {@const row = f.rows.find((r) => r.value === rowValue)}
                  {#if row}
                    <tr
                      class:selected={tableValue.selected.includes(row.value)}
                      onclick={() => toggleTableRow(f.name, row.value, !!f.multi_select)}
                    >
                      {#if f.multi_select}
                        <td class="row-pick">
                          <span class="check" class:on={tableValue.selected.includes(row.value)}>
                            {#if tableValue.selected.includes(row.value)}✓{/if}
                          </span>
                        </td>
                      {/if}
                      {#each f.columns as col}
                        <td class={col.align ?? "left"}>{row.values[col.key] ?? ""}</td>
                      {/each}
                    </tr>
                  {/if}
                {/each}
              </tbody>
            </table>
          </div>
        </div>
      {:else}
        <div>
          <!-- Most field kinds want a label *above* the input. Checkbox is
            the exception: the standard checkbox layout pairs the label
            inline next to the box, and rendering an additional outer
            label produces a visible duplicate (seen in user testing of
            the v0.4.11 demo). Skip the outer label for checkbox. -->
          {#if f.kind !== "checkbox"}
            <label>{f.label}{"required" in f && f.required ? " *" : ""}</label>
          {/if}
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
          {:else if f.kind === "datetime"}
            <input type="datetime-local" bind:value={values[f.name]} />
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

  <!-- Spacer reserves footer height inside the scroll flow so the
       last form field never gets overlapped by the sticky footer at
       scroll-end. Belongs to this component, not to .container —
       Settings has its own footer-vs-content geometry. v0.4.35. -->
  <div class="form-footer-spacer" aria-hidden="true"></div>
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

  /* --- markdown --- */
  .markdown-field {
    padding: 10px 12px;
    border-radius: 8px;
    border: 1px solid var(--border);
    background: var(--surface);
    font-size: 13px;
    line-height: 1.55;
    color: var(--fg);
  }
  .markdown-field :global(p) { margin: 0 0 8px 0; }
  .markdown-field :global(p:last-child) { margin-bottom: 0; }
  .markdown-field :global(h1),
  .markdown-field :global(h2),
  .markdown-field :global(h3) { margin: 6px 0 4px; font-size: 14px; }
  .markdown-field :global(ul),
  .markdown-field :global(ol) { margin: 6px 0; padding-left: 20px; }
  .markdown-field :global(li) { margin: 2px 0; }
  .markdown-field :global(code) {
    background: color-mix(in srgb, var(--fg) 8%, transparent);
    padding: 1px 5px;
    border-radius: 4px;
    font-size: 12.5px;
  }
  .markdown-field :global(pre) {
    background: color-mix(in srgb, var(--fg) 8%, transparent);
    padding: 8px 10px;
    border-radius: 6px;
    overflow-x: auto;
    font-size: 12.5px;
  }
  .markdown-field :global(pre code) { background: transparent; padding: 0; }
  .markdown-field :global(a) { color: var(--accent); }
  .markdown-field :global(table) { border-collapse: collapse; width: 100%; margin: 6px 0; }
  .markdown-field :global(th),
  .markdown-field :global(td) { border: 1px solid var(--border); padding: 4px 8px; font-size: 12.5px; }

  /* --- image --- */
  .image-field {
    margin: 0;
    border-radius: 8px;
    overflow: hidden;
    border: 1px solid var(--border);
    background: var(--surface);
  }
  .image-field img {
    display: block;
    width: 100%;
    height: auto;
    object-fit: contain;
  }
  .image-field figcaption {
    padding: 6px 10px;
    font-size: 12px;
    color: var(--muted);
    border-top: 1px solid var(--border);
    text-align: center;
  }

  /* --- image grid --- */
  .image-grid {
    display: grid;
    gap: 8px;
    margin-top: 4px;
  }
  .image-cell {
    position: relative;
    border: 2px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    cursor: pointer;
    padding: 0;
    overflow: hidden;
    transition: border-color 0.12s, transform 0.08s;
  }
  .image-cell:hover { border-color: var(--accent); }
  .image-cell.selected { border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 25%, transparent); }
  .image-cell img {
    display: block;
    width: 100%;
    aspect-ratio: 1 / 1;
    object-fit: cover;
  }
  .image-cell-label {
    display: block;
    padding: 4px 6px;
    font-size: 11.5px;
    color: var(--muted);
    text-align: center;
    border-top: 1px solid var(--border);
  }
  .image-cell-check {
    position: absolute;
    top: 6px;
    right: 6px;
    background: var(--accent);
    color: var(--accent-fg);
    font-size: 12px;
    width: 22px;
    height: 22px;
    line-height: 22px;
    border-radius: 50%;
    text-align: center;
    box-shadow: 0 1px 2px rgba(0,0,0,0.25);
  }

  /* --- list --- */
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
  .list-item.has-thumbnail { padding: 6px 10px; }
  .list-item.clickable { cursor: pointer; }
  .list-item.clickable:hover { border-color: var(--accent); }
  .list-item.selected {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, var(--surface));
  }
  .list-thumb {
    width: 36px;
    height: 36px;
    border-radius: 4px;
    object-fit: cover;
    flex-shrink: 0;
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

  /* --- table --- */
  .table-wrap {
    margin-top: 4px;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
    background: var(--surface);
  }
  .data-table {
    border-collapse: collapse;
    width: 100%;
    font-size: 13px;
  }
  .data-table th,
  .data-table td {
    padding: 6px 10px;
    text-align: left;
    border-bottom: 1px solid var(--border);
  }
  .data-table th.right,
  .data-table td.right { text-align: right; }
  .data-table th.center,
  .data-table td.center { text-align: center; }
  .data-table thead th {
    background: color-mix(in srgb, var(--fg) 5%, transparent);
    font-weight: 500;
    font-size: 12px;
    color: var(--muted);
    user-select: none;
  }
  .data-table th.sortable { cursor: pointer; }
  .data-table th.sortable:hover { color: var(--fg); }
  .sort-marker { font-size: 10px; margin-left: 4px; opacity: 0.7; }
  .data-table tbody tr { cursor: pointer; transition: background 0.12s; }
  .data-table tbody tr:hover { background: color-mix(in srgb, var(--accent) 5%, transparent); }
  .data-table tbody tr.selected { background: color-mix(in srgb, var(--accent) 12%, transparent); }
  .data-table .row-pick {
    width: 28px;
    text-align: center;
    padding: 4px 6px;
  }

  /* --- tabs --- */
  .tab-bar {
    display: flex;
    gap: 2px;
    border-bottom: 1px solid var(--border);
    margin-top: -2px;
  }
  .tab {
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    padding: 8px 14px;
    font: inherit;
    color: var(--muted);
    cursor: pointer;
    font-size: 13px;
    border-radius: 0;
    box-shadow: none;
    transition: color 0.12s, border-color 0.12s;
  }
  .tab:hover { color: var(--fg); }
  .tab.active {
    color: var(--accent);
    border-bottom-color: var(--accent);
    background: transparent;
  }
</style>
