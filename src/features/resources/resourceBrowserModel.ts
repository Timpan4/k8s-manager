import type {
	DiscoveredResourceKind,
	ResourceKindSelection,
	ResourceSummary,
	ResourceTopology,
	TopologyNode,
} from "@/lib/types";
import { CLUSTER_SCOPED_KINDS, SUPPORTED_KINDS } from "@/lib/types";
import {
	isDiscoveredResourceKind,
	resourceIdentityKey,
	resourceKindFetchKey,
	resourceKindLabel,
	resourceSelectionKey,
} from "./helpers";

export function initialOwnershipMapOpen(
	restoredState: { mapPanelOpen: boolean } | null | undefined,
	showByDefault: boolean,
): boolean {
	return restoredState?.mapPanelOpen ?? showByDefault;
}

export function shouldLoadOwnershipMap(
	mapPanelOpen: boolean,
	componentLoaded: boolean,
	loadFailed: boolean,
): boolean {
	return mapPanelOpen && !componentLoaded && !loadFailed;
}

export function kindSelectionKey(kind: ResourceKindSelection): string {
	return resourceKindFetchKey(kind);
}

export function kindSelectionLabel(kind: ResourceKindSelection): string {
	return resourceKindLabel(kind);
}

export interface ResourceScopeOption {
	key: string;
	label: string;
}

export function filterResourceScopeOptions(
	options: ResourceScopeOption[],
	search: string,
): ResourceScopeOption[] {
	const query = search.trim().toLowerCase();
	return query
		? options.filter((option) => option.label.toLowerCase().includes(query))
		: options;
}

export function nextNamespaceSelection(
	selectedNamespaces: string[],
	availableNamespaces: string[],
	namespace: string,
	checked: boolean,
): string[] {
	if (checked) return Array.from(new Set([...selectedNamespaces, namespace])).sort();
	const currentSelection = selectedNamespaces.length > 0 ? selectedNamespaces : availableNamespaces;
	return currentSelection.filter((item) => item !== namespace);
}

export function allKindOptions(
	discoveredKinds: DiscoveredResourceKind[],
): ResourceKindSelection[] {
	const discovered = discoveredKinds
		.toSorted((left, right) => left.kind.localeCompare(right.kind))
	return [...SUPPORTED_KINDS, ...CLUSTER_SCOPED_KINDS, ...discovered];
}

function exactResourceKindKey(kind: string, apiVersion: string | undefined): string {
	return `${kind}\0${apiVersion ?? ""}`;
}

function indexKindSelections(kinds: ResourceKindSelection[]) {
	const builtInKinds = new Set<string>();
	const exactKinds = new Set<string>();
	for (const kind of kinds) {
		if (isDiscoveredResourceKind(kind)) {
			exactKinds.add(exactResourceKindKey(kind.kind, kind.apiVersion));
		} else {
			builtInKinds.add(kind);
		}
	}
	return { builtInKinds, exactKinds };
}

function resourceMatchesKindIndex(
	resource: ResourceSummary,
	index: ReturnType<typeof indexKindSelections>,
): boolean {
	return (
		index.builtInKinds.has(resource.kind) ||
		index.exactKinds.has(exactResourceKindKey(resource.kind, resource.apiVersion))
	);
}

export function filterResourcesByKinds(
	resources: ResourceSummary[],
	kinds: ResourceKindSelection[],
): ResourceSummary[] {
	const index = indexKindSelections(kinds);
	return resources.filter((resource) => resourceMatchesKindIndex(resource, index));
}

export function filterTopologyByKinds(
	topology: ResourceTopology | undefined,
	kinds: ResourceKindSelection[],
): ResourceTopology | undefined {
	if (!topology) return undefined;
	const index = indexKindSelections(kinds);
	const nodes = topology.nodes.filter((node) => resourceMatchesKindIndex(node.summary, index));
	const nodeIds = new Set(nodes.map((node) => node.id));
	return {
		...topology,
		nodes,
		edges: topology.edges.filter(
			(edge) => nodeIds.has(edge.source) && nodeIds.has(edge.target),
		),
	};
}

