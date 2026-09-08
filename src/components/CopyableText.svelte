<script lang="ts">
	import { Check, Copy } from "lucide-svelte";
	import { copyTextWithAnnouncement } from "./copy-text";
	import { copyToast } from "./CopyToast.svelte";
	import Tooltip from "./ui/svelte/Tooltip.svelte";
	import TooltipContent from "./ui/svelte/TooltipContent.svelte";
	import TooltipTrigger from "./ui/svelte/TooltipTrigger.svelte";

	let {
		value,
		label,
		class: className = "",
		textClass = "",
		onActivate,
		actionLabel,
		active,
	}: {
		value: string;
		label: string;
		class?: string;
		textClass?: string;
		onActivate?: () => void;
		actionLabel?: string;
		active?: boolean;
	} = $props();

	let copied = $state(false);
	let message = $state("");

	async function handleCopy(event: MouseEvent) {
		event.stopPropagation();
		const result = await copyTextWithAnnouncement(
			(nextMessage) => (message = nextMessage),
			navigator.clipboard,
			value,
			label,
		);
		copied = result.startsWith("Copied ");
		copyToast.set(copied ? `Copied ${label}: ${value}` : result);
	}
</script>

<span class={`flex min-w-0 items-center gap-1 ${className}`}>
	{#if onActivate}
		<button
			type="button"
			class={`min-w-0 cursor-pointer truncate rounded-sm bg-transparent p-0 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 ${textClass}`}
			aria-label={actionLabel ?? `Open ${label}: ${value}`}
			aria-pressed={active}
			onclick={(event: MouseEvent) => {
				event.stopPropagation();
				onActivate();
			}}
		>{value}</button>
	{:else}
		<span class={`min-w-0 truncate ${textClass}`}>{value}</span>
	{/if}
	<Tooltip>
		<TooltipTrigger
			type="button"
			class="inline-flex size-5 shrink-0 cursor-pointer items-center justify-center rounded-sm text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
			aria-label={`Copy ${label}: ${value}`}
			onclick={handleCopy}
			onkeydown={(event: KeyboardEvent) => event.stopPropagation()}
		>
			{#if copied}<Check class="size-3" aria-hidden="true" />{:else}<Copy class="size-3" aria-hidden="true" />{/if}
		</TooltipTrigger>
		<TooltipContent sideOffset={8} class="border bg-popover text-popover-foreground shadow-md">Copy {label}</TooltipContent>
	</Tooltip>
	<span class="sr-only" role="status" aria-live="polite">{message}</span>
</span>
