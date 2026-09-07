<script lang="ts">
	import "@xterm/xterm/css/xterm.css";

	import { createQuery, useQueryClient } from "@tanstack/svelte-query";
	import { onMount, tick } from "svelte";
	import { Play, RotateCcw, Terminal as TerminalIcon } from "lucide-svelte";
	import FriendlyError from "@/components/FriendlyError.svelte";
	import {
		Alert,
		AlertDescription,
		AlertTitle,
		Badge,
		Button,
		Empty,
		EmptyDescription,
		EmptyHeader,
		EmptyTitle,
		Spinner,
	} from "@/components/ui/svelte";
	import {
		buildPodExecRequest,
		commandForPreset,
		invalidatePodExecQueries,
		isPodExecForResource,
		podExecCommandText,
		podExecQueryOptions,
		podExecTarget,
		startPodExec,
		stopPodExec,
		type PodExecPreset,
	} from "@/features/live-sessions";
	import { settingsStore } from "@/lib/settings-store";
	import {
		closePodExecChannel,
		createPodExecChannel,
		resizePodExecTerminal,
		writePodExecStdin,
		type TauriClient,
	} from "@/lib/tauri";
	import type {
		PodExecSessionMessage,
		PodExecSessionSummary,
		ResourceSummary,
	} from "@/lib/types";
	import PodExecLaunchForm from "./PodExecLaunchForm.svelte";
	import PodExecSessionList from "./PodExecSessionList.svelte";
	import { createExecTerminal, type ExecTerminalHandle } from "./execTerminal";
	import { createExecInput } from "./execInput";
	import type { ContainerStatusRow } from "./helpers";

	const DEFAULT_COLS = 100;
	const DEFAULT_ROWS = 32;

	let {
		client,
		resource,
		containers,
		selectedContainer = $bindable(""),
		kubeconfigSourceKey,
		active,
	}: {
		client: TauriClient;
		resource: ResourceSummary;
		containers: ContainerStatusRow[];
		selectedContainer?: string;
		kubeconfigSourceKey?: string;
		active: boolean;
	} = $props();

	const queryClient = useQueryClient();
	let preset = $state<PodExecPreset>("sh");
	let customArgv = $state("");
	let confirmed = $state(false);
	let starting = $state(false);
	let stoppingId = $state<string | null>(null);
	let status = $state("idle");
	let message = $state("Start an exec session to open the terminal.");
	let error = $state<unknown>(null);
	let sessionId = $state<string | null>(null);
	let channel = $state<ReturnType<typeof createPodExecChannel> | null>(null);
	let terminalHost = $state<HTMLDivElement | null>(null);
	let terminal: ExecTerminalHandle | null = null;
	let input: ReturnType<typeof createExecInput> | null = null;

	const isPod = $derived(resource.kind === "Pod" && Boolean(resource.namespace));
	const showKubeconfigSourceLabels = $derived(
		$settingsStore.showKubeconfigSourceLabels,
	);
	const command = $derived(commandForPreset(preset, customArgv));
	const commandText = $derived(
		isValidationMessage(command) ? "" : podExecCommandText(command),
	);

	const sessionsQuery = createQuery<PodExecSessionSummary[]>(() =>
		podExecQueryOptions(client, {
			enabled: active,
			refetchInterval: active ? 3_000 : false,
		}),
	);
	const sessions = $derived(
		(sessionsQuery.data ?? []).filter((session) =>
			isPodExecForResource(session, resource, kubeconfigSourceKey),
		),
	);

	onMount(() => {
		if (terminalHost) {
			const nextTerminal = createExecTerminal(
				terminalHost,
				(data) => {
					input?.enqueue(data);
				},
				(size) => {
					if (sessionId) void resizePodExecTerminal(client, sessionId, size).catch((err) => { error = err; });
				},
			);
			terminal = nextTerminal;
			const fit = () => fitTerminal();
			const frame = window.requestAnimationFrame(fit);
			window.addEventListener("resize", fit);
			return () => {
				input?.dispose();
				window.cancelAnimationFrame(frame);
				window.removeEventListener("resize", fit);
				if (channel) closePodExecChannel(channel);
				if (sessionId) {
					void stopPodExec({
						client,
						sessionId,
						invalidateQueries: (options) => queryClient.invalidateQueries(options),
					});
				}
				nextTerminal.dispose();
				terminal = null;
			};
		}
		return () => {
			input?.dispose();
			if (channel) closePodExecChannel(channel);
			if (sessionId) {
				void stopPodExec({
					client,
					sessionId,
					invalidateQueries: (options) => queryClient.invalidateQueries(options),
				});
			}
		};
	});

	$effect(() => {
		if (!active) return;
		const frame = window.requestAnimationFrame(() => fitTerminal());
		return () => window.cancelAnimationFrame(frame);
	});

	function fitTerminal() {
		terminal?.fit();
	}

	function isValidationMessage<Value>(value: Value): value is Extract<Value, string> {
		return Object(value) !== value;
	}

	function appendOutput(text: string) {
		terminal?.write(text);
	}

	function closeChannelOnly() {
		input?.dispose();
		input = null;
		if (channel) closePodExecChannel(channel);
		channel = null;
		sessionId = null;
	}

	function handleMessage(event: PodExecSessionMessage) {
		if (event.type !== "started" && sessionId && event.sessionId !== sessionId) return;
		if (event.type === "started") {
			sessionId = event.sessionId;
			input?.dispose();
			input = createExecInput(
				(data) => writePodExecStdin(client, event.sessionId, data),
				(err) => { error = err; },
			);
			status = event.summary.status;
			return;
		}
		if (event.type === "status") {
			status = event.status;
			message = event.message;
			return;
		}
		if (event.type === "output") {
			appendOutput(event.data);
			return;
		}
		if (event.type === "error") {
			status = "error";
			error = event.message;
			appendOutput(`\n${event.message}\n`);
			closeChannelOnly();
			void invalidatePodExecQueries((options) => queryClient.invalidateQueries(options));
			return;
		}
		if (event.type === "exited") {
			status = "exited";
			message =
				event.exitCode === undefined
					? "Exec session exited"
					: `Exec session exited with code ${event.exitCode}`;
			closeChannelOnly();
			void invalidatePodExecQueries((options) => queryClient.invalidateQueries(options));
			return;
		}
		if (event.type === "stopped") {
			message = "Exec session stopped";
			closeChannelOnly();
			void invalidatePodExecQueries((options) => queryClient.invalidateQueries(options));
		}
	}

	async function startSession() {
		if (starting) return;
		starting = true;
		try {
			error = null;
			await tick();
			fitTerminal();
			const request = buildPodExecRequest(
				resource,
				{
					preset,
					customArgv,
					container: selectedContainer,
					cols: terminal?.cols ?? DEFAULT_COLS,
					rows: terminal?.rows ?? DEFAULT_ROWS,
					confirmed,
				},
				kubeconfigSourceKey,
			);
			if (isValidationMessage(request)) {
				error = request;
				return;
			}
			const previous = sessionId;
			closeChannelOnly();
			if (previous) {
				await stopPodExec({
					client,
					sessionId: previous,
					invalidateQueries: (options) => queryClient.invalidateQueries(options),
				});
			}
			const nextChannel = createPodExecChannel(handleMessage);
			channel = nextChannel;
			status = "starting";
			message = "Starting exec session";
			terminal?.clear();
			terminal?.writeln(
				`Starting ${commandText} on ${podExecTarget(resource, selectedContainer)}\r\n`,
			);
			try {
				const summary = await startPodExec({
					client,
					request,
					channel: nextChannel,
					invalidateQueries: (options) => queryClient.invalidateQueries(options),
				});
				// Channel messages may have advanced or torn down the session
				// while the start call was in flight; never regress their state.
				if (channel !== nextChannel) return;
				if (!sessionId) {
					sessionId = summary.id;
					status = summary.status;
				}
			} catch (err) {
				closePodExecChannel(nextChannel);
				if (channel === nextChannel) channel = null;
				status = "error";
				error = err;
			}
		} finally {
			starting = false;
		}
	}

	async function stopSession(id: string) {
		stoppingId = id;
		try {
			if (sessionId === id) closeChannelOnly();
			await stopPodExec({
				client,
				sessionId: id,
				invalidateQueries: (options) => queryClient.invalidateQueries(options),
			});
		} catch (err) {
			error = err;
		} finally {
			stoppingId = null;
		}
	}
