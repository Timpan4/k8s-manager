<script lang="ts">
	import {
		Bug,
		Cable,
		FileCode2,
		FolderCog,
		RefreshCw,
		Search,
		SlidersHorizontal,
	} from "lucide-svelte";
	import { openUrl } from "@tauri-apps/plugin-opener";
	import {
		Button,
		buttonClass,
		FieldGroup,
		Input,
		SegmentedControl,
		Switch,
	} from "@/components/ui/svelte";
	import FriendlyError from "@/components/FriendlyError.svelte";
	import type { TimestampTimezone, YamlDiffStyle } from "@/lib/settings";
	import { IS_DEV_BUILD } from "@/lib/build-env";
	import { isTauriRuntime } from "@/lib/tauri-runtime";
	import type { YamlEncoding, YamlViewMode } from "@/lib/types";
	import ArgoConnectionSettings from "./ArgoConnectionSettings.svelte";
	import DiagnosticsSettings from "./DiagnosticsSettings.svelte";
	import KubeconfigSettings from "./KubeconfigSettings.svelte";
	import SettingsRow from "./SettingsRow.svelte";
	import UpdatesSettings from "./UpdatesSettings.svelte";
	import { settingsStore } from "@/lib/settings-store";

	type SettingsCategoryId =
		| "general"
		| "sessions"
		| "yaml"
		| "kubeconfig"
		| "updates"
		| "diagnostics"
		| "argo";
	type SettingsRowMeta = { title: string; description: string };

	const WIKI_URL = "https://github.com/Timpan4/kubecove/wiki";

	const GENERAL_ROWS = {
		documentation: {
			title: "KubeCove documentation",
			description: "Opens user guides and reference documentation in your browser.",
		},
		exactTimestamps: {
			title: "Show exact timestamps",
			description: "Adds exact timestamps next to relative ages.",
		},
		timezone: {
			title: "Timestamp timezone",
			description: "Controls exact timestamps in labels and tooltips.",
		},
		usageFooter: {
			title: "Show CPU and memory footer",
			description: "Shows compact app process CPU and memory usage.",
		},
		fullTopologyOnSelection: {
			title: "Keep full map visible during selection",
			description:
				"Shows unrelated ownership branches while a resource is selected. Large namespaces may render slower.",
		},
		ownershipMap: {
			title: "Show ownership map by default",
			description: "Opens the ownership map in new resource views. Returning to a view restores its previous layout.",
		},
	unavailableGitOpsProviders: {
		title: "Show unavailable GitOps providers",
		description:
			"Shows Argo CD and Flux provider groups as disabled when CRDs are missing.",
	},
	customResources: {
		title: "Show Custom Resources",
		description:
			"Loads Custom Resources after native Kubernetes resources are visible.",
	},
	redactSecrets: {
		title: "Redact secrets",
		description: "Keeps Secret values hidden by default. Connected Argo Secret values stay provider masked.",
	},
} satisfies Record<string, SettingsRowMeta>;

	const SESSION_ROWS = {
		autoStartPortForwards: {
			title: "Auto-start saved port forwards",
			description:
				"Starts saved Service port-forward presets when a workspace is restored.",
		},
		keepLiveSessions: {
			title: "Keep live sessions across workspace switches",
			description:
				"Leaves port-forward and Pod exec sessions running when you leave their scope.",
		},
	} satisfies Record<string, SettingsRowMeta>;

	const YAML_ROWS = {
		viewMode: {
			title: "YAML shape",
			description:
				"Controls whether YAML opens as Kubectl view or Apply clean output.",
		},
		encoding: {
			title: "YAML encoding",
			description: "Controls whether YAML panels open as regular YAML or KYAML.",
		},
		diffAppearance: {
			title: "YAML diff appearance",
			description: "Controls selected-resource dry-run diff rendering.",
		},
		errorLens: {
			title: "YAML error lens",
			description: "Shows inline editor diagnostics below YAML lines.",
		},
		forceConflicts: {
			title: "Allow YAML force-conflicts",
			description: "Lets guarded YAML operations take server-side field ownership.",
		},
	} satisfies Record<string, SettingsRowMeta>;

	const KUBECONFIG_ROWS = {
		activeSources: {
			title: "Active sources",
			description:
				"Shows where contexts are discovered from. Full file paths are shown only here.",
		},
		envVar: {
			title: "Environment variable",
			description:
				"Variable holding kubeconfig paths. The default path is read when it is unset.",
		},
		sourceLabels: {
			title: "Show source labels",
			description:
				"Marks contexts with the kubeconfig source they were discovered from.",
		},
		addedPaths: {
			title: "Added kubeconfig paths",
			description:
				"Extra kubeconfig files KubeCove reads alongside the environment variable.",
		},
	} satisfies Record<string, SettingsRowMeta>;

	const UPDATES_ROWS = {
		version: {
			title: "KubeCove version",
			description: "Current version, update checks, and installing new releases.",
		},
	} satisfies Record<string, SettingsRowMeta>;

	const DIAGNOSTICS_ROWS: SettingsRowMeta[] = [
		{
			title: "Enable diagnostics",
			description:
				"Collects local latency timings for release smoke testing. Trace data stays in memory.",
		},
		{
			title: "Latency report",
			description:
				"Shows frontend and backend timing summaries, with redacted copy output by default.",
		},
	];

	if (IS_DEV_BUILD) {
		const topologySpike: SettingsRowMeta = {
			title: "Topology spike",
			description:
				"Opens the Svelte topology benchmark harness with synthetic 4,000-node data, compact nodes, selected edges, and a focused viewport.",
		};
		DIAGNOSTICS_ROWS.push(topologySpike);
	}

	const categoryRows = {
		general: Object.values(GENERAL_ROWS),
		sessions: Object.values(SESSION_ROWS),
		yaml: Object.values(YAML_ROWS),
		kubeconfig: Object.values(KUBECONFIG_ROWS),
		updates: Object.values(UPDATES_ROWS),
		diagnostics: DIAGNOSTICS_ROWS,
		argo: [{ title: "Argo CD connection", description: "Connect to Argo CD without storing credentials in settings." }],
	} satisfies Record<SettingsCategoryId, SettingsRowMeta[]>;

	const categories: Array<{ id: SettingsCategoryId; label: string }> = [
		{ id: "general", label: "General" },
		{ id: "sessions", label: "Live sessions" },
		{ id: "yaml", label: "YAML" },
		{ id: "kubeconfig", label: "Kubeconfig" },
		{ id: "updates", label: "Updates" },
		{ id: "diagnostics", label: "Diagnostics" },
		{ id: "argo", label: "Argo CD" },
	];

	let activeCategory = $state<SettingsCategoryId>("general");
	let query = $state("");
	let documentationError = $state<unknown>(null);
	let {
		onBack,
		clusterContext,
		workspaceId,
		kubeconfigEnvVar,
	}: {
		onBack?: () => void;
		clusterContext?: string;
		workspaceId?: string;
		kubeconfigEnvVar?: string;
	} = $props();
	const settings = $derived($settingsStore);

	const searching = $derived(query.trim().length > 0);
	const heading = $derived(
		searching
			? "Search results"
			: (categories.find((category) => category.id === activeCategory)?.label ??
				"Settings"),
	);
	const hasMatches = $derived(
		Object.values(categoryRows)
			.flat()
			.some((row) => matchesSettingsQuery(query, row)),
	);

	function matchesSettingsQuery(value: string, row: SettingsRowMeta): boolean {
		const needle = value.trim().toLowerCase();
		return (
			needle.length === 0 ||
			row.title.toLowerCase().includes(needle) ||
			row.description.toLowerCase().includes(needle)
		);
	}

	function showCategory(id: SettingsCategoryId): boolean {
		if (!searching) return activeCategory === id;
		return categoryRows[id].some((row) => matchesSettingsQuery(query, row));
	}

	async function openDocumentation(event: MouseEvent): Promise<void> {
		if (!isTauriRuntime()) return;
		event.preventDefault();
		try {
			await openUrl(WIKI_URL);
			documentationError = null;
		} catch (error) {
			documentationError = error;
		}
	}

