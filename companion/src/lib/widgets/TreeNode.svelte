<script lang="ts">
  import Self from "./TreeNode.svelte";

  interface TreeItem {
    label: string;
    value: string;
    description?: string;
    children?: TreeItem[];
  }

  interface Props {
    item: TreeItem;
    depth: number;
    selected: string[];
    expanded: Set<string>;
    multiSelect: boolean;
    onToggleExpand: (value: string) => void;
    onToggleSelect: (value: string) => void;
  }

  let {
    item,
    depth,
    selected,
    expanded,
    multiSelect,
    onToggleExpand,
    onToggleSelect,
  }: Props = $props();

  let hasChildren = $derived((item.children ?? []).length > 0);
  let isExpanded = $derived(expanded.has(item.value));
  let isSelected = $derived(selected.includes(item.value));
</script>

<div class="node" style={`padding-left: ${depth * 16}px`}>
  <div class="row" class:selected={isSelected}>
    {#if hasChildren}
      <button
        type="button"
        class="chev"
        onclick={() => onToggleExpand(item.value)}
        aria-label={isExpanded ? "collapse" : "expand"}>{isExpanded ? "▾" : "▸"}</button
      >
    {:else}
      <span class="chev spacer"></span>
    {/if}
    <button
      type="button"
      class="pick"
      onclick={() => onToggleSelect(item.value)}
    >
      {#if multiSelect}
        <span class="check" class:on={isSelected}>{isSelected ? "✓" : ""}</span>
      {/if}
      <span class="item-label">{item.label}</span>
      {#if item.description}<span class="item-desc">— {item.description}</span>{/if}
    </button>
  </div>
  {#if hasChildren && isExpanded}
    {#each item.children ?? [] as child (child.value)}
      <Self
        item={child}
        depth={depth + 1}
        {selected}
        {expanded}
        {multiSelect}
        {onToggleExpand}
        {onToggleSelect}
      />
    {/each}
  {/if}
</div>

<style>
  .node {
    display: flex;
    flex-direction: column;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 6px;
    border-radius: 6px;
    transition: background 0.1s;
  }
  .row.selected {
    background: color-mix(in srgb, var(--accent) 15%, transparent);
  }
  .row:hover {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .chev {
    width: 18px;
    height: 18px;
    font-size: 11px;
    border: none;
    background: transparent;
    color: var(--muted);
    padding: 0;
    cursor: pointer;
    flex-shrink: 0;
  }
  .chev.spacer {
    visibility: hidden;
  }
  .pick {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    background: transparent;
    border: none;
    padding: 2px 4px;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .check {
    width: 16px;
    height: 16px;
    border: 1px solid var(--border);
    border-radius: 3px;
    display: inline-flex;
    justify-content: center;
    align-items: center;
    font-size: 11px;
    flex-shrink: 0;
  }
  .check.on {
    background: var(--accent);
    color: var(--accent-fg);
    border-color: var(--accent);
  }
  .item-label {
    font-size: 14px;
  }
  .item-desc {
    font-size: 12px;
    color: var(--muted);
  }
</style>
