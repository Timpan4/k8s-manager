<script lang="ts">
	import type { HealthAssessment } from "@/lib/types";
	import {
		healthSourceLabel,
		healthSourceSummary,
		healthStateLabel,
	} from "@/lib/resource-health";
	import { Badge, Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/svelte";

	let {
		assessment,
		loading = false,
		details = false,
		compact = false,
		rawStatuses = [],
	}: { assessment?: HealthAssessment | null; loading?: boolean; details?: boolean; compact?: boolean; rawStatuses?: string[] } = $props();

	const state = $derived(assessment?.state);
	const label = $derived(
		loading ? "Loading" : assessment ? healthStateLabel(assessment.state) : "Assessment unavailable",
	);
	const source = $derived(
		loading ? "Assessment pending" : assessment ? healthSourceSummary(assessment) : "Backend contract missing",
	);
	const variant = $derived(state === "degraded" ? "destructive" : "outline");
	const tone = $derived(
		state === "healthy"
			? "border-emerald-500/50 text-emerald-700 dark:text-emerald-300"
			: state === "needsAttention"
				? "border-amber-500/50 text-amber-700 dark:text-amber-300"
				: "",
	);

	function raw<Value>(value: Value): string {
		return isString(value) ? value : (JSON.stringify(value) ?? String(value));
	}

	function isString<Value>(value: Value): value is Value & string {
		return String(value) === value;
	}
</script>

<div class="flex min-w-0 flex-wrap items-center gap-1.5">
	{#if compact}
		<Tooltip>
			<TooltipTrigger type="button" class="rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50">
				<Badge {variant} class={tone}>{label}</Badge>
			</TooltipTrigger>
			<TooltipContent class="max-w-sm flex-col items-stretch rounded-lg border border-border bg-surface-2 p-3 text-popover-foreground shadow-xl">
				<div class="grid gap-3 text-xs">
					<div>
						<p class="font-medium text-sm">{label}</p>
						<p class="mt-1 text-muted-foreground">{source}</p>
					</div>
					{#if rawStatuses.length > 0}
						<div class="flex flex-wrap gap-2 border-t border-border pt-3">
							{#each rawStatuses as status}
								<Badge variant="outline" class="whitespace-normal break-words">{status}</Badge>
							{/each}
						</div>
					{/if}
				</div>
			</TooltipContent>
		</Tooltip>
	{:else}
		<Badge {variant} class={tone}>{label}</Badge>
		<span class="truncate text-[0.6875rem] text-muted-foreground" title={source}>Source: {source}</span>
	{/if}
	{#if assessment?.completeness === "partial"}
		<Badge variant="outline" class="border-dashed">Partial</Badge>
	{/if}
</div>

{#if details && assessment}
	<details class="mt-2 rounded-md border bg-background/40 p-2 text-xs">
		<summary class="cursor-pointer font-medium">Health evidence</summary>
		<ul class="mt-2 grid gap-1.5 text-muted-foreground">
			{#each assessment.evidence as evidence}
				<li>
					<span class="font-medium text-foreground">{healthSourceLabel(evidence.source)}</span>:
					{raw(evidence.raw)} — {evidence.reason}{evidence.current ? "" : " (historical)"}
				</li>
			{/each}
		</ul>
	</details>
{/if}