</script>

<div class="mx-auto grid w-full max-w-7xl gap-5 p-4 md:p-6 xl:grid-cols-[240px_minmax(0,1fr)]">
	<aside class="flex min-w-0 flex-col gap-3">
		{#if onBack}
			<Button
				type="button"
				variant="ghost"
				size="sm"
				class="h-7 w-fit px-0 text-sm text-muted-foreground hover:text-foreground"
				onclick={onBack}
			>
				Back to app
			</Button>
		{/if}
		<div class="relative">
			<Search
				class="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
			/>
			<Input
				class="h-9 pl-9 text-sm"
				bind:value={query}
				placeholder="Search settings..."
				aria-label="Search settings"
			/>
		</div>
		<nav class="flex flex-wrap gap-1 xl:grid" aria-label="Settings sections">
			{#each categories as category}
				<Button
					type="button"
					variant={activeCategory === category.id && !searching
						? "secondary"
						: "ghost"}
					size="sm"
					class="h-8 justify-start gap-2.5 px-3 text-sm"
					onclick={() => {
						activeCategory = category.id;
						query = "";
					}}
				>
					{#if category.id === "general"}<SlidersHorizontal data-icon="inline-start" />
					{:else if category.id === "sessions"}<Cable data-icon="inline-start" />
					{:else if category.id === "yaml"}<FileCode2 data-icon="inline-start" />
					{:else if category.id === "kubeconfig"}<FolderCog data-icon="inline-start" />
					{:else if category.id === "updates"}<RefreshCw data-icon="inline-start" />
					{:else if category.id === "diagnostics"}<Bug data-icon="inline-start" />{:else}<Cable data-icon="inline-start" />{/if}
					<span>{category.label}</span>
				</Button>
			{/each}
		</nav>
	</aside>

	<section class="flex w-full max-w-5xl min-w-0 flex-col gap-4">
		<h2 class="font-heading text-xl font-semibold">{heading}</h2>

		{#if searching && !hasMatches}
			<div class="rounded-md border bg-card p-4 text-sm text-muted-foreground">
				No settings match "{query.trim()}".
			</div>
		{/if}

		{#if showCategory("general")}
		<FieldGroup>
			<SettingsRow {...GENERAL_ROWS.documentation}>
				<a
					href={WIKI_URL}
					target="_blank"
					rel="noreferrer"
					class={buttonClass({ variant: "outline", size: "sm" })}
					onclick={openDocumentation}
				>
					Open Wiki
				</a>
			</SettingsRow>
			{#if documentationError}
				<FriendlyError
					error={documentationError}
					mode="compact"
					context={{ fallbackTitle: "KubeCove could not open the documentation" }}
				/>
			{/if}
			<SettingsRow {...GENERAL_ROWS.exactTimestamps}>
				<Switch
					checked={settings.showExactTimestamps}
					onCheckedChange={settings.setShowExactTimestamps}
					aria-label={GENERAL_ROWS.exactTimestamps.title}
				/>
			</SettingsRow>
			<SettingsRow {...GENERAL_ROWS.timezone}>
				<SegmentedControl
					value={settings.timestampTimezone}
					options={[
						{ value: "local", label: "Local" },
						{ value: "utc", label: "UTC" },
					]}
					onChange={(value: TimestampTimezone) =>
						settings.setTimestampTimezone(value)}
					ariaLabel={GENERAL_ROWS.timezone.title}
				/>
			</SettingsRow>
			<SettingsRow {...GENERAL_ROWS.usageFooter}>
				<Switch
					checked={settings.showUsageFooter}
					onCheckedChange={settings.setShowUsageFooter}
					aria-label={GENERAL_ROWS.usageFooter.title}
				/>
			</SettingsRow>
			<SettingsRow {...GENERAL_ROWS.redactSecrets}>
				<Switch checked={settings.redactSecrets} onCheckedChange={settings.setRedactSecrets} aria-label={GENERAL_ROWS.redactSecrets.title} />
			</SettingsRow>
			<SettingsRow {...GENERAL_ROWS.fullTopologyOnSelection}>
				<Switch
					checked={settings.showFullTopologyOnSelection}
					onCheckedChange={settings.setShowFullTopologyOnSelection}
					aria-label={GENERAL_ROWS.fullTopologyOnSelection.title}
				/>
			</SettingsRow>
			<SettingsRow {...GENERAL_ROWS.ownershipMap}>
				<Switch
					checked={settings.showOwnershipMapByDefault}
					onCheckedChange={settings.setShowOwnershipMapByDefault}
					aria-label={GENERAL_ROWS.ownershipMap.title}
				/>
			</SettingsRow>
			<SettingsRow {...GENERAL_ROWS.unavailableGitOpsProviders}>
				<Switch
					checked={settings.showUnavailableGitOpsProviders}
					onCheckedChange={settings.setShowUnavailableGitOpsProviders}
					aria-label={GENERAL_ROWS.unavailableGitOpsProviders.title}
				/>
			</SettingsRow>
			<SettingsRow {...GENERAL_ROWS.customResources}>
				<Switch
					checked={settings.showCustomResources}
					onCheckedChange={settings.setShowCustomResources}
					aria-label={GENERAL_ROWS.customResources.title}
				/>
			</SettingsRow>
		</FieldGroup>
		{/if}
		{#if showCategory("sessions")}
		<FieldGroup>
			<SettingsRow {...SESSION_ROWS.autoStartPortForwards}>
				<Switch
					checked={settings.autoStartSavedPortForwards}
					onCheckedChange={settings.setAutoStartSavedPortForwards}
					aria-label={SESSION_ROWS.autoStartPortForwards.title}
				/>
			</SettingsRow>
			<SettingsRow {...SESSION_ROWS.keepLiveSessions}>
				<Switch
					checked={settings.keepLiveSessionsOnWorkspaceSwitch}
					onCheckedChange={settings.setKeepLiveSessionsOnWorkspaceSwitch}
					aria-label={SESSION_ROWS.keepLiveSessions.title}
				/>
			</SettingsRow>
		</FieldGroup>
		{/if}

		{#if showCategory("yaml")}
		<FieldGroup>
			<SettingsRow {...YAML_ROWS.viewMode}>
				<SegmentedControl
					value={settings.yamlViewModeDefault}
					options={[
						{ value: "kubectl", label: "Kubectl view" },
						{ value: "applyClean", label: "Apply clean" },
					]}
					onChange={(value: YamlViewMode) => settings.setYamlViewModeDefault(value)}
					ariaLabel={YAML_ROWS.viewMode.title}
				/>
			</SettingsRow>
			<SettingsRow {...YAML_ROWS.encoding}>
				<SegmentedControl
					value={settings.yamlEncodingDefault}
					options={[
						{ value: "yaml", label: "YAML" },
						{ value: "kyaml", label: "KYAML" },
					]}
					onChange={(value: YamlEncoding) => settings.setYamlEncodingDefault(value)}
					ariaLabel={YAML_ROWS.encoding.title}
				/>
			</SettingsRow>
			<SettingsRow {...YAML_ROWS.diffAppearance}>
				<SegmentedControl
					value={settings.yamlDiffStyle}
					options={[
						{ value: "clean", label: "Clean" },
						{ value: "git", label: "Git" },
					]}
					onChange={(value: YamlDiffStyle) => settings.setYamlDiffStyle(value)}
					ariaLabel={YAML_ROWS.diffAppearance.title}
				/>
			</SettingsRow>
			<SettingsRow {...YAML_ROWS.errorLens}>
				<Switch
					checked={settings.yamlErrorLensEnabled}
					onCheckedChange={settings.setYamlErrorLensEnabled}
					aria-label={YAML_ROWS.errorLens.title}
				/>
			</SettingsRow>
			<SettingsRow {...YAML_ROWS.forceConflicts}>
				<Switch
					checked={settings.allowYamlForceConflicts}
					onCheckedChange={settings.setAllowYamlForceConflicts}
					aria-label={YAML_ROWS.forceConflicts.title}
				/>
			</SettingsRow>
		</FieldGroup>
		{/if}

		{#if showCategory("kubeconfig")}
			<KubeconfigSettings />
		{/if}

		{#if showCategory("updates")}
			<UpdatesSettings />
		{/if}

		{#if showCategory("diagnostics")}
			<DiagnosticsSettings />
		{/if}

		{#if showCategory("argo")}
			<ArgoConnectionSettings {clusterContext} {workspaceId} {kubeconfigEnvVar} />
		{/if}

	</section>
</div>