</script>

{#if resource.kind !== "Pod"}
	<Empty class="min-h-32 border border-dashed">
		<EmptyHeader>
			<EmptyTitle>Exec starts from Pods</EmptyTitle>
			<EmptyDescription>Select an exact namespaced Pod before exec.</EmptyDescription>
		</EmptyHeader>
	</Empty>
{:else if !isPod}
	<Alert variant="destructive">
		<AlertTitle>Namespace required</AlertTitle>
		<AlertDescription>Pod exec requires a namespaced Pod target.</AlertDescription>
	</Alert>
{:else}
	<div class="flex flex-col gap-3">
		<Alert>
			<TerminalIcon />
			<AlertTitle>Guarded Pod exec</AlertTitle>
			<AlertDescription>Exec is limited to this exact Pod through the Kubernetes API.</AlertDescription>
		</Alert>

		<PodExecLaunchForm
			{resource}
			{containers}
			bind:selectedContainer
			bind:preset
			bind:customArgv
			bind:confirmed
			{showKubeconfigSourceLabels}
			{kubeconfigSourceKey}
			{commandText}
		/>

		{#if error}
			<FriendlyError
				error={error}
				context={{ operation: "exec", fallbackTitle: "Exec failed" }}
			/>
		{/if}

		<div class="flex flex-wrap items-center gap-2">
			<Badge variant={status === "error" ? "destructive" : "outline"}>Exec: {status}</Badge>
			<span class="text-xs text-muted-foreground">{message}</span>
			<Button onclick={startSession} disabled={starting}>
				{#if starting}
					<Spinner data-icon="inline-start" />
				{:else}
					<Play data-icon="inline-start" />
				{/if}
				Start
			</Button>
		</div>

		<div
			bind:this={terminalHost}
			class="min-h-[280px] overflow-hidden rounded-md border bg-background p-2"
			class:hidden={!active}
		></div>
		<div class="flex justify-end">
			<Button variant="outline" size="sm" onclick={() => terminal?.clear()}>
				<RotateCcw data-icon="inline-start" />
				Clear terminal
			</Button>
		</div>

		<PodExecSessionList
			{sessions}
			fetching={sessionsQuery.isFetching}
			{showKubeconfigSourceLabels}
			{stoppingId}
			onStop={(sessionId) => void stopSession(sessionId)}
		/>
	</div>
{/if}
