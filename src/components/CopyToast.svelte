<script module lang="ts">
	import { writable } from "svelte/store";

	export const copyToast = writable("");
</script>

<script lang="ts">
	import { X } from "lucide-svelte";
</script>

{#if $copyToast}
	<div class="fixed bottom-4 right-4 z-[var(--z-toast)] flex max-w-sm items-start gap-3 rounded-lg border bg-popover px-3 py-2 text-sm text-popover-foreground shadow-lg">
		<p role="status" aria-live="polite" class="min-w-0 break-words">{$copyToast}</p>
		<button type="button" aria-label="Dismiss copy notification" class="inline-flex size-6 shrink-0 items-center justify-center rounded-sm text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50" onclick={() => copyToast.set("")}>
			<X class="size-3.5" aria-hidden="true" />
		</button>
	</div>
{/if}
