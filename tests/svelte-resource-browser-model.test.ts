import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resourceBrowserAvailableKinds } from "../src/app/svelte/workspaceShellModel";
import { argoResourceIdentityKey } from "../src/features/gitops/argo-workspace-model";
import { PAGE_SIZE } from "../src/features/resources/constants";
import {
	buildFetchKeys,
	isDiscoveredResourceKind,
	resourceGroupCollapseKey,
	resourceTypeGroupCollapseKey,
} from "../src/features/resources/helpers";
import {
	allKindOptions,
	filterHistoricalReplicaSets,
	filterResourceScopeOptions,
	filterResourcesByKinds,
	filterTopologyByKinds,
	filterTopologyByTableRows,
	initialOwnershipMapOpen,
	nextNamespaceSelection,
	shouldLoadOwnershipMap,
	syncedTopologyNodeId,
} from "../src/features/resources/resourceBrowserModel";
import { buildResourceBrowserReadSpecs } from "../src/features/resources/resourceBrowserReadSpecs";
import {
	buildResourceTableModel as buildProjectedResourceTableModel,
	buildResourceTableProjection,
} from "../src/features/resources/resourceTableModel";
import {
	buildFlowTopologyFitPlan,
	buildFlowTopologyView,
} from "../src/features/resources/topology";
import type {
	DiscoveredResourceKind,
	ResourceSummary,
	ResourceTopology,
	TopologyNode,
} from "../src/lib/types";
import type { SavedWorkspace } from "../src/lib/workspace-model";
import { buildWorkspaceReadContext } from "../src/lib/workspaceReadContext";

function resource(name: string, patch: Partial<ResourceSummary> = {}): ResourceSummary {
	const state = patch.health === "degraded"
		? "degraded"
		: patch.health === "attention"
			? "needsAttention"
			: "healthy";
	return {
		cluster: "kind-dev",
		kind: "Pod",
		name,
		namespace: "default",
		age: "1d",
		health: "healthy",
		healthAssessment: {
			state,
			completeness: "complete",
			winningSources: ["kubernetes"],
			reasons: [`Test Kubernetes state is ${state}`],
			evidence: [{ source: "kubernetes", raw: state, state, current: true, reason: `Test Kubernetes state is ${state}` }],
		},
		status: "Running",
		ready: "1/1",
		...patch,
	};
}

function buildResourceTableModel(
	rows: ResourceSummary[],
	state: Parameters<typeof buildProjectedResourceTableModel>[1],
) {
	return buildProjectedResourceTableModel(buildResourceTableProjection(rows), state);
}

function topologyNode(id: string, summary: ResourceSummary): TopologyNode {
	return {
		id,
		kind: summary.kind,
		name: summary.name,
		namespace: summary.namespace ?? null,
		status: summary.status,
		health: summary.health,
		selectable: true,
		summary,
	};
}

function topologyGraph(
	topology: ResourceTopology,
	selectedNodeId: string | null = null,
	mode: "ownership" | "networkFlow" = "ownership",
	expandedStandaloneKinds: ReadonlySet<string> = new Set(),
) {
	return buildFlowTopologyView(topology, {
		mode,
		selectedNodeId,
		showFullTopologyOnSelection: true,
		expandedStandaloneKinds,
	}).graph;
}

const widget: DiscoveredResourceKind = {
	group: "example.com",
	version: "v1",
	apiVersion: "example.com/v1",
	kind: "Widget",
	plural: "widgets",
	shortNames: ["wdg"],
	namespaced: true,
};

test("keeps cluster-kind leaf rows, counts, and topology on the selected kind", () => {
	const rows = [
		resource("worker-a", { apiVersion: "v1", kind: "Node", namespace: undefined }),
		resource("fast", { apiVersion: "storage.k8s.io/v1", kind: "StorageClass", namespace: undefined }),
		resource("data", { apiVersion: "v1", kind: "PersistentVolume", namespace: undefined }),
		resource("checkout", { apiVersion: "argoproj.io/v1alpha1", kind: "Application" }),
		resource("default", { apiVersion: "cilium.io/v2", kind: "CiliumClusterwideNetworkPolicy", namespace: undefined }),
	];
	const topology: ResourceTopology = {
		nodes: rows.map((row) => topologyNode(`${row.kind}:${row.name}`, row)),
		edges: [
			{ id: "node-to-app", source: "Node:worker-a", target: "Application:checkout", relation: "targets" },
			{ id: "storage-to-pv", source: "StorageClass:fast", target: "PersistentVolume:data", relation: "groups" },
		],
		warnings: [],
	};
	const tableState = {
		search: "",
		gitOpsFilter: "",
		healthFilter: "all" as const,
		sort: { id: "name" as const, desc: false },
		pageIndex: 0,
		collapsedGroups: new Set<string>(),
	};

	for (const kind of ["Node", "StorageClass", "PersistentVolume"] as const) {
		const scopedRows = filterResourcesByKinds(rows, [kind]);
		const table = buildResourceTableModel(scopedRows, tableState);
		const scopedTopology = filterTopologyByKinds(topology, [kind]);

		expect(resourceBrowserAvailableKinds([kind], [widget], true)).toEqual([kind]);
		expect(table.totalRows).toBe(1);
		expect(table.scopedRows.map((row) => row.kind)).toEqual([kind]);
		expect(
			table.entries
				.filter((entry) => entry.type === "resource")
				.map((entry) => entry.resource.kind),
		).toEqual([kind]);
		expect(scopedTopology?.nodes.map((node) => node.kind)).toEqual([kind]);
		expect(scopedTopology?.edges).toEqual([]);
	}
});