/** Keeps matching resources and their ownership explanation. */
export function filterTopologyByTableRows(
	topology: ResourceTopology | undefined,
	rows: ResourceSummary[],
): ResourceTopology | undefined {
	if (!topology) return undefined;
	const rowKeys = new Set(rows.map(resourceIdentityKey));
	const nodesById = new Map(topology.nodes.map((node) => [node.id, node]));
	const incoming = new Map<string, string[]>();
	for (const edge of topology.edges) {
		const sources = incoming.get(edge.target) ?? [];
		sources.push(edge.source);
		incoming.set(edge.target, sources);
	}
	const nodeIds = new Set(
		topology.nodes
			.filter((node) => rowKeys.has(resourceIdentityKey(node.summary)))
			.map((node) => node.id),
	);
	const pending = [...nodeIds];
	for (let index = 0; index < pending.length; index += 1) {
		for (const parentId of incoming.get(pending[index]) ?? []) {
			if (nodeIds.has(parentId) || !nodesById.has(parentId)) continue;
			nodeIds.add(parentId);
			pending.push(parentId);
		}
	}
	return {
		...topology,
		nodes: topology.nodes.filter((node) => nodeIds.has(node.id)),
		edges: topology.edges.filter((edge) => nodeIds.has(edge.source) && nodeIds.has(edge.target)),
	};
}

export function filterHistoricalReplicaSets(
	topology: ResourceTopology | undefined,
	hide: boolean,
): ResourceTopology | undefined {
	if (!topology || !hide) return topology;
	const incoming = new Map<string, TopologyNode[]>();
	const nodesById = new Map(topology.nodes.map((node) => [node.id, node]));
	const podOwners = new Set<string>();
	for (const edge of topology.edges) {
		const source = nodesById.get(edge.source);
		const target = nodesById.get(edge.target);
		if (source && target) {
			const parents = incoming.get(target.id) ?? [];
			parents.push(source);
			incoming.set(target.id, parents);
			if (source.kind === "ReplicaSet" && target.kind === "Pod") podOwners.add(source.id);
		}
	}
	const hidden = new Set(
		topology.nodes
			.filter((node) => {
				if (node.kind !== "ReplicaSet" || podOwners.has(node.id) || node.deploymentRevision === undefined) return false;
				const deployment = incoming.get(node.id)?.find((parent) => parent.kind === "Deployment");
				return deployment?.deploymentRevision !== undefined && node.deploymentRevision < deployment.deploymentRevision;
			})
			.map((node) => node.id),
	);
	return {
		...topology,
		nodes: topology.nodes.filter((node) => !hidden.has(node.id)),
		edges: topology.edges.filter((edge) => !hidden.has(edge.source) && !hidden.has(edge.target)),
	};
}

export function syncedTopologyNodeId({
	selectedTopologyNodeId,
	selectedResource,
	topologyNodes,
}: {
	selectedTopologyNodeId: string | null;
	selectedResource: ResourceSummary | null;
	topologyNodes: TopologyNode[] | undefined;
}): string | null {
	if (!topologyNodes) return selectedTopologyNodeId;
	if (
		selectedTopologyNodeId &&
		!topologyNodes.some((node) => node.id === selectedTopologyNodeId)
	) {
		return null;
	}
	if (!selectedResource) return selectedTopologyNodeId;
	const selectedKey = resourceSelectionKey(selectedResource);
	const selectedIdentityKey = resourceIdentityKey(selectedResource);
	const selectedFromTable =
		topologyNodes.find((node) => resourceSelectionKey(node.summary) === selectedKey) ??
		topologyNodes.find(
			(node) => resourceIdentityKey(node.summary) === selectedIdentityKey,
		);
	return selectedFromTable?.id ?? selectedTopologyNodeId;
}
