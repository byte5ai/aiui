<script lang="ts">
  import { tick } from "svelte";
  import { _ } from "svelte-i18n";
  import { renderMarkdown } from "../markdown";
  import { handleContentClick } from "../external-link";
  import { onActivate, reorderTarget } from "../a11y";
  import TreeNode from "./TreeNode.svelte";
  import MermaidView from "./MermaidView.svelte";
  import WireframeView from "./WireframeView.svelte";
  // #206: the value logic (defaults, validation, serialisation) lives in a
  // plain module so it can be unit-tested; this component only renders it.
  import {
    firstIncompleteTab,
    incompleteFields,
    initialValue,
    isFieldComplete,
    listItems,
    serialisableValues,
    sortTableBy,
    valueFields,
    type Action,
    type AnnField,
    type AnnValue,
    type Field,
    type ListItem,
    type Tab,
    type TableRow,
    type TableValue,
    type WriteTarget,
  } from "./form-values";

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
    /** Origin host of a bridge-served session (#207). Set → the `target`
     *  write happens on THAT host, so the approval line must show the raw
     *  `~/`-form qualified with the host, never this Mac's resolved `$HOME`. */
    sessionOrigin?: string;
    /** field name → absolute path, resolved by Rust for a LOCAL session
     *  (#207). Empty for a bridge-served session. */
    resolvedTargets?: Record<string, string>;
  }

  let {
    spec,
    onsubmit,
    oncancel,
    sessionOrigin = undefined,
    resolvedTargets = {},
  }: Props = $props();

  /** What the approval line shows as the destination. Local: the absolute
   *  path Rust resolved (`docs/skill.md` promises the user sees it). Remote:
   *  the raw agent-supplied form, qualified with the origin host in the
   *  markup — expanding `~` here would name the wrong machine's home. */
  function targetPath(name: string, target: WriteTarget): string {
    if (sessionOrigin) return target.path;
    return resolvedTargets[name] ?? target.path;
  }

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

  // Screen-reader announcement for a keyboard reorder. Drag-and-drop is
  // silent by nature; a keyboard user needs to hear where the item landed
  // (#207). Rendered into a visually-hidden `aria-live="polite"` region.
  let reorderAnnouncement = $state("");

  /**
   * Alt/Cmd + ↑/↓ on a sortable list item. Moves the item and keeps focus on
   * it: Svelte's keyed `{#each}` moves the DOM node itself, and a moved node
   * loses focus, so we re-focus explicitly once the DOM has settled.
   */
  function onListKeydown(
    e: KeyboardEvent,
    f: Extract<Field, { kind: "list" }>,
    item: ListItem,
    idx: number,
  ) {
    if (f.sortable) {
      const to = reorderTarget(e, idx);
      if (to !== null) {
        const list = values[f.name] as { selected: string[]; order: string[] };
        if (to < 0 || to >= list.order.length) return;
        const total = list.order.length;
        const el = e.currentTarget as HTMLElement;
        moveItem(f.name, idx, to);
        reorderAnnouncement = $_("dialog.list.moved", {
          values: { label: item.label, pos: to + 1, total },
        });
        requestAnimationFrame(() => el.focus());
        return;
      }
    }
    if (f.selectable) {
      onActivate(e, () => toggleListItem(f.name, item.value, !!f.multi_select));
    }
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

  function sortTable(field: Extract<Field, { kind: "table" }>, key: string) {
    values[field.name] = sortTableBy(field, key, values[field.name] as TableValue);
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

  // --- annotated image ----------------------------------------------------
  // A single point marker and/or a rectangular region, both stored as
  // normalized 0..1 coordinates relative to the *displayed* image (which,
  // because the overlay exactly covers the <img>, are also fractions of the
  // natural image — resolution-independent). `mode` decides which gestures
  // are available; in "both" the user flips an explicit Point/Region tool so
  // the gesture is never ambiguous.
  //
  // Below this drag distance (in normalized units) a region gesture counts as
  // a stray click and is discarded rather than committing a degenerate rect.
  const ANN_MIN_REGION = 0.01;

  let annTool = $state<Record<string, "point" | "region">>({});
  let annDrag = $state<{
    name: string;
    tool: "point" | "region";
    startX: number;
    startY: number;
    prevRegion: AnnValue["region"];
  } | null>(null);

  function annActiveTool(f: AnnField): "point" | "region" {
    if (f.mode === "region") return "region";
    if (f.mode !== "both") return "point";
    return annTool[f.name] ?? "point";
  }

  function annSetTool(f: AnnField, tool: "point" | "region") {
    annTool = { ...annTool, [f.name]: tool };
  }

  function annNormFromEvent(e: PointerEvent, stage: HTMLElement): { x: number; y: number } {
    const rect = stage.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return { x: 0, y: 0 };
    const x = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
    const y = Math.min(1, Math.max(0, (e.clientY - rect.top) / rect.height));
    return { x: round4(x), y: round4(y) };
  }

  function round4(n: number): number {
    return Math.round(n * 1e4) / 1e4;
  }

  function annOnImageLoad(name: string, img: HTMLImageElement) {
    if (!img.naturalWidth || !img.naturalHeight) return;
    const v = values[name] as AnnValue;
    values[name] = { ...v, natural: { width: img.naturalWidth, height: img.naturalHeight } };
  }

  function annPointerDown(f: AnnField, e: PointerEvent, stage: HTMLElement) {
    // Only react to the primary (left / touch / pen) button.
    if (e.button !== 0) return;
    e.preventDefault();
    stage.setPointerCapture?.(e.pointerId);
    const tool = annActiveTool(f);
    const p = annNormFromEvent(e, stage);
    const v = values[f.name] as AnnValue;
    annDrag = { name: f.name, tool, startX: p.x, startY: p.y, prevRegion: v.region };
    if (tool === "point") {
      values[f.name] = { ...v, point: p };
    } else {
      // Start a zero-size region; it grows on move.
      values[f.name] = { ...v, region: { x: p.x, y: p.y, w: 0, h: 0 } };
    }
  }

  function annPointerMove(f: AnnField, e: PointerEvent, stage: HTMLElement) {
    if (!annDrag || annDrag.name !== f.name) return;
    e.preventDefault();
    const p = annNormFromEvent(e, stage);
    const v = values[f.name] as AnnValue;
    if (annDrag.tool === "point") {
      values[f.name] = { ...v, point: p };
    } else {
      const x = Math.min(annDrag.startX, p.x);
      const y = Math.min(annDrag.startY, p.y);
      const w = Math.abs(p.x - annDrag.startX);
      const h = Math.abs(p.y - annDrag.startY);
      values[f.name] = {
        ...v,
        region: { x: round4(x), y: round4(y), w: round4(w), h: round4(h) },
      };
    }
  }

  function annPointerUp(f: AnnField, e: PointerEvent, stage: HTMLElement) {
    if (!annDrag || annDrag.name !== f.name) return;
    e.preventDefault();
    stage.releasePointerCapture?.(e.pointerId);
    const drag = annDrag;
    annDrag = null;
    if (drag.tool === "region") {
      const v = values[f.name] as AnnValue;
      const r = v.region;
      // Discard a degenerate drag (a stray click) — restore what was there.
      if (!r || r.w < ANN_MIN_REGION || r.h < ANN_MIN_REGION) {
        values[f.name] = { ...v, region: drag.prevRegion };
      }
    }
  }

  function annClear(f: AnnField) {
    const v = values[f.name] as AnnValue;
    values[f.name] = { ...v, point: null, region: null };
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
  // #206: validation is *visible*, not decorative. The affirmative buttons
  // stay enabled — a disabled button fires no click, so the tab jump
  // docs/skill.md promises could never run and the user got no hint of what
  // was missing or where. Pressing one now switches to the offending tab,
  // shows a footer message and marks the offending controls instead.
  let invalid = $derived(incompleteFields(allFields, values));
  let canSubmit = $derived(invalid.length === 0);
  let badTab = $derived(firstIncompleteTab(spec.tabs, values));
  /** Set only after a failed submit attempt — an untouched form shows no red. */
  let showErrors = $state(false);
  let shellEl = $state<HTMLElement | null>(null);

  function fieldInvalid(f: Field): boolean {
    return showErrors && !isFieldComplete(f, values);
  }

  async function focusFirstInvalid() {
    await tick();
    const el = shellEl?.querySelector<HTMLElement>('[aria-invalid="true"]');
    el?.focus?.();
    el?.scrollIntoView?.({ block: "nearest" });
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

  function runAction(a: Action) {
    if (a.value === "__cancel__") {
      oncancel();
      return;
    }
    if (!a.skip_validation && !canSubmit) {
      showErrors = true;
      // Surface the first invalid tab if we have tabs.
      if (badTab) activeTab = badTab.tabIndex;
      void focusFirstInvalid();
      return;
    }
    onsubmit({
      action: a.value === "__submit__" ? null : a.value,
      values: serialisableValues(values),
    });
  }

  // --- markdown rendering -------------------------------------------------
  // Shared renderer — see ../markdown.ts for the sanitization rationale.
</script>

<!-- The body of a list item, shared by the operable and the static variant so
     the two only differ in their ARIA/handler surface, never in content. -->
{#snippet listItemBody(
  f: Extract<Field, { kind: "list" }>,
  item: ListItem,
  listValue: { selected: string[]; order: string[] },
)}
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
{/snippet}

<!-- The data cells of a table row, shared by the multi-select variant (which
     carries a real checkbox) and the single-select variant (a focusable row
     button). -->
{#snippet tableRowCells(f: Extract<Field, { kind: "table" }>, row: TableRow)}
  {#each f.columns as col}
    <td class={col.align ?? "left"}>{row.values[col.key] ?? ""}</td>
  {/each}
{/snippet}

<main class="window-shell" bind:this={shellEl}>
  <div class="window-scroll">
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
        <!-- #189: links inside agent-supplied markdown open in the user's
             browser, never by navigating this window away. Delegated, because
             the content is {@html} and its anchors have no Svelte lifecycle.
             Not interactive itself — the handler only acts on a real <a>. -->
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="markdown-field" onclick={handleContentClick}>
          <!-- eslint-disable-next-line svelte/no-at-html-tags -->
          {@html renderMarkdown(f.text)}
        </div>
      {:else if f.kind === "image"}
        <figure class="image-field" style={f.max_height ? `max-height: ${f.max_height}px` : ""}>
          <img src={f.src} alt={f.alt ?? f.label ?? ""} />
          {#if f.label}<figcaption>{f.label}</figcaption>{/if}
        </figure>
      {:else if f.kind === "annotated_image"}
        {@const ann = values[f.name] as AnnValue}
        {@const annMode = f.mode ?? "point"}
        <!-- Group labels are `<span id=…>` + `aria-labelledby`, not `<label>`:
             there is no single control to bind a `<label for>` to, and an
             unbound `<label>` is announced as orphan text (#207). -->
        <div
          class="annimg"
          role="group"
          class:invalid={fieldInvalid(f)}
          aria-labelledby={f.label ? `lbl-${f.name}` : undefined}
        >
          {#if f.label}<span class="group-label" id={`lbl-${f.name}`}>{f.label}{f.required ? " *" : ""}</span>{/if}
          <div class="annimg-toolbar">
            {#if annMode === "both"}
              <div class="annimg-tools" role="group" aria-label="Annotation tool">
                <button
                  type="button"
                  class="annimg-tool"
                  class:active={annActiveTool(f) === "point"}
                  onclick={() => annSetTool(f, "point")}>● Point</button>
                <button
                  type="button"
                  class="annimg-tool"
                  class:active={annActiveTool(f) === "region"}
                  onclick={() => annSetTool(f, "region")}>▭ Region</button>
              </div>
            {:else}
              <span class="annimg-hint">
                {annMode === "region" ? "Drag to mark a region" : "Click to mark a point"}
              </span>
            {/if}
            <button
              type="button"
              class="annimg-clear"
              disabled={!ann.point && !ann.region}
              onclick={() => annClear(f)}>Clear</button>
          </div>
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="annimg-stage"
            class:region-tool={annActiveTool(f) === "region"}
            aria-invalid={fieldInvalid(f) ? "true" : undefined}
            tabindex="-1"
            onpointerdown={(e) => annPointerDown(f, e, e.currentTarget as HTMLElement)}
            onpointermove={(e) => annPointerMove(f, e, e.currentTarget as HTMLElement)}
            onpointerup={(e) => annPointerUp(f, e, e.currentTarget as HTMLElement)}
            onpointercancel={(e) => annPointerUp(f, e, e.currentTarget as HTMLElement)}
          >
            <img
              src={f.src}
              alt={f.alt ?? f.label ?? ""}
              draggable="false"
              style={f.max_height ? `max-height: ${f.max_height}px` : ""}
              onload={(e) => annOnImageLoad(f.name, e.currentTarget as HTMLImageElement)}
            />
            <svg
              class="annimg-overlay"
              viewBox="0 0 100 100"
              preserveAspectRatio="none"
              aria-hidden="true"
            >
              {#if ann.region}
                <rect
                  class="ann-region"
                  x={ann.region.x * 100}
                  y={ann.region.y * 100}
                  width={ann.region.w * 100}
                  height={ann.region.h * 100}
                />
              {/if}
              {#if ann.point}
                <line class="ann-cross" x1={ann.point.x * 100} y1="0" x2={ann.point.x * 100} y2="100" />
                <line class="ann-cross" x1="0" y1={ann.point.y * 100} x2="100" y2={ann.point.y * 100} />
                <circle class="ann-point" cx={ann.point.x * 100} cy={ann.point.y * 100} r="1.6" />
              {/if}
            </svg>
          </div>
          <div class="annimg-readout">
            {#if ann.point}
              <code>point {ann.point.x.toFixed(3)}, {ann.point.y.toFixed(3)}</code>
            {/if}
            {#if ann.region}
              <code
                >region {ann.region.x.toFixed(3)}, {ann.region.y.toFixed(3)} · {ann.region.w.toFixed(3)}×{ann.region.h.toFixed(3)}</code>
            {/if}
            {#if !ann.point && !ann.region}
              <span class="annimg-empty">No annotation yet</span>
            {/if}
          </div>
        </div>
      {:else if f.kind === "audio"}
        <figure class="audio-field">
          {#if f.label}<figcaption>{f.label}</figcaption>{/if}
          <!-- svelte-ignore a11y_media_has_caption -->
          <audio src={f.src} controls preload="metadata"></audio>
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
        <div role="group" aria-labelledby={f.label ? `lbl-${f.name}` : undefined}>
          {#if f.label}<span class="group-label" id={`lbl-${f.name}`}>{f.label}</span>{/if}
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
        <div role="group" aria-labelledby={f.label ? `lbl-${f.name}` : undefined}>
          {#if f.label}<span class="group-label" id={`lbl-${f.name}`}>{f.label}</span>{/if}
          <div class="list-widget" class:sortable={f.sortable}>
            {#each listValue.order as itemValue, idx (itemValue)}
              {@const item = listItems(f).find((x: ListItem) => x.value === itemValue)}
              {#if item}
                {#if f.selectable || f.sortable}
                  <!-- Operable item: a STATIC role/tabindex (not the previous
                       dynamic expression, which defeated the compiler's
                       interactivity check) plus a real key handler, so Enter /
                       Space select and Alt+↑/↓ reorder. A sortable-only item
                       used to be `tabindex=-1` — unreachable by keyboard
                       entirely, with no way to answer at all (#207). -->
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
                    onkeydown={(e) => onListKeydown(e, f, item, idx)}
                    role="button"
                    tabindex="0"
                    aria-pressed={f.selectable
                      ? listValue.selected.includes(item.value)
                      : undefined}
                    aria-keyshortcuts={f.sortable ? "Alt+ArrowUp Alt+ArrowDown" : undefined}
                  >
                    {@render listItemBody(f, item, listValue)}
                  </div>
                {:else}
                  <div class="list-item" class:has-thumbnail={!!item.thumbnail}>
                    {@render listItemBody(f, item, listValue)}
                  </div>
                {/if}
              {/if}
            {/each}
          </div>
        </div>
      {:else if f.kind === "image_grid"}
        {@const gridValue = values[f.name] as { selected: string[] }}
        <div role="group" aria-labelledby={f.label ? `lbl-${f.name}` : undefined}>
          {#if f.label}<span class="group-label" id={`lbl-${f.name}`}>{f.label}{f.required ? " *" : ""}</span>{/if}
          <div
            class="image-grid"
            class:invalid={fieldInvalid(f)}
            aria-invalid={fieldInvalid(f) ? "true" : undefined}
            tabindex="-1"
            style={`grid-template-columns: repeat(${f.columns ?? 3}, 1fr)`}
          >

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
        <div role="group" aria-labelledby={f.label ? `lbl-${f.name}` : undefined}>
          {#if f.label}<span class="group-label" id={`lbl-${f.name}`}>{f.label}{f.required ? " *" : ""}</span>{/if}
          <div
            class="table-wrap"
            class:invalid={fieldInvalid(f)}
            aria-invalid={fieldInvalid(f) ? "true" : undefined}
            tabindex="-1"
          >

            <table class="data-table">
              <thead>
                <tr>
                  {#if f.multi_select}<th class="row-pick" aria-label="select"></th>{/if}
                  {#each f.columns as col}
                    <th
                      class="col-head {col.align ?? 'left'}"
                      class:sortable={f.sortable_by_column}
                      aria-sort={f.sortable_by_column && tableValue.sort.column === col.key
                        ? tableValue.sort.dir === "asc"
                          ? "ascending"
                          : "descending"
                        : undefined}
                    >
                      {#if f.sortable_by_column}
                        <!-- A real `<button>` rather than a click handler on the
                             `<th>`: sorting is then reachable by Tab+Enter and
                             announced as a control (#207). -->
                        <button type="button" class="col-sort" onclick={() => sortTable(f, col.key)}>
                          {col.label}
                          {#if tableValue.sort.column === col.key}
                            <span class="sort-marker">{tableValue.sort.dir === "asc" ? "▲" : "▼"}</span>
                          {/if}
                        </button>
                      {:else}
                        {col.label}
                      {/if}
                    </th>
                  {/each}
                </tr>
              </thead>
              <tbody>
                {#each tableValue.order as rowValue (rowValue)}
                  {@const row = f.rows.find((r) => r.value === rowValue)}
                  {#if row}
                    {#if f.multi_select}
                      <!-- Multi-select rows get a REAL checkbox rather than an
                           ARIA-flavoured `<tr>`: the native control is the
                           keyboard and screen-reader path, and the row click
                           stays as a mouse convenience that forwards to it
                           (#207). A full ARIA grid would be far more focus
                           machinery than a selection list with columns needs. -->
                      <tr
                        class:selected={tableValue.selected.includes(row.value)}
                        onclick={() => toggleTableRow(f.name, row.value, true)}
                      >
                        <td class="row-pick">
                          <input
                            type="checkbox"
                            checked={tableValue.selected.includes(row.value)}
                            aria-label={String(row.values[f.columns[0]?.key ?? ""] ?? row.value)}
                            onclick={(e) => e.stopPropagation()}
                            onchange={() => toggleTableRow(f.name, row.value, true)}
                          />
                        </td>
                        {@render tableRowCells(f, row)}
                      </tr>
                    {:else}
                      <tr
                        class:selected={tableValue.selected.includes(row.value)}
                        role="button"
                        tabindex="0"
                        aria-pressed={tableValue.selected.includes(row.value)}
                        onclick={() => toggleTableRow(f.name, row.value, false)}
                        onkeydown={(e) =>
                          onActivate(e, () => toggleTableRow(f.name, row.value, false))}
                      >
                        {@render tableRowCells(f, row)}
                      </tr>
                    {/if}
                  {/if}
                {/each}
              </tbody>
            </table>
          </div>
        </div>
      {:else}
        {@const bad = fieldInvalid(f) ? "true" : undefined}
        <div class:invalid={!!bad}>
          <!-- Most field kinds want a label *above* the input. Checkbox is
            the exception: the standard checkbox layout pairs the label
            inline next to the box, and rendering an additional outer
            label produces a visible duplicate (seen in user testing of
            the v0.4.11 demo). Skip the outer label for checkbox. -->
          {#if f.kind !== "checkbox"}
            <!-- `for`/`id` rather than a bare `<label>`: an unbound label is a
                 screen-reader orphan, and these kinds all have exactly one
                 control to bind to (#207). `date_range` binds the FROM input;
                 the TO input carries its own aria-label. -->
            <label for={`f-${f.name}`}>{f.label}{"required" in f && f.required ? " *" : ""}</label>
          {/if}
          {#if f.kind === "text"}
            {#if f.multiline}
              <textarea id={`f-${f.name}`} placeholder={f.placeholder ?? ""} bind:value={values[f.name]} rows="4" aria-invalid={bad}></textarea>
            {:else}
              <input id={`f-${f.name}`} type="text" placeholder={f.placeholder ?? ""} bind:value={values[f.name]} aria-invalid={bad} />
            {/if}
          {:else if f.kind === "password"}
            <input id={`f-${f.name}`} type="password" placeholder={f.placeholder ?? ""} bind:value={values[f.name]} autocomplete="off" aria-invalid={bad} />
          {:else if f.kind === "secret"}
            <input id={`f-${f.name}`} type="password" placeholder={f.placeholder ?? ""} bind:value={values[f.name]} autocomplete="off" spellcheck="false" aria-invalid={bad} />
          {:else if f.kind === "number"}
            <input id={`f-${f.name}`} type="number" min={f.min} max={f.max} step={f.step ?? 1} bind:value={values[f.name]} aria-invalid={bad} />
          {:else if f.kind === "select"}
            <select id={`f-${f.name}`} bind:value={values[f.name]} aria-invalid={bad}>
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
              <input id={`f-${f.name}`} type="range" min={f.min} max={f.max} step={f.step ?? 1} bind:value={values[f.name]} style="flex: 1;" aria-invalid={bad} />
              <code>{values[f.name]}</code>
            </div>
          {:else if f.kind === "date"}
            <input id={`f-${f.name}`} type="date" bind:value={values[f.name]} aria-invalid={bad} />
          {:else if f.kind === "datetime"}
            <input id={`f-${f.name}`} type="datetime-local" bind:value={values[f.name]} aria-invalid={bad} />
          {:else if f.kind === "date_range"}
            <div class="row">
              <input id={`f-${f.name}`} type="date" bind:value={values[f.name].from} style="flex: 1;" aria-invalid={bad} />
              <span style="color: var(--muted); font-size: 12px;">—</span>
              <input
                type="date"
                bind:value={values[f.name].to}
                style="flex: 1;"
                aria-invalid={bad}
                aria-label={$_("dialog.date_range.to", { values: { label: f.label } })}
              />
            </div>
          {:else if f.kind === "color"}
            <div class="row">
              <input id={`f-${f.name}`} type="color" bind:value={values[f.name]} style="width: 50px; height: 34px; padding: 2px;" />
              <code>{values[f.name]}</code>
            </div>
          {/if}
          {#if "target" in f && f.target}
            <!-- Issue #135: show the user *where* this value will be written
                 before they approve by submitting. The affirmative button IS
                 the per-operation approval. -->
            <!-- #207: this line WAS hardcoded German. It is the entire
                 authorization surface for an arbitrary file write, so a user
                 outside `de` was approving a write they could not read. It
                 also showed the raw `~/`-form; for a local session it now
                 shows the path Rust resolved, as `docs/skill.md` promises,
                 and for a bridge-served session the raw form qualified with
                 the host that will actually be written to. -->
            <p class="write-target">
              <span class="wt-icon" aria-hidden="true">↳</span>
              {$_(
                f.kind === "secret"
                  ? "dialog.write_target.secret"
                  : "dialog.write_target.value",
              )}
              <code>{targetPath(f.name, f.target)}</code>
              {#if sessionOrigin}
                <span class="wt-host"
                  >{$_("dialog.write_target.on_host", { values: { host: sessionOrigin } })}</span>
              {/if}
              <span class="wt-meta"
                >{$_("dialog.write_target.meta", {
                  values: {
                    mode: f.target.mode,
                    perm: f.target.perm ? `, ${f.target.perm}` : "",
                    overwrite: f.target.overwrite
                      ? $_("dialog.write_target.overwrite_flag")
                      : "",
                  },
                })}</span>
            </p>
          {:else if f.kind === "secret"}
            <!-- Issue #186: the write-only promise belongs to the KIND, not to
                 the presence of a `target`. This note used to render only for
                 target-carrying fields, so a target-less `secret` looked and
                 behaved exactly like `password` while the docs told the user
                 it was write-only. A current companion rejects that shape in
                 validate_spec; this is the layer the user actually sees. -->
            <p class="write-target">
              <span class="wt-icon" aria-hidden="true">↳</span>
              {$_("dialog.write_target.secret_only")}
            </p>
          {/if}
        </div>
      {/if}
    {/each}
  </div>

  <!-- Keyboard reorders are otherwise silent: drag-and-drop has no
       announcement to borrow, so a screen-reader user gets no feedback that
       the item moved, or where to (#207). -->
  <p class="sr-only" role="status" aria-live="polite">{reorderAnnouncement}</p>

  </div><!-- /.window-scroll -->

  {#if showErrors && !canSubmit}
    <!-- #206: the only signal a user used to get for a failed submit was a
         greyed-out button and a " *" somewhere in the form — nothing at all
         when the missing field sat on another tab. -->
    <p class="form-error" role="alert">
      {badTab
        ? $_("dialog.validation.missing_on_tab", {
            values: { n: invalid.length, tab: badTab.tabLabel },
          })
        : $_("dialog.validation.missing", { values: { n: invalid.length } })}
    </p>
  {/if}

  <!-- Footer is now a real flex sibling of .window-scroll, not a sticky
       overlay. The form-footer-spacer hack from v0.4.35 (a fixed-height
       transparent block reserving sticky-footer overlap room) is gone:
       the footer can no longer overlap content because content lives in
       a separately-scrolling region above it. -->

  <footer class="window-footer">
    {#each actions as a}
      <button
        class:primary={a.primary}
        class:danger={a.destructive}
        class:success={a.success}
        onclick={() => runAction(a)}
      >
        {a.label}
      </button>
    {/each}
  </footer>
</main>

<style>
  /* --- validation (#206) --- */
  .form-error {
    flex: 0 0 auto;
    margin: 0;
    padding: 10px 20px 0;
    background: var(--bg);
    color: var(--danger);
    font-size: 12.5px;
    line-height: 1.4;
  }
  .invalid input,
  .invalid textarea,
  .invalid select {
    border-color: var(--danger);
  }
  .annimg.invalid .annimg-stage,
  .image-grid.invalid,
  .table-wrap.invalid {
    outline: 1px solid var(--danger);
    border-radius: 6px;
  }

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
  /* Group label for widgets that have no single control to bind a `<label>`
     to. Mirrors the global `label` rule in app.css. */
  .group-label {
    display: block;
    font-size: 11.5px;
    font-weight: 500;
    color: var(--muted);
    margin-bottom: 5px;
    letter-spacing: 0.01em;
  }

  /* Visually hidden, still announced — the reorder live region. */
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    margin: -1px;
    padding: 0;
    overflow: hidden;
    clip: rect(0 0 0 0);
    clip-path: inset(50%);
    white-space: nowrap;
    border: 0;
  }

  /* #207: this line is a security control — it is the only disclosure of an
     arbitrary file write — so it no longer hides in 12px `--muted`. Warning
     tone; the foreground is mixed against `--fg`, not `--warning-fg`, because
     the background here is a 18% wash rather than a solid amber fill — the
     same pairing the TTL/update banners use, ≥4.5:1 in both themes. */
  .write-target {
    margin: 6px 0 0;
    padding: 6px 8px;
    border-radius: 6px;
    font-size: 13px;
    color: color-mix(in srgb, var(--warning) 35%, var(--fg));
    background: color-mix(in srgb, var(--warning) 18%, var(--bg));
    border: 1px solid color-mix(in srgb, var(--warning) 45%, transparent);
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 6px;
  }
  .write-target code {
    font-size: 12.5px;
    word-break: break-all;
  }
  .write-target .wt-icon { color: inherit; }
  .write-target .wt-host { font-weight: 600; }
  .write-target .wt-meta { opacity: 0.85; }

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

  /* --- annotated image --- */
  .annimg { display: flex; flex-direction: column; gap: 6px; }
  .annimg-toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .annimg-hint { font-size: 12px; color: var(--muted); }
  .annimg-tools { display: inline-flex; gap: 0; border: 1px solid var(--border); border-radius: 7px; overflow: hidden; }
  .annimg-tool {
    background: var(--surface);
    border: none;
    border-radius: 0;
    box-shadow: none;
    padding: 4px 10px;
    font-size: 12px;
    color: var(--muted);
    cursor: pointer;
  }
  .annimg-tool + .annimg-tool { border-left: 1px solid var(--border); }
  .annimg-tool.active { background: var(--accent); color: var(--accent-fg); }
  .annimg-clear {
    margin-left: auto;
    padding: 4px 10px;
    font-size: 12px;
    border: 1px solid var(--border);
    border-radius: 7px;
    background: var(--surface);
    cursor: pointer;
  }
  .annimg-clear:disabled { opacity: 0.5; cursor: default; }
  .annimg-stage {
    position: relative;
    width: fit-content;
    max-width: 100%;
    border-radius: 8px;
    overflow: hidden;
    border: 1px solid var(--border);
    background: var(--surface);
    cursor: crosshair;
    touch-action: none;
    user-select: none;
  }
  .annimg-stage.region-tool { cursor: crosshair; }
  .annimg-stage img {
    display: block;
    max-width: 100%;
    height: auto;
    -webkit-user-drag: none;
    user-select: none;
  }
  .annimg-overlay {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }
  .ann-region {
    fill: color-mix(in srgb, var(--accent) 18%, transparent);
    stroke: var(--accent);
    stroke-width: 0.5;
    vector-effect: non-scaling-stroke;
  }
  .ann-cross {
    stroke: var(--accent);
    stroke-width: 1;
    stroke-dasharray: 2 2;
    vector-effect: non-scaling-stroke;
    opacity: 0.7;
  }
  .ann-point {
    fill: var(--accent);
    stroke: var(--accent-fg, #fff);
    stroke-width: 0.5;
    vector-effect: non-scaling-stroke;
  }
  .annimg-readout {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 12px;
    font-size: 11.5px;
    color: var(--muted);
  }
  .annimg-readout code { font-size: 11.5px; }
  .annimg-empty { font-size: 11.5px; color: var(--muted); }

  /* --- audio --- */
  .audio-field {
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .audio-field figcaption {
    font-size: 12px;
    color: var(--muted);
  }
  .audio-field audio {
    display: block;
    width: 100%;
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
  .data-table th.sortable { cursor: pointer; padding: 0; }
  .data-table th.sortable:hover { color: var(--fg); }
  .col-sort {
    display: block;
    width: 100%;
    background: transparent;
    border: none;
    border-radius: 0;
    box-shadow: none;
    padding: 6px 10px;
    font: inherit;
    color: inherit;
    text-align: inherit;
    cursor: pointer;
  }
  .col-sort:hover { color: var(--fg); }
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