test("matches discovered kinds by API version without rescanning selections", () => {
	const v1 = resource("v1", { apiVersion: "example.com/v1", kind: "Widget" });
	const v2 = resource("v2", { apiVersion: "example.com/v2", kind: "Widget" });
	const builtIn = resource("pod", { apiVersion: "v2", kind: "Pod" });

	expect(filterResourcesByKinds([v1, v2, builtIn], [widget])).toEqual([v1]);
	expect(filterResourcesByKinds([v1, v2, builtIn], ["Pod"])).toEqual([builtIn]);
});

test("builds safe active-workspace read metadata from one source result", () => {
	const workspace: SavedWorkspace = {
		id: "workspace-a",
		name: "Ops",
		createdAt: "2026-07-20T00:00:00.000Z",
		updatedAt: "2026-07-20T00:00:00.000Z",
		scope: {
			clusterContext: "kind-dev",
			namespaces: ["default"],
			kinds: ["Pod"],
			argoAppFilter: "",
			layout: "resources",
		},
		shortcuts: [],
		portForwards: [],
	};
	const context = buildWorkspaceReadContext({
		workspace,
		sources: {
			kubeconfigEnvVar: "KUBECONFIG",
			paths: [],
			sourceKey: "source-a",
			sourceLabel: "Source A",
			showSourceLabels: true,
			warnings: [],
		},
		sourceSucceeded: true,
		sourceFailed: false,
		sourceError: null,
	});

	expect(context).toEqual({
		workspaceId: "workspace-a",
		clusterContext: "kind-dev",
		namespaceScope: ["default"],
		kubeconfigSourceKey: "source-a",
		sourceReady: true,
		sourceError: null,
		showKubeconfigSourceLabels: true,
	});
});

test("builds immutable ResourceBrowser read identities and enablement", () => {
	const fetchKeys = buildFetchKeys(["default", "kube-system"], ["Pod", widget]);
	const specs = buildResourceBrowserReadSpecs({
		clusterContext: "kind-dev",
		kubeconfigSourceKey: "source-a",
		fetchKeys,
		namespaces: ["kube-system", "default", "default"],
		topologyMode: "ownership",
		mapPanelOpen: false,
		sourceReady: true,
		customResourcesEnabled: true,
	});

	expect(specs.topologyNamespaces).toEqual(["default", "kube-system"]);
	expect(specs.resourceQueryKey[0]).toBe("resources");
	expect(specs.topologyQueryKey).toEqual([
		"resource-topology",
		"kubeconfigEnv=source-a",
		"kind-dev",
		"default,kube-system",
		"ownership",
		"dynamic:example.com/v1:widgets:Widget",
	]);
	expect(specs.resourcesEnabled).toBe(true);
	expect(specs.topologyEnabled).toBe(false);
	expect(specs.resourceKindsEnabled).toBe(true);

	const waitingForSource = buildResourceBrowserReadSpecs({
		clusterContext: "kind-dev",
		kubeconfigSourceKey: undefined,
		fetchKeys: [],
		namespaces: ["default"],
		topologyMode: "ownership",
		mapPanelOpen: true,
		sourceReady: false,
		customResourcesEnabled: false,
	});
	expect(waitingForSource.namespacesEnabled).toBe(false);
	expect(waitingForSource.resourceKindsEnabled).toBe(false);
	expect(waitingForSource.resourcesEnabled).toBe(false);
	expect(waitingForSource.topologyEnabled).toBe(false);
});

