<script lang="ts">
  import { tick } from 'svelte';

  /**
   * A destructive action behind an inline question: the trigger swaps for the confirming
   * button and Keep, and focus follows. `onconfirm` resolving to false (a failure) returns
   * focus to the trigger; on success the caller places focus.
   */
  let {
    label = 'Remove',
    busyLabel = 'Removing…',
    name,
    confirmLabel,
    confirmName,
    keepName,
    groupName,
    prompt = '',
    busy = false,
    onconfirm
  }: {
    label?: string;
    busyLabel?: string;
    name: string;
    confirmLabel: string;
    confirmName?: string;
    keepName: string;
    groupName: string;
    prompt?: string;
    busy?: boolean;
    onconfirm: () => Promise<boolean>;
  } = $props();

  let asking = $state(false);
  let trigger = $state<HTMLButtonElement>();
  let confirm = $state<HTMLButtonElement>();

  async function ask(): Promise<void> {
    asking = true;
    await tick();
    confirm?.focus();
  }

  async function keep(): Promise<void> {
    asking = false;
    await tick();
    trigger?.focus();
  }

  async function run(): Promise<void> {
    asking = false;
    if (!(await onconfirm())) {
      await tick();
      trigger?.focus();
    }
  }
</script>

{#if asking}
  <span class="confirm" role="group" aria-label={groupName}>
    {#if prompt}<span class="prompt">{prompt}</span>{/if}
    <button
      bind:this={confirm}
      type="button"
      class="danger"
      data-confirm="yes"
      aria-label={confirmName}
      disabled={busy}
      onclick={run}>{confirmLabel}</button
    >
    <button type="button" aria-label={keepName} onclick={keep}>Keep</button>
  </span>
{:else}
  <button
    bind:this={trigger}
    type="button"
    data-confirm="ask"
    aria-label={name}
    disabled={busy}
    onclick={ask}>{busy ? busyLabel : label}</button
  >
{/if}

<style>
  .confirm {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em;
    align-items: center;
  }

  .prompt {
    flex-basis: 100%;
    font-size: 0.85em;
    color: var(--fg-dim);
  }
</style>
