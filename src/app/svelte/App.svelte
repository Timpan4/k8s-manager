<script lang="ts">
	import { onMount, tick } from "svelte";
	import CopyToast from "@/components/CopyToast.svelte";
	import { markStartup } from "@/lib/startup-marks";
	import { useQueryClient } from "@tanstack/svelte-query";
	import { FolderOpen, Settings } from "lucide-svelte";
	import { Button, TooltipProvider } from "@/components/ui/svelte";
	import AppUsageFooter from "./AppUsageFooter.svelte";
	import SettingsSurface from "./SettingsSurface.svelte";
	import UpdateStatusButton from "./UpdateStatusButton.svelte";
	import WorkspaceShell from "./WorkspaceShell.svelte";
	import WorkspaceLauncher from "@/features/workspaces/WorkspaceLauncher.svelte";
	import {
		invalidatePodExecQueries,
		invalidatePortForwardQueries,
	} from "@/features/live-sessions";
	import ForegroundLoadingBar from "./ForegroundLoadingBar.svelte";
	import { workspaceStore } from "@/features/workspaces/workspaceStore";
	import { diagnosticLog, setDiagnosticsEnabled } from "@/lib/diagnostics";
	import { isAppUpdatesEnabled } from "@/lib/release-channel";
	import { kubeconfigSourcesQueryOptions } from "@/lib/kubeconfig-sources-query";
	import {
		createTauriClient,
		cancelWorkspaceRequests,
		setBackendDiagnosticsEnabled,
		stopLiveSessionsOutsideScope,
	} from "@/lib/tauri";
	import {
		readPathState,
		writePathState,
		type PathStateWorkspaceSnapshot,
	} from "@/lib/path-state";
	import { getSettingsSnapshot, settingsStore } from "@/lib/settings-store";
	import { workspaceScopeContexts, type CreateWorkspaceInput } from "@/lib/workspace-model";
	import { isFiniteReadQuery } from "@/lib/finite-read-lifecycle";
	import {
		cancelWorkspaceWork,
		createWorkspaceTransitionCoordinator,
	} from "./workspaceTransition";
	import { scheduleAutomaticUpdateCheck } from "./appUpdateStore";

	const selectedWorkspace = workspaceStore.selectedWorkspace;
	const diagnosticsClient = createTauriClient();
	const liveSessionClient = createTauriClient();
	const workspaceTransitionClient = createTauriClient();
	const queryClient = useQueryClient();
	type WorkspaceDestination =
		| { type: "open"; workspaceId: string }
		| { type: "create"; input: CreateWorkspaceInput }
		| { type: "launcher" }
		| { type: "context"; workspaceId: string; input: CreateWorkspaceInput };
	let workspaceTransitionPending = $state(false);
	let liveSessionCleanupMessage = $state<string | null>(null);
	let initialWorkspacePathState = $state<PathStateWorkspaceSnapshot | null>(null);
	let launcherView = $state<"workspaces" | "settings">("workspaces");
	let pathStateReady = $state(false);
	let lastDiagnosticsEnabled: boolean | null = null;
	let liveSessionScopeInitialized = false;
	let lastLiveSessionCleanupKey = "";

	const workspaceTransition = createWorkspaceTransitionCoordinator<WorkspaceDestination>({
		suspend: async () => {
			workspaceTransitionPending = true;
			await tick();
		},
		cancel: () =>
			cancelWorkspaceWork(
				() =>
					queryClient.cancelQueries({ predicate: isFiniteReadQuery }),
				async () => {
					await cancelWorkspaceRequests(workspaceTransitionClient);
				},
			),
		apply: (destination) => {
			switch (destination.type) {
				case "open":
					workspaceStore.openWorkspace(destination.workspaceId);
					break;
				case "create":
					workspaceStore.createWorkspace(destination.input);
					break;
				case "launcher":
					workspaceStore.clearSelectedWorkspace();
					break;
				case "context":
					workspaceStore.updateWorkspace(destination.workspaceId, destination.input);
					break;
			}
		},
		resume: () => {
			workspaceTransitionPending = false;
		},
		onCancelError: (error) => {
			diagnosticLog("workspace.transition.cancel.error", {
				error: error instanceof Error ? error.message : String(error),
			});
		},
	});

	function openWorkspace(workspaceId: string) {
		void workspaceTransition.request({ type: "open", workspaceId });
	}

	function createWorkspace(input: CreateWorkspaceInput) {
		void workspaceTransition.request({ type: "create", input });
	}

	function closeWorkspace() {
		void workspaceTransition.request({ type: "launcher" });
	}

	function changeWorkspaceContext(workspaceId: string, input: CreateWorkspaceInput) {
		void workspaceTransition.request({ type: "context", workspaceId, input });
	}

	function openLauncherSettings() {
		launcherView = "settings";
	}

	function openWorkspaceLauncher() {
		launcherView = "workspaces";
	}

	onMount(() => {
		const pathState = readPathState();
		if (pathState?.workspace) {
			initialWorkspacePathState = pathState.workspace;
			workspaceStore.openWorkspace(pathState.workspace.workspaceId);
		} else if (pathState?.launcherView) {
			launcherView = pathState.launcherView;
		}
		pathStateReady = true;
		markStartup("path-restored");
	});

	onMount(() => {
		void queryClient.fetchQuery(kubeconfigSourcesQueryOptions(liveSessionClient))
			.then((sources) => {
				getSettingsSnapshot().setKubeconfigSources(sources);
			})
			.catch((error) => {
				diagnosticLog("kubeconfig.sources.bootstrap.error", {
					error: error instanceof Error ? error.message : String(error),
				});
			});
	});

	onMount(() => {
		if (!isAppUpdatesEnabled()) return;
		return scheduleAutomaticUpdateCheck();
	});

	function syncDiagnostics(enabled: boolean) {
		setDiagnosticsEnabled(enabled);
		void setBackendDiagnosticsEnabled(diagnosticsClient, enabled).catch((error) => {
			diagnosticLog("diagnostics.backend.toggle.error", {
				error: error instanceof Error ? error.message : String(error),
			});
		});
	}

	$effect(() => {
		const enabled = $settingsStore.debugModeEnabled;
		if (lastDiagnosticsEnabled === enabled) return;
		lastDiagnosticsEnabled = enabled;
		syncDiagnostics(enabled);
	});

	$effect(() => {
		if (!pathStateReady || $selectedWorkspace || workspaceTransitionPending) return;
		writePathState({
			version: 1,
			runtime: "svelte",
			launcherView,
			workspace: null,
		});
	});

	$effect(() => {
		const workspace = $selectedWorkspace;
		const allowedClusterContexts = workspace
			? workspaceScopeContexts(workspace.scope)
			: [];
		const keepLiveSessions =
			$settingsStore.keepLiveSessionsOnWorkspaceSwitch;
		const kubeconfigSourceKey = $settingsStore.kubeconfigSourceKey;
		const cleanupKey = JSON.stringify({
			workspaceId: workspace?.id ?? null,
			allowedClusterContexts,
			keepLiveSessions,
			kubeconfigSourceKey,
		});
		if (lastLiveSessionCleanupKey === cleanupKey) return;
		lastLiveSessionCleanupKey = cleanupKey;

		if (!liveSessionScopeInitialized) {
			liveSessionScopeInitialized = true;
			return;
		}
		if (keepLiveSessions) return;

		let cancelled = false;
		void stopLiveSessionsOutsideScope(liveSessionClient, {
			allowedClusterContexts,
			kubeconfigSourceKey,
		})
			.then(async (result) => {
				if (cancelled) return;
				await Promise.all([
					invalidatePortForwardQueries((options) =>
						queryClient.invalidateQueries(options),
					),
					invalidatePodExecQueries((options) =>
						queryClient.invalidateQueries(options),
					),
				]);
				const stopped =
					result.stoppedPortForwards + result.stoppedPodExecSessions;
				if (stopped > 0) {
					liveSessionCleanupMessage = `Stopped ${result.stoppedPortForwards} port ${result.stoppedPortForwards === 1 ? "forward" : "forwards"} and ${result.stoppedPodExecSessions} exec ${result.stoppedPodExecSessions === 1 ? "session" : "sessions"} outside this workspace.`;
				}
			})
			.catch((error) => {
				if (cancelled) return;
				liveSessionCleanupMessage = `Live session cleanup failed: ${error instanceof Error ? error.message : String(error)}`;
			});

		return () => {
			cancelled = true;
		};
	});
