<script lang="ts">
  import { tick } from "svelte";
  import { _ } from "svelte-i18n";
  import { renderMarkdown } from "../markdown";
  import { handleContentClick } from "../external-link";
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
    type TableValue,
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
        <div class="annimg" class:invalid={fieldInvalid(f)}>
          {#if f.label}<label>{f.label}{f.required ? " *" : ""}</label>{/if}
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
        <div>
          {#if f.label}<label>{f.label}{f.required ? " *" : ""}</label>{/if}
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
                      onclick={() => f.sortable_by_column && sortTable(f, col.key)}
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
        {@const bad = fieldInvalid(f) ? "true" : undefined}
        <div class:invalid={!!bad}>
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
              <textarea placeholder={f.placeholder ?? ""} bind:value={values[f.name]} rows="4" aria-invalid={bad}></textarea>
            {:else}
              <input type="text" placeholder={f.placeholder ?? ""} bind:value={values[f.name]} aria-invalid={bad} />
            {/if}
          {:else if f.kind === "password"}
            <input type="password" placeholder={f.placeholder ?? ""} bind:value={values[f.name]} autocomplete="off" aria-invalid={bad} />
          {:else if f.kind === "secret"}
            <input type="password" placeholder={f.placeholder ?? ""} bind:value={values[f.name]} autocomplete="off" spellcheck="false" aria-invalid={bad} />
          {:else if f.kind === "number"}
            <input type="number" min={f.min} max={f.max} step={f.step ?? 1} bind:value={values[f.name]} aria-invalid={bad} />
          {:else if f.kind === "select"}
            <select bind:value={values[f.name]} aria-invalid={bad}>
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
              <input type="range" min={f.min} max={f.max} step={f.step ?? 1} bind:value={values[f.name]} style="flex: 1;" aria-invalid={bad} />
              <code>{values[f.name]}</code>
            </div>
          {:else if f.kind === "date"}
            <input type="date" bind:value={values[f.name]} aria-invalid={bad} />
          {:else if f.kind === "datetime"}
            <input type="datetime-local" bind:value={values[f.name]} aria-invalid={bad} />
          {:else if f.kind === "date_range"}
            <div class="row">
              <input type="date" bind:value={values[f.name].from} style="flex: 1;" aria-invalid={bad} />
              <span style="color: var(--muted); font-size: 12px;">—</span>
              <input type="date" bind:value={values[f.name].to} style="flex: 1;" aria-invalid={bad} />
            </div>
          {:else if f.kind === "color"}
            <div class="row">
              <input type="color" bind:value={values[f.name]} style="width: 50px; height: 34px; padding: 2px;" />
              <code>{values[f.name]}</code>
            </div>
          {/if}
          {#if "target" in f && f.target}
            <!-- Issue #135: show the user *where* this value will be written
                 before they approve by submitting. The affirmative button IS
                 the per-operation approval. -->
            <p class="write-target">
              <span class="wt-icon" aria-hidden="true">↳</span>
              {f.kind === "secret" ? "Wird geschrieben (nicht an den Agent zurück):" : "Wird zusätzlich geschrieben:"}
              <!-- #199: `resolved_path` is the destination the writing host
                   (this app locally, the bridge for a remote session) will
                   actually write, with `~/` expanded and symlinks followed.
                   An older bridge doesn't send it; `path` is absolute or
                   `~/`-rooted either way, so the fallback still identifies
                   the file. -->
              <code>{f.target.resolved_path ?? f.target.path}</code>
              <span class="wt-meta"
                >mode: {f.target.mode}{f.target.perm ? `, ${f.target.perm}` : ""}{f.target.overwrite
                  ? ", overwrite"
                  : ""}</span>
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
              Wird nicht an den Agent zurückgegeben und nirgends gespeichert.
            </p>
          {/if}
        </div>
      {/if}
    {/each}
  </div>

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
  .write-target {
    margin: 4px 0 0;
    font-size: 12px;
    color: var(--muted);
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 6px;
  }
  .write-target code {
    font-size: 11px;
    word-break: break-all;
  }
  .write-target .wt-icon { color: var(--accent); }
  .write-target .wt-meta { color: var(--muted); opacity: 0.8; }

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