describe("svelte resource browser model", () => {
	test("filters resource scope options by label", () => {
		const options = [
			{ key: "default", label: "default" },
			{ key: "kube-system", label: "kube-system" },
			{ key: "storage-class", label: "StorageClass" },
		];

		expect(filterResourceScopeOptions(options, " KUBE ")).toEqual([options[1]]);
		expect(filterResourceScopeOptions(options, "storage")).toEqual([options[2]]);
		expect(filterResourceScopeOptions(options, "")).toEqual(options);
	});

	test("deselects one namespace from the implicit all selection", () => {
		expect(nextNamespaceSelection([], ["default", "kube-system"], "default", false)).toEqual([
			"kube-system",
		]);
	});

	test("does not request the ownership map for restored closed state", () => {
		const mapPanelOpen = initialOwnershipMapOpen({ mapPanelOpen: false }, true);
		let loadCalls = 0;
		if (shouldLoadOwnershipMap(mapPanelOpen, false, false)) loadCalls += 1;

		expect(mapPanelOpen).toBe(false);
		expect(loadCalls).toBe(0);
		expect(shouldLoadOwnershipMap(true, false, false)).toBe(true);
	});

	test("uses the global map default for fresh scopes and preserves restored layouts", () => {
		expect(initialOwnershipMapOpen(null, true)).toBe(true);
		expect(initialOwnershipMapOpen(undefined, false)).toBe(false);
		expect(initialOwnershipMapOpen({ mapPanelOpen: true }, false)).toBe(true);
		expect(initialOwnershipMapOpen({ mapPanelOpen: false }, true)).toBe(false);
	});

	test("filters topology to table matches and ownership connectors", () => {
		const application = topologyNode("Application:api", resource("api-app", { kind: "Application" }));
		const deployment = { ...topologyNode("Deployment:api", resource("api", { kind: "Deployment" })), deploymentRevision: 2 };
		const replicaSet = topologyNode("ReplicaSet:api-2", resource("api-2", { kind: "ReplicaSet" }));
		const job = topologyNode("Job:api-2", resource("api-2-job", { kind: "Job" }));
		const pod = topologyNode("Pod:api-2-pod", resource("api-2-pod"));
		const unrelated = topologyNode("Pod:other", resource("other"));
		const filtered = filterTopologyByTableRows(
			{ nodes: [application, deployment, replicaSet, job, pod, unrelated], edges: [
				{ id: "application-deployment", source: application.id, target: deployment.id, relation: "owns" },
				{ id: "deployment-rs", source: deployment.id, target: replicaSet.id, relation: "owns" },
				{ id: "rs-job", source: replicaSet.id, target: job.id, relation: "owns" },
				{ id: "job-pod", source: job.id, target: pod.id, relation: "owns" },
			], warnings: [] },
			[pod.summary],
		);
		expect(filtered?.nodes.map((node) => node.id)).toEqual([
			application.id,
			deployment.id,
			replicaSet.id,
			job.id,
			pod.id,
		]);
		expect(filtered?.edges.map((edge) => edge.id)).toEqual([
			"application-deployment",
			"deployment-rs",
			"rs-job",
			"job-pod",
		]);
	});

	test("handles cyclic and missing topology connectors", () => {
		const deployment = topologyNode("Deployment:api", resource("api", { kind: "Deployment" }));
		const replicaSet = topologyNode("ReplicaSet:api", resource("api-rs", { kind: "ReplicaSet" }));
		const pod = topologyNode("Pod:api", resource("api-pod"));
		const filtered = filterTopologyByTableRows(
			{
				nodes: [deployment, replicaSet, pod],
				edges: [
					{ id: "deployment-rs", source: deployment.id, target: replicaSet.id, relation: "owns" },
					{ id: "rs-deployment", source: replicaSet.id, target: deployment.id, relation: "owns" },
					{ id: "rs-pod", source: replicaSet.id, target: pod.id, relation: "owns" },
					{ id: "missing-pod", source: "missing", target: pod.id, relation: "owns" },
				],
				warnings: [],
			},
			[pod.summary],
		);

		expect(filtered?.nodes.map((node) => node.id)).toEqual([
			deployment.id,
			replicaSet.id,
			pod.id,
		]);
		expect(filtered?.edges.map((edge) => edge.id)).toEqual([
			"deployment-rs",
			"rs-deployment",
			"rs-pod",
		]);
	});

	test("hides only inactive older Deployment ReplicaSets", () => {
		const deployment = {
			...topologyNode("Deployment:api", resource("api", { kind: "Deployment" })),
			deploymentRevision: 2,
		};
		const old = { ...topologyNode("ReplicaSet:api-1", resource("api-1", { kind: "ReplicaSet" })), deploymentRevision: 1 };
		const current = { ...topologyNode("ReplicaSet:api-2", resource("api-2", { kind: "ReplicaSet" })), deploymentRevision: 2 };
		const retained = { ...topologyNode("ReplicaSet:api-0", resource("api-0", { kind: "ReplicaSet" })), deploymentRevision: 0 };
		const pod = topologyNode("Pod:api-0-pod", resource("api-0-pod"));
		const filtered = filterHistoricalReplicaSets({ nodes: [deployment, old, current, retained, pod], edges: [
			{ id: "deployment-old", source: deployment.id, target: old.id, relation: "owns" },
			{ id: "deployment-current", source: deployment.id, target: current.id, relation: "owns" },
			{ id: "deployment-retained", source: deployment.id, target: retained.id, relation: "owns" },
			{ id: "retained-pod", source: retained.id, target: pod.id, relation: "owns" },
		], warnings: [] }, true);
		expect(filtered?.nodes.map((node) => node.id)).toEqual([deployment.id, current.id, retained.id, pod.id]);

		const unknownCurrent = { ...deployment, deploymentRevision: undefined };
		const safeFallback = filterHistoricalReplicaSets({
			nodes: [unknownCurrent, old],
			edges: [{ id: "deployment-old", source: unknownCurrent.id, target: old.id, relation: "owns" }],
			warnings: [],
		}, true);
		expect(safeFallback?.nodes.map((node) => node.id)).toEqual([unknownCurrent.id, old.id]);
	});

	test("shared resource table state stays independent from table runtime imports", () => {
		const tableStateSource = readFileSync(
			"src/features/resources/table-state.ts",
			"utf8",
		);
		const helpersSource = readFileSync("src/features/resources/helpers.ts", "utf8");

		expect(tableStateSource).not.toContain("@tanstack/");
		expect(helpersSource).not.toContain("@tanstack/");
	});

	test("filters, summarizes, groups, and collapses resource rows", () => {
		const owned = resource("api", {
			gitOpsOwner: {
				provider: "argo",
				kind: "Application",
				name: "payments",
				namespace: "argocd",
				confidence: "metadata",
			},
		});
		const restarted = resource("worker", {
			health: "restarted",
			restarts: 2,
			status: "Running",
		});
		const collapsedGroups = new Set<string>([resourceGroupCollapseKey(owned)]);
		const model = buildResourceTableModel([owned, restarted], {
			search: "",
			gitOpsFilter: "",
			healthFilter: "all",
			sort: { id: "name", desc: false },
			pageIndex: 0,
			collapsedGroups,
		});

		expect(model.healthSummary.total).toBe(2);
		expect(model.healthSummary.healthy).toBe(2);
		expect(model.healthSummary.restartEvidence).toBe(1);
		expect(model.groupedByGitOps).toBe(true);
		expect(model.gitOpsFilters.map((filter) => filter.label)).toEqual([
			"Owned by Argo CD: payments (partial evidence)",
		]);
		expect(model.entries.some((entry) => entry.type === "group")).toBe(true);
		expect(model.entries).toContainEqual(
			expect.objectContaining({ type: "type", kind: "Pod" }),
		);
		expect(
			model.entries.some(
				(entry) => entry.type === "resource" && entry.resource.name === "api",
			),
		).toBe(false);
	});

	test("reuses one resource projection across table states", () => {
		const owned = resource("api", {
			gitOpsOwner: {
				provider: "argo",
				kind: "Application",
				name: "payments",
				namespace: "argocd",
			},
		});
		const restarted = resource("worker", { health: "restarted", restarts: 2 });
		const rows = [owned, restarted];
		const projection = buildResourceTableProjection(rows);
		const states = [
			{
				search: "",
				gitOpsFilter: "",
				healthFilter: "all" as const,
				sort: { id: "name" as const, desc: false },
				pageIndex: 0,
				collapsedGroups: new Set<string>(),
			},
			{
				search: "api",
				gitOpsFilter: "argo:Application:argocd:payments",
				healthFilter: "healthy" as const,
				sort: { id: "restarts" as const, desc: true },
				pageIndex: 1,
				collapsedGroups: new Set([resourceGroupCollapseKey(owned)]),
				selectedResource: owned,
			},
		];

		for (const state of states) {
			expect(buildProjectedResourceTableModel(projection, state)).toEqual(
				buildResourceTableModel(rows, state),
			);
		}
	});

	test("keeps narrowed rows and GitOps filters in one projection", () => {
			const focused = resource("api", {
				gitOpsOwner: {
					provider: "argo",
					kind: "Application",
					name: "payments",
					namespace: "argocd",
				},
			});
			const projection = buildResourceTableProjection([focused]);
			const model = buildProjectedResourceTableModel(projection, {
				search: "",
				gitOpsFilter: "",
				healthFilter: "all",
				sort: { id: "name", desc: false },
				pageIndex: 0,
				collapsedGroups: new Set(),
			});

			expect(model.displayRows).toEqual([focused]);
			expect(model.gitOpsFilters.map((filter) => filter.label)).toEqual([
				"Owned by Argo CD: payments (partial evidence)",
			]);
			expect(model.totalRows).toBe(1);
			expect(model.healthSummary.total).toBe(1);
		});

		test("inherits GitOps table grouping through Kubernetes owner chains", () => {
		const argoOwner = {
			provider: "argo",
			kind: "Application",
			name: "todo",
			namespace: "argocd",
			confidence: "metadata",
		} as const;
		const rows = [
			resource("todo-web", { kind: "Deployment", gitOpsOwner: argoOwner }),
			resource("todo-web-97bfcd566", {
				kind: "ReplicaSet",
				ownerRef: "todo-web",
			}),
			resource("todo-web-97bfcd566-hhplq", {
				kind: "Pod",
				ownerRef: "todo-web-97bfcd566",
			}),
			resource("issue-143-cron-success", { kind: "CronJob", gitOpsOwner: argoOwner }),
			resource("issue-143-cron-success-29713390", {
				kind: "Job",
				ownerRef: "issue-143-cron-success",
			}),
			resource("issue-143-cron-success-29713390-25hg7", {
				kind: "Pod",
				ownerRef: "issue-143-cron-success-29713390",
			}),
		];
		const model = buildResourceTableModel(rows, {
			search: "",
			gitOpsFilter: "",
			healthFilter: "all",
			sort: { id: "name", desc: false },
			pageIndex: 0,
			collapsedGroups: new Set(),
		});

		expect(model.entries).toContainEqual(
			expect.objectContaining({
				type: "group",
				label: "Owned by Argo CD: todo (partial evidence)",
				count: 6,
			}),
		);
		expect(model.entries).not.toContainEqual(
			expect.objectContaining({ type: "group", label: "No GitOps ownership evidence" }),
		);
		expect(
			model.pageRows
				.filter((row) => row.kind === "Pod")
				.map((row) => row.gitOpsOwner?.name),
		).toEqual(["todo", "todo"]);
	});

	test("prioritizes the exact focused GitOps owner before namespace collisions", () => {
		const focused = resource("zz-focused", {
			gitOpsOwner: {
				provider: "argo",
				kind: "Application",
				name: "payments",
				namespace: "argocd",
				confidence: "inventory",
			},
		});
		const sameNameOtherNamespaceRows = Array.from({ length: PAGE_SIZE }, (_, index) =>
			resource(`aa-other-${index}`, {
				gitOpsOwner: {
					provider: "argo",
					kind: "Application",
					name: "payments",
					namespace: "other",
					confidence: "inventory",
				},
			}),
		);
		const rows = [...sameNameOtherNamespaceRows, focused];
		const focusedKey = argoResourceIdentityKey(focused);
		const otherNamespaceKeys = new Set(
			sameNameOtherNamespaceRows
				.map(argoResourceIdentityKey)
				.filter((key): key is string => key !== null),
		);
		if (focusedKey === null) throw new Error("focused resource identity missing");
		const state = {
			search: "",
			gitOpsFilter: "",
			healthFilter: "all" as const,
			sort: { id: "name" as const, desc: false },
			pageIndex: 0,
			collapsedGroups: new Set<string>(),
			preferredGitOpsResourceKeys: new Set([focusedKey]),
		};

		const preferredModel = buildResourceTableModel(rows, state);
		expect(preferredModel.pageRows[0]).toBe(focused);
		expect(
			buildResourceTableModel(rows, {
				...state,
				preferredGitOpsResourceKeys: otherNamespaceKeys,
			}).pageRows,
		).not.toContain(focused);
		expect(
			buildResourceTableModel(rows, {
				...state,
				preferredGitOpsResourceKeys: undefined,
			}).pageRows,
		).not.toContain(focused);
		expect(
			buildResourceTableModel(rows, {
				...state,
				preferredGitOpsResourceKeys: undefined,
				pageIndex: 1,
			}).pageRows,
		).toContain(focused);
	});

	test("sorts GitOps groups by Name before paginating", () => {
		const owner = {
			provider: "argo",
			kind: "Application",
			name: "payments",
			namespace: "argocd",
			confidence: "metadata",
		} as const;
		const rows = [
			...Array.from({ length: PAGE_SIZE + 1 }, (_, index) =>
				resource(`pod-${index}`, { gitOpsOwner: owner }),
			),
			resource("z-deployment", { kind: "Deployment", gitOpsOwner: owner }),
		];
		const state = {
			search: "",
			gitOpsFilter: "",
			healthFilter: "all" as const,
			sort: { id: "name" as const, desc: false },
			pageIndex: 1,
			collapsedGroups: new Set<string>(),
		};

		expect(buildResourceTableModel(rows, state).pageRows.map((row) => row.name)).toEqual([
			"pod-50",
			"z-deployment",
		]);
		expect(
			buildResourceTableModel(rows, {
				...state,
				sort: { id: "name", desc: true },
			}).pageRows.map((row) => row.name),
		).toEqual(["pod-1", "pod-0"]);
	});

	test("keeps GitOps ownership groups contiguous across resource types", () => {
		const owner = (name: string) => ({
			provider: "argo" as const,
			kind: "Application",
			name,
			namespace: "argocd",
			confidence: "metadata" as const,
		});
		const model = buildResourceTableModel(
			[
				resource("a-pod", { gitOpsOwner: owner("alpha") }),
				resource("b-pod", { gitOpsOwner: owner("bravo") }),
				resource("c-deployment", {
					kind: "Deployment",
					gitOpsOwner: owner("alpha"),
				}),
			],
			{
				search: "",
				gitOpsFilter: "",
				healthFilter: "all",
				sort: { id: "name", desc: false },
				pageIndex: 0,
				collapsedGroups: new Set(),
			},
		);

		expect(model.pageRows.map((row) => row.name)).toEqual([
			"a-pod",
			"c-deployment",
			"b-pod",
		]);
		expect(
			model.entries
				.filter((entry) => entry.type === "group")
				.map((entry) => entry.label),
		).toEqual([
			"Owned by Argo CD: alpha (partial evidence)",
			"Owned by Argo CD: bravo (partial evidence)",
		]);
	});

	test("sorts resource type groups by Age in both directions", () => {
		const rows = [
			resource("newer", {
				kind: "Deployment",
				createdAt: "2026-08-19T00:00:00Z",
			}),
			resource("older", {
				kind: "Pod",
				createdAt: "2026-08-18T00:00:00Z",
			}),
		];
		const state = {
			search: "",
			gitOpsFilter: "",
			healthFilter: "all" as const,
			sort: { id: "age" as const, desc: false },
			pageIndex: 0,
			collapsedGroups: new Set<string>(),
		};

		expect(buildResourceTableModel(rows, state).pageRows.map((row) => row.name)).toEqual([
			"older",
			"newer",
		]);
		expect(
			buildResourceTableModel(rows, {
				...state,
				sort: { id: "age", desc: true },
			}).pageRows.map((row) => row.name),
		).toEqual(["newer", "older"]);
	});

	test("groups unmanaged Svelte table rows by resource type", () => {
		const deployment = resource("argocd-applicationset-controller", {
			kind: "Deployment",
		});
		const pod = resource("argocd-application-controller-0", {
			kind: "Pod",
		});
		const service = resource("argocd-metrics", {
			kind: "Service",
		});
		const model = buildResourceTableModel([service, pod, deployment], {
			search: "",
			gitOpsFilter: "",
			healthFilter: "all",
			sort: { id: "name", desc: false },
			pageIndex: 0,
			collapsedGroups: new Set([resourceTypeGroupCollapseKey(pod)]),
		});

		expect(model.groupedByGitOps).toBe(false);
		expect(model.entries.filter((entry) => entry.type === "group")).toEqual([]);
		expect(
			model.entries
				.filter((entry) => entry.type === "type")
				.map((entry) => ({
					kind: entry.kind,
					label: entry.label,
					count: entry.count,
					collapsed: entry.collapsed,
				})),
		).toEqual([
			{ kind: "Pod", label: "Pods", count: 1, collapsed: true },
			{ kind: "Deployment", label: "Deployments", count: 1, collapsed: false },
			{ kind: "Service", label: "Services", count: 1, collapsed: false },
		]);
		expect(
			model.entries
				.filter((entry) => entry.type === "resource")
				.map((entry) => entry.resource.name),
		).toEqual(["argocd-applicationset-controller", "argocd-metrics"]);
	});

	test("keeps built-in, cluster-scoped, and discovered kinds editable", () => {
		const kindNames = allKindOptions([widget]).map((kind) =>
			isDiscoveredResourceKind(kind) ? kind.kind : kind,
		);

		expect(kindNames).toContain("Node");
		expect(kindNames).toContain("Widget");
	});

	test("keeps selected Svelte resource visible inside collapsed groups", () => {
		const owned = resource("api", {
			apiVersion: "apps/v1",
			gitOpsOwner: {
				provider: "argo",
				kind: "Application",
				name: "payments",
				namespace: "argocd",
				confidence: "metadata",
			},
			kind: "Deployment",
		});
		const collapsedGroups = new Set<string>([
			resourceGroupCollapseKey(owned),
			resourceTypeGroupCollapseKey(owned),
		]);
		const model = buildResourceTableModel([owned], {
			search: "",
			gitOpsFilter: "",
			healthFilter: "all",
			sort: { id: "name", desc: false },
			pageIndex: 0,
			collapsedGroups,
			selectedResource: resource("api", { apiVersion: "apps/v1", kind: "Deployment" }),
		});

		expect(
			model.entries.some(
				(entry) => entry.type === "resource" && entry.resource.name === "api",
			),
		).toBe(true);
	});

	test("syncs table selection into the Svelte topology selection", () => {
		const selected = resource("api", { apiVersion: "v1" });
		const topologySummary = resource("api", { apiVersion: "apps/v1" });
		const topologyNodes = [
			topologyNode("Deployment:api", topologySummary),
			topologyNode("Pod:worker", resource("worker")),
		];

		expect(
			syncedTopologyNodeId({
				selectedTopologyNodeId: null,
				selectedResource: selected,
				topologyNodes,
			}),
		).toBe("Deployment:api");
		expect(
			syncedTopologyNodeId({
				selectedTopologyNodeId: "missing-node",
				selectedResource: selected,
				topologyNodes,
			}),
		).toBeNull();
	});

	test("scrolls selected Svelte table rows into view", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(source).toContain("function scrollSelectedTableRowIntoView");
		expect(source).toContain("tr[data-resource-selected='true']");
		expect(source).toContain("scrollIntoView({ block: \"center\", inline: \"nearest\" })");
		expect(source).toContain("secondFrame = window.requestAnimationFrame");
		expect(source).toContain("appliedSelectionScrollKey === scrollKey");
		expect(source).toContain("new ResizeObserver(measure)");
		expect(source).toContain("hasExactSelectedResource");
		expect(source).toContain("!exactMatchExists && resourceIdentityKey(resource) === identityKey");
	});

	test("passes filtered, metrics-enriched topology into the Svelte ownership map", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(source).toContain("resourceMetricIndex(metricsQuery.data)");
		expect(source).toContain("mergeTopologyMetrics(");
		expect(source).toContain("filterTopologyByTableRows(");
		expect(source).toContain("tableModel.filteredRows");
		expect(source).toContain("filterHistoricalReplicaSets(");
		expect(source).toContain("topology={topologyWithMetrics}");
		expect(source).toContain("topologyNodes: topologyWithMetrics?.nodes");
	});

	test("keeps Svelte resource table collapsible beside the ownership map", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(source).toContain("let tablePanelOpen = $state(true)");
		expect(source).toContain('aria-label="Collapse resource table"');
		expect(source).toContain('aria-label="Show resource table"');
		expect(source).toContain("<Table2");
	});

	test("keeps Svelte resource table icons and spans aligned", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(source).toContain("getResourceGroupVisual(entry.label)");
		expect(source).toContain("getResourceKindVisual(entry.kind)");
		expect(source).toContain("class={TABLE_CLASS}");
		expect(source).toContain("colspan={tableVisibleColumnCount}");
		expect(source).not.toContain('colspan="10"');
		expect(source).not.toContain('{entry.collapsed ? "Show" : "Hide"}');
	});

	test("keeps Svelte resource selection shell-controlled", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(source).toContain("selectedResource?: ResourceSummary | null");
		expect(source).toContain(
			'onResourceSelect?: (resource: ResourceSummary, source?: "explicit" | "restore") => void',
		);
		expect(source).toContain("onResourceClose?: () => void");
		expect(source).toContain('onResourceSelect(matchedResource, "restore")');
		expect(source).toContain('onResourceSelect(resource, "explicit")');
		expect(source).toContain("onResourceClose()");
		expect(source).not.toContain("let selectedResource = $state");
		expect(source).not.toContain("ResourceDetailPanel");
	});

	test("keeps Svelte table and topology selection in sync", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(source).toContain("resourceIdentityKey(resource) === identityKey");
		expect(source).toContain(
			'selectedTopologyNodeId = null;\n\t\tonResourceSelect(resource, "explicit")',
		);
		expect(source).toContain("function closeMapPanel()");
		expect(source).toContain("onMapToggle={closeMapPanel}");
	});

	test("stacks Svelte map and table when shell inspector is open", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

	expect(source).toContain("const inspectorOpen = $derived(Boolean(selectedResource))");
	expect(source).toContain("inspectorOpen && mapPanelOpen && tablePanelOpen");
	expect(source).toContain("grid min-h-0 min-w-0 flex-1");
	expect(source).toContain("grid-rows-[minmax(400px,1fr)_minmax(400px,1fr)]");
	expect(source).toContain(
		"min-[1101px]:grid-cols-[minmax(0,0.42fr)_minmax(0,0.58fr)]",
	);
});

	test("keeps Svelte resource detail panel out of the browser", () => {
		const browser = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);
		const shell = readFileSync("src/app/svelte/WorkspaceShell.svelte", "utf8");

		expect(browser).not.toContain("ResourceDetailPanel");
		expect(browser).not.toContain("Close resource details");
		expect(shell).toContain("<ResourceDetailPanel");
		expect(shell).toContain('aria-label="Close resource details"');
		expect(browser).toContain("selectedTopologyNodeId = null");
	});

	test("groups standalone Svelte topology nodes by kind", () => {
		const configMap = topologyNode("ConfigMap:settings", resource("settings", { kind: "ConfigMap" }));
		const secret = topologyNode("Secret:api-token", resource("api-token", { kind: "Secret" }));
		const topology: ResourceTopology = {
			nodes: [configMap, secret],
			edges: [],
			warnings: [],
		};

		const graph = topologyGraph(topology);

		expect(graph.nodes.map((node) => node.id)).toEqual([
			"standalone-kind:ConfigMap",
			"standalone-kind:Secret",
		]);
		expect(graph.nodes.every((node) => node.data.resource === null)).toBe(true);
		expect(graph.nodes[0]?.data.expanded).toBe(false);
	});

	test("expands selected standalone Svelte topology bucket", () => {
		const configMap = topologyNode("ConfigMap:settings", resource("settings", { kind: "ConfigMap" }));
		const secret = topologyNode("Secret:api-token", resource("api-token", { kind: "Secret" }));
		const topology: ResourceTopology = {
			nodes: [configMap, secret],
			edges: [],
			warnings: [],
		};

		const graph = topologyGraph(topology, configMap.id);

		expect(graph.nodes.map((node) => node.id)).toContain("standalone-kind:ConfigMap");
		expect(graph.nodes.map((node) => node.id)).toContain(configMap.id);
		expect(graph.nodes.find((node) => node.id === configMap.id)?.parentId).toBe(
			"standalone-kind:ConfigMap",
		);
		expect(graph.nodes.map((node) => node.id)).not.toContain(secret.id);
	});

	test("expands clicked standalone Svelte topology buckets without selecting resources", () => {
		const configMap = topologyNode("ConfigMap:settings", resource("settings", { kind: "ConfigMap" }));
		const secret = topologyNode("Secret:api-token", resource("api-token", { kind: "Secret" }));
		const topology: ResourceTopology = {
			nodes: [configMap, secret],
			edges: [],
			warnings: [],
		};

		const graph = topologyGraph(topology, null, "ownership", new Set(["Secret"]));

		expect(graph.nodes.map((node) => node.id)).toContain("standalone-kind:Secret");
		expect(graph.nodes.map((node) => node.id)).toContain(secret.id);
		expect(graph.nodes.find((node) => node.id === secret.id)?.parentId).toBe(
			"standalone-kind:Secret",
		);
		expect(graph.nodes.map((node) => node.id)).not.toContain(configMap.id);
	});

	test("keeps network-flow Svelte topology nodes ungrouped", () => {
		const service = topologyNode("Service:web", resource("web", { kind: "Service" }));
		const pod = topologyNode("Pod:web-abc", resource("web-abc", { kind: "Pod" }));
		const topology: ResourceTopology = {
			nodes: [service, pod],
			edges: [],
			warnings: [],
		};

		const graph = topologyGraph(topology, null, "networkFlow");

		expect(graph.nodes.map((node) => node.id)).toEqual(["Service:web", "Pod:web-abc"]);
		expect(graph.nodes.some((node) => node.id.startsWith("standalone-kind:"))).toBe(false);
	});

	test("toggles standalone Svelte topology buckets from map clicks", () => {
		const source = readFileSync("src/features/resources/OwnershipMap.svelte", "utf8");

		expect(source).toContain("let expandedStandaloneKinds = $state<Set<string>>(new Set())");
		expect(source).toContain("expandedStandaloneKinds,");
		expect(source).toContain('node.id.startsWith("standalone-kind:")');
		expect(source).toContain("expandedStandaloneKinds = next");
		expect(source).toContain("return;");
	});

	test("loads Svelte topology when ownership map is open", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(source).toContain('withForegroundLoad("resource-topology"');
		expect(source).toContain("enabled: readSpecs.topologyEnabled");
	});

	test("keeps Svelte topology mode copy and warnings aligned", () => {
		const source = readFileSync("src/features/resources/OwnershipMap.svelte", "utf8");

		expect(source).toContain('mode === "networkFlow" ? "Failed to load network flow" : "Failed to load ownership map"');
		expect(source).toContain('mode === "networkFlow" ? "No network flow" : "No ownership graph"');
		expect(source).toContain("No ingress, service, or pod traffic relationships were found in this scope.");
		expect(source).toContain("No workload ownership relationships were found in this scope.");
		expect(source).toContain('aria-label="Collapse ownership map"');
		expect(source).toContain("Network Flow");
		expect(source).toContain("Topology warnings");
		expect(source).toContain("topology.warnings.slice(0, 3)");
	});

	test("keeps Svelte resource browser map collapse rail parity", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(source).toContain("initialOwnershipMapOpen(initialPathState, getSettingsSnapshot().showOwnershipMapByDefault)");
		expect(source).toContain("initialOwnershipMapOpen(pathState, getSettingsSnapshot().showOwnershipMapByDefault)");
		expect(source).toContain("settingsStore");
		expect(source).toContain("$settingsStore.showFullTopologyOnSelection");
		expect(source).toContain("{showFullTopologyOnSelection}");
		expect(source).toContain("onMapToggle={closeMapPanel}");
		expect(source).toContain('aria-label="Show ownership map"');
		expect(source).toContain("[writing-mode:vertical-rl]");
	});

	test("renders Svelte sortable resource table headers for data columns", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		for (const column of [
			"name",
			"namespace",
			"kind",
			"status",
			"ready",
			"restarts",
			"cpu",
			"memory",
			"age",
		]) {
			expect(source).toContain(`@render SortButton("${column}"`);
		}
		expect(source).toContain("onclick={() => toggleSort(column)}");
	});

	test("keeps Svelte topology viewport fitting selected paths", () => {
		const mapSource = readFileSync("src/features/resources/OwnershipMap.svelte", "utf8");
		const viewportSource = readFileSync(
			"src/features/resources/OwnershipMapViewport.svelte",
			"utf8",
		);

		expect(mapSource).toContain("preventScrolling={false}");
		expect(mapSource).toContain("zoomOnScroll={false}");
		expect(mapSource).toContain("panOnScroll={false}");
		expect(mapSource).toContain("let afterGraphElement = $state<HTMLDivElement>()");
		expect(mapSource).toContain("onclick={() => afterGraphElement?.focus()}");
		expect(mapSource).toContain("bind:this={afterGraphElement}");
		expect(mapSource).toContain('tabindex="-1"');
		expect(mapSource).toContain("Drag to pan. Use controls to zoom.");
		expect(mapSource).toContain("<OwnershipMapViewport");
		expect(viewportSource).toContain("useSvelteFlow<FlowTopologyNode, FlowTopologyEdge>()");
		expect(viewportSource).toContain("fitPlan?.focused");
		expect(viewportSource).toContain("flow.setViewport");
		expect(viewportSource).toContain('import { prefersReducedMotion } from "svelte/motion"');
		expect(viewportSource).toContain("prefersReducedMotion.current ? 0 : fitDuration");
		expect(viewportSource).toContain("buildFlowTopologyFitPlan");
		expect(viewportSource).toContain("FIT_VIEW_DURATION_MS");
		expect(viewportSource).toContain("SELECTED_FIT_VIEW_DURATION_MS");
		expect(mapSource).toContain("buildFlowTopologyView");
		expect(mapSource).toContain("showFullTopologyOnSelection");
		expect(mapSource).toContain("translateExtent");
		expect(mapSource).toContain("{translateExtent}");
		expect(mapSource).toContain("VISIBLE_ELEMENT_RENDER_THRESHOLD");
		expect(mapSource).toContain("graph.nodes.length + graph.edges.length");
		expect(mapSource).toContain("{onlyRenderVisibleElements}");
		expect(mapSource).toContain("bind:clientWidth={viewportWidth}");
		expect(mapSource).toContain("{viewportWidth}");
		expect(mapSource).toContain("{viewportHeight}");
		expect(mapSource).toContain("viewportKey={fitViewportKey}");
	});

	test("refits Svelte topology when scope, viewport size, or layout changes", () => {
		const deployment = topologyNode(
			"Deployment:api",
			resource("api", { apiVersion: "apps/v1", kind: "Deployment" }),
		);
		const pod = topologyNode("Pod:api-7f9", resource("api-7f9"));
		const topology: ResourceTopology = {
			nodes: [deployment, pod],
			edges: [{ id: "Deployment:api->Pod:api-7f9", source: deployment.id, target: pod.id, relation: "owns" }],
			warnings: [],
		};
		const graph = topologyGraph(topology);
		const baseKey = buildFlowTopologyFitPlan(
			graph.nodes,
			graph.edges,
			null,
			'["resourceTopology","kind-dev",["default"],"ownership","a"]',
			{ width: 800, height: 600 },
		)?.key;
		const resizedKey = buildFlowTopologyFitPlan(
			graph.nodes,
			graph.edges,
			null,
			'["resourceTopology","kind-dev",["default"],"ownership","a"]:900x600',
			{ width: 900, height: 600 },
		)?.key;
		const movedNodes = graph.nodes.map((node) =>
			node.id === pod.id ? { ...node, position: { ...node.position, x: node.position.x + 120 } } : node,
		);
		const movedKey = buildFlowTopologyFitPlan(
			movedNodes,
			graph.edges,
			null,
			'["resourceTopology","kind-dev",["default"],"ownership","a"]',
			{ width: 800, height: 600 },
		)?.key;
		const browserSource = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(resizedKey).not.toBe(baseKey);
		expect(movedKey).not.toBe(baseKey);
		expect(browserSource).toContain("const topologyFitViewKey = $derived(JSON.stringify(topologyBaseQueryKey))");
		expect(browserSource).toContain("const topologyQueryKey = $derived(readSpecs.topologyQueryKey)");
		expect(browserSource).toContain("fitViewKey={topologyFitViewKey}");
	});

	test("keeps Svelte resource rows visible during scope refetches", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(source).toContain("placeholderData: (previousData) => previousData");
		expect(source).toContain(
			"resourcesQuery.isPending && !resourcesQuery.isPlaceholderData",
		);
		expect(source).toContain("!resourcesQuery.isPlaceholderData");
		const targetEffectStart = source.indexOf("if (!targetResourceKey)");
		const targetEffectEnd = source.indexOf("const matchedResource", targetEffectStart);
		expect(source.slice(targetEffectStart, targetEffectEnd)).toContain(
			"resourcesQuery.isPlaceholderData",
		);
	});

	test("defers Svelte metrics until resource rows and topology settle", () => {
		const source = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);

		expect(source).toContain("const BACKGROUND_METRICS_DELAY_MS = 1_500");
		expect(source).toContain("let metricsQueryReady = $state(false)");
		expect(source).toContain("resourcesQuery.isPlaceholderData");
		expect(source).toContain("(mapPanelOpen && topologyQuery.isPending)");
		expect(source).toContain('diagnosticLog("resources.metrics.defer"');
		expect(source).toContain('diagnosticLog("resources.metrics.enable"');
		expect(source).toContain("window.setTimeout");
		expect(source).toContain("enabled: metricsQueryReady && Boolean(clusterContext)");
	});

	test("preserves incoming Svelte health filters when opening resources", () => {
		const browserSource = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);
		const shellSource = readFileSync("src/app/svelte/WorkspaceShell.svelte", "utf8");
		const surfacesSource = readFileSync("src/features/incidents/IncidentView.svelte", "utf8");

		expect(browserSource).toContain('initialHealthFilter = "all"');
		expect(browserSource).toContain("healthFilter: initialHealthFilter");
		expect(browserSource).toContain("healthFilter = pathState?.healthFilter ?? initialHealthFilter");
		expect(shellSource).toContain("resourceInitialHealthFilter");
		expect(shellSource).toContain('initialHealthFilter: HealthFilter = "all"');
		expect(shellSource).toContain("initialHealthFilter={resourceInitialHealthFilter}");
		expect(surfacesSource).toContain("incidentResourcesHealthFilterFor(incidentFilter)");
		expect(surfacesSource).toContain(
			'onOpenResources(undefined, "", "", incidentResourcesHealthFilterFor(incidentFilter))',
		);
	});
});