</script>

<CopyToast />

<TooltipProvider>
	{#if workspaceTransitionPending}
	<div class="flex h-screen w-full items-center justify-center bg-background text-sm text-muted-foreground">
		Switching workspace…
	</div>
{:else if $selectedWorkspace}
	<ForegroundLoadingBar />
	<WorkspaceShell
		workspace={$selectedWorkspace}
		initialPathState={initialWorkspacePathState?.workspaceId === $selectedWorkspace.id
			? initialWorkspacePathState
			: null}
		onPathStateConsumed={() => (initialWorkspacePathState = null)}
		{liveSessionCleanupMessage}
		onDismissLiveSessionCleanup={() => (liveSessionCleanupMessage = null)}
		onOpenLauncher={closeWorkspace}
		onChangeClusterContext={changeWorkspaceContext}
	/>
{:else}
	<div class="flex h-screen w-full flex-col overflow-hidden bg-background text-foreground">
		<ForegroundLoadingBar />
		<header class="flex h-12 shrink-0 items-center gap-4 border-b bg-sidebar px-4 [-webkit-app-region:drag]">
			<div class="flex min-w-0 flex-1 items-center gap-2 [-webkit-app-region:no-drag]">
				<span class="truncate whitespace-nowrap font-heading text-sm font-semibold">KubeCove</span>
				<span class="hidden text-xs text-muted-foreground sm:inline">
					{launcherView === "settings" ? "Settings" : "Workspaces"}
				</span>
			</div>
			<div class="flex min-w-0 flex-1 items-center justify-center">
				<span class="truncate whitespace-nowrap text-sm font-semibold">
					{launcherView === "settings" ? "Settings" : "Workspaces"}
				</span>
			</div>
			<div class="flex flex-1 items-center justify-end gap-1 [-webkit-app-region:no-drag]">
				<UpdateStatusButton />
				{#if launcherView === "settings"}
					<Button
						type="button"
						variant="ghost"
						size="icon"
						aria-label="Open workspaces"
						onclick={openWorkspaceLauncher}
					>
						<FolderOpen />
					</Button>
				{:else}
					<Button
						type="button"
						variant="ghost"
						size="icon"
						aria-label="Open settings"
						onclick={openLauncherSettings}
					>
						<Settings />
					</Button>
				{/if}
			</div>
		</header>

		<main class="min-h-0 flex-1 overflow-y-auto">
			{#if launcherView === "settings"}
				<SettingsSurface onBack={openWorkspaceLauncher} />
			{:else}
				<WorkspaceLauncher {openWorkspace} {createWorkspace} />
			{/if}
		</main>
		<AppUsageFooter />
	</div>
	{/if}
</TooltipProvider>
