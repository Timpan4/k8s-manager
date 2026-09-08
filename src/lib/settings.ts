import { persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";
import {
	type ArgoConnectionPreference,
	normalizeArgoConnectionPreference,
} from "./argo-connection-policy";
import type { ArgoServerEndpoint } from "./gitops-types";
import type {
	JsonObject,
	KubeconfigSourcesSummary,
	YamlEncoding,
	YamlViewMode,
} from "./types";

export type TimestampTimezone = "local" | "utc";
export type YamlDiffStyle = "clean" | "git";
export type GitOpsViewMode = "cards" | "list";
export type HelmViewMode = "cards" | "list";
export type SavedArgoProfile = {
	id: string;
	endpoint: ArgoServerEndpoint;
	clusterContext?: string;
	workspaceId?: string;
	kubeconfigSourceKey?: string | null;
	rememberCredential: boolean;
};
export const DEFAULT_KUBECONFIG_ENV_VAR = "KUBECONFIG";
const DEFAULT_KUBECONFIG_SOURCE_KEY = "kubeconfigSource=default";

export interface SettingsState {
	showExactTimestamps: boolean;
	showUsageFooter: boolean;
	showOwnershipMapByDefault: boolean;
	showFullTopologyOnSelection: boolean;
	showUnavailableGitOpsProviders: boolean;
	redactSecrets: boolean;
	argoProfiles: SavedArgoProfile[];
	argoConnectionPreferences: Record<string, ArgoConnectionPreference>;
	gitOpsViewMode: GitOpsViewMode;
	helmViewMode: HelmViewMode;
	showCustomResources: boolean;
	debugModeEnabled: boolean;
	autoStartSavedPortForwards: boolean;
	keepLiveSessionsOnWorkspaceSwitch: boolean;
	allowYamlForceConflicts: boolean;
	timestampTimezone: TimestampTimezone;
	yamlViewModeDefault: YamlViewMode;
	yamlEncodingDefault: YamlEncoding;
	yamlDiffStyle: YamlDiffStyle;
	yamlErrorLensEnabled: boolean;
	kubeconfigEnvVar: string;
	kubeconfigSourceKey: string;
	kubeconfigSourceLabel: string;
	showKubeconfigSourceLabels: boolean;
	setShowExactTimestamps: (show: boolean) => void;
	setShowUsageFooter: (show: boolean) => void;
	setShowOwnershipMapByDefault: (show: boolean) => void;
	setShowFullTopologyOnSelection: (show: boolean) => void;
	setShowUnavailableGitOpsProviders: (show: boolean) => void;
	setRedactSecrets: (redact: boolean) => void;
	setArgoProfiles: (profiles: SavedArgoProfile[]) => void;
	setArgoConnectionPreference: (workspaceId: string, preference: ArgoConnectionPreference) => void;
	setGitOpsViewMode: (mode: GitOpsViewMode) => void;
	setHelmViewMode: (mode: HelmViewMode) => void;
	setShowCustomResources: (show: boolean) => void;
	setDebugModeEnabled: (enabled: boolean) => void;
	setAutoStartSavedPortForwards: (autoStart: boolean) => void;
	setKeepLiveSessionsOnWorkspaceSwitch: (keep: boolean) => void;
	setAllowYamlForceConflicts: (allow: boolean) => void;
	setTimestampTimezone: (timezone: TimestampTimezone) => void;
	setYamlViewModeDefault: (mode: YamlViewMode) => void;
	setYamlEncodingDefault: (encoding: YamlEncoding) => void;
	setYamlDiffStyle: (style: YamlDiffStyle) => void;
	setYamlErrorLensEnabled: (enabled: boolean) => void;
	setKubeconfigEnvVar: (envVar: string) => void;
	resetKubeconfigEnvVar: () => void;
	setKubeconfigSources: (sources: KubeconfigSourcesSummary) => void;
}

export function normalizeKubeconfigEnvVar(envVar: string | undefined): string {
	const trimmed = envVar?.trim();
	return trimmed || DEFAULT_KUBECONFIG_ENV_VAR;
}

export function kubeconfigSourceKey(envVar: string | undefined): string {
	if (envVar?.startsWith("kubeconfigSource=")) return envVar;
	return `kubeconfigEnv=${normalizeKubeconfigEnvVar(envVar)}`;
}

function isRecord<Value>(value: Value): value is Value & JsonObject {
	return value !== null && !Array.isArray(value) && Object(value) === value;
}

function isString<Value>(value: Value): value is Value & string {
	return String(value) === value;
}

function booleanValue<Value>(value: Value, fallback: boolean): boolean {
	return Boolean(value) === value ? Boolean(value) : fallback;
}

function isYamlDiffStyle<Value>(style: Value): style is Value & YamlDiffStyle {
	return style === "clean" || style === "git";
}

export function normalizeGitOpsViewMode<Value>(mode: Value): GitOpsViewMode {
	return mode === "list" ? "list" : "cards";
}

export function normalizeHelmViewMode<Value>(mode: Value): HelmViewMode {
	return mode === "list" ? "list" : "cards";
}

export function mergePersistedSettings<Persisted>(
	persisted: Persisted,
	current: SettingsState,
): SettingsState {
	if (!isRecord(persisted)) return current;
	const saved = persisted;
	return {
		...current,
		showExactTimestamps: booleanValue(saved.showExactTimestamps, current.showExactTimestamps),
		showUsageFooter: booleanValue(saved.showUsageFooter, current.showUsageFooter),
		showOwnershipMapByDefault:
			booleanValue(saved.showOwnershipMapByDefault, current.showOwnershipMapByDefault),
		showFullTopologyOnSelection:
			booleanValue(saved.showFullTopologyOnSelection, current.showFullTopologyOnSelection),
		showUnavailableGitOpsProviders:
			booleanValue(saved.showUnavailableGitOpsProviders, current.showUnavailableGitOpsProviders),
		redactSecrets: booleanValue(saved.redactSecrets, current.redactSecrets),
		argoProfiles: Array.isArray(saved.argoProfiles)
			? saved.argoProfiles.flatMap((profile) => {
					const normalized = normalizeSavedArgoProfile(profile);
					return normalized ? [normalized] : [];
				})
			: current.argoProfiles,
		argoConnectionPreferences: normalizeArgoConnectionPreferences(
			saved.argoConnectionPreferences,
			current.argoConnectionPreferences,
		),
		gitOpsViewMode: normalizeGitOpsViewMode(saved.gitOpsViewMode),
		helmViewMode: normalizeHelmViewMode(saved.helmViewMode),
		showCustomResources: booleanValue(saved.showCustomResources, current.showCustomResources),
		debugModeEnabled: booleanValue(saved.debugModeEnabled, current.debugModeEnabled),
		autoStartSavedPortForwards:
			booleanValue(saved.autoStartSavedPortForwards, current.autoStartSavedPortForwards),
		keepLiveSessionsOnWorkspaceSwitch:
			booleanValue(saved.keepLiveSessionsOnWorkspaceSwitch, current.keepLiveSessionsOnWorkspaceSwitch),
		allowYamlForceConflicts: booleanValue(
			saved.allowYamlForceConflicts,
			current.allowYamlForceConflicts,
		),
		timestampTimezone:
			saved.timestampTimezone === "local" || saved.timestampTimezone === "utc"
				? saved.timestampTimezone
				: current.timestampTimezone,
		yamlViewModeDefault:
			saved.yamlViewModeDefault === "kubectl" || saved.yamlViewModeDefault === "applyClean"
				? saved.yamlViewModeDefault
				: current.yamlViewModeDefault,
		yamlEncodingDefault:
			saved.yamlEncodingDefault === "yaml" || saved.yamlEncodingDefault === "kyaml"
				? saved.yamlEncodingDefault
				: current.yamlEncodingDefault,
		yamlDiffStyle: isYamlDiffStyle(saved.yamlDiffStyle)
			? saved.yamlDiffStyle
			: current.yamlDiffStyle,
		yamlErrorLensEnabled: booleanValue(saved.yamlErrorLensEnabled, current.yamlErrorLensEnabled),
	};
}

function normalizeArgoConnectionPreferences<Value>(
	value: Value,
	fallback: Record<string, ArgoConnectionPreference>,
): Record<string, ArgoConnectionPreference> {
	if (!isRecord(value)) return fallback;
	return Object.fromEntries(
		Object.entries(value).flatMap(([workspaceId, preference]) =>
			workspaceId.trim() ? [[workspaceId, normalizeArgoConnectionPreference(preference)]] : [],
		),
	);
}

export function normalizeSavedArgoProfile<Value>(value: Value): SavedArgoProfile | null {
	if (!isRecord(value)) return null;
	const profile = value;
	const id = isString(profile.id) ? profile.id.trim() : "";
	if (!id) return null;
	const endpoint = normalizeArgoEndpoint(profile.endpoint, profile.url);
	if (!endpoint) return null;
	const saved: SavedArgoProfile = {
		id,
		endpoint,
		rememberCredential: Boolean(profile.rememberCredential),
	};
	if (isString(profile.clusterContext) && profile.clusterContext.trim()) {
		saved.clusterContext = profile.clusterContext;
	}
	if (isString(profile.workspaceId) && profile.workspaceId.trim()) {
		saved.workspaceId = profile.workspaceId;
	}
	if (isString(profile.kubeconfigSourceKey) && profile.kubeconfigSourceKey.trim()) {
		saved.kubeconfigSourceKey = profile.kubeconfigSourceKey.trim();
	} else if (profile.kubeconfigSourceKey === null) {
		saved.kubeconfigSourceKey = null;
	}
	return saved;
}

function normalizeArgoEndpoint<Endpoint, LegacyUrl>(
	endpoint: Endpoint,
	legacyUrl: LegacyUrl,
): ArgoServerEndpoint | null {
	if (!isRecord(endpoint)) {
		return externalHttpsEndpoint(legacyUrl);
	}
	const value = endpoint;
	if (value.kind === "externalHttps") return externalHttpsEndpoint(value.url);
	if (value.kind !== "serviceTunnel") return null;
	const namespace = isString(value.namespace) ? value.namespace.trim() : "";
	const serviceName = isString(value.serviceName) ? value.serviceName.trim() : "";
	const servicePort = Number(value.servicePort);
	if (
		!namespace ||
		!serviceName ||
		!Number.isInteger(servicePort) ||
		!Object.is(servicePort, value.servicePort) ||
		servicePort < 1 ||
		servicePort > 65535 ||
		(value.scheme !== "https" && value.scheme !== "http")
	) {
		return null;
	}
	const rootPath = isString(value.rootPath) ? value.rootPath.trim() : "";
	const tlsServerName = isString(value.tlsServerName) ? value.tlsServerName.trim() : "";
	const normalized: ArgoServerEndpoint = {
		kind: "serviceTunnel",
		namespace,
		serviceName,
		servicePort,
		scheme: value.scheme,
	};
	if (rootPath) normalized.rootPath = rootPath;
	if (tlsServerName) normalized.tlsServerName = tlsServerName;
	return normalized;
}

function externalHttpsEndpoint<Value>(value: Value): ArgoServerEndpoint | null {
	if (!isString(value)) return null;
	try {
		return new URL(value).protocol === "https:"
			? { kind: "externalHttps", url: value }
			: null;
	} catch {
		return null;
	}
}

export function partializeSettings(state: SettingsState): Partial<SettingsState> {
	return {
		showExactTimestamps: state.showExactTimestamps,
		showUsageFooter: state.showUsageFooter,
		showOwnershipMapByDefault: state.showOwnershipMapByDefault,
		showFullTopologyOnSelection: state.showFullTopologyOnSelection,
		showUnavailableGitOpsProviders: state.showUnavailableGitOpsProviders,
		redactSecrets: state.redactSecrets,
		argoProfiles: state.argoProfiles,
		argoConnectionPreferences: state.argoConnectionPreferences,
		gitOpsViewMode: state.gitOpsViewMode,
		helmViewMode: state.helmViewMode,
		showCustomResources: state.showCustomResources,
		debugModeEnabled: state.debugModeEnabled,
		autoStartSavedPortForwards: state.autoStartSavedPortForwards,
		keepLiveSessionsOnWorkspaceSwitch: state.keepLiveSessionsOnWorkspaceSwitch,
		allowYamlForceConflicts: state.allowYamlForceConflicts,
		timestampTimezone: state.timestampTimezone,
		yamlViewModeDefault: state.yamlViewModeDefault,
		yamlEncodingDefault: state.yamlEncodingDefault,
		yamlDiffStyle: state.yamlDiffStyle,
		yamlErrorLensEnabled: state.yamlErrorLensEnabled,
	};
}

export const useSettingsState = createStore<SettingsState>()(
	persist(
		(set) => ({
			showExactTimestamps: false,
			showUsageFooter: false,
			showOwnershipMapByDefault: true,
			showFullTopologyOnSelection: false,
			showUnavailableGitOpsProviders: false,
			redactSecrets: true,
			argoProfiles: [],
			argoConnectionPreferences: {},
			gitOpsViewMode: "cards",
			helmViewMode: "cards",
			showCustomResources: true,
			debugModeEnabled: false,
			autoStartSavedPortForwards: false,
			keepLiveSessionsOnWorkspaceSwitch: false,
			allowYamlForceConflicts: true,
			timestampTimezone: "local",
			yamlViewModeDefault: "kubectl",
			yamlEncodingDefault: "yaml",
			yamlDiffStyle: "clean",
			yamlErrorLensEnabled: true,
			kubeconfigEnvVar: DEFAULT_KUBECONFIG_ENV_VAR,
			kubeconfigSourceKey: DEFAULT_KUBECONFIG_SOURCE_KEY,
			kubeconfigSourceLabel: DEFAULT_KUBECONFIG_ENV_VAR,
			showKubeconfigSourceLabels: true,
			setShowExactTimestamps: (show: boolean) =>
				set({ showExactTimestamps: show }),
			setShowUsageFooter: (show: boolean) => set({ showUsageFooter: show }),
			setShowOwnershipMapByDefault: (show: boolean) =>
				set({ showOwnershipMapByDefault: show }),
			setShowFullTopologyOnSelection: (show: boolean) =>
				set({ showFullTopologyOnSelection: show }),
			setShowUnavailableGitOpsProviders: (show: boolean) =>
				set({ showUnavailableGitOpsProviders: show }),
			setRedactSecrets: (redactSecrets: boolean) => set({ redactSecrets }),
			setArgoProfiles: (argoProfiles: SavedArgoProfile[]) => set({ argoProfiles }),
			setArgoConnectionPreference: (workspaceId, preference) =>
				set((state) => ({
					argoConnectionPreferences: {
						...state.argoConnectionPreferences,
						[workspaceId]: normalizeArgoConnectionPreference(preference),
					},
				})),
			setGitOpsViewMode: (gitOpsViewMode: GitOpsViewMode) => set({ gitOpsViewMode }),
			setHelmViewMode: (helmViewMode: HelmViewMode) => set({ helmViewMode }),
			setShowCustomResources: (show: boolean) => set({ showCustomResources: show }),
			setDebugModeEnabled: (debugModeEnabled: boolean) =>
				set({ debugModeEnabled }),
			setAutoStartSavedPortForwards: (autoStart: boolean) =>
				set({ autoStartSavedPortForwards: autoStart }),
			setKeepLiveSessionsOnWorkspaceSwitch: (keep: boolean) =>
				set({ keepLiveSessionsOnWorkspaceSwitch: keep }),
			setAllowYamlForceConflicts: (allow: boolean) =>
				set({ allowYamlForceConflicts: allow }),
			setTimestampTimezone: (timezone: TimestampTimezone) =>
				set({ timestampTimezone: timezone }),
			setYamlViewModeDefault: (mode: YamlViewMode) =>
				set({ yamlViewModeDefault: mode }),
			setYamlEncodingDefault: (encoding: YamlEncoding) =>
				set({ yamlEncodingDefault: encoding }),
			setYamlDiffStyle: (yamlDiffStyle: YamlDiffStyle) =>
				set({ yamlDiffStyle }),
			setYamlErrorLensEnabled: (yamlErrorLensEnabled: boolean) =>
				set({ yamlErrorLensEnabled }),
			setKubeconfigEnvVar: (envVar: string) =>
				set({ kubeconfigEnvVar: envVar.trim() }),
			resetKubeconfigEnvVar: () =>
				set({ kubeconfigEnvVar: DEFAULT_KUBECONFIG_ENV_VAR }),
			setKubeconfigSources: (sources: KubeconfigSourcesSummary) =>
				set({
					kubeconfigEnvVar: sources.kubeconfigEnvVar,
					kubeconfigSourceKey: sources.sourceKey,
					kubeconfigSourceLabel: sources.sourceLabel,
					showKubeconfigSourceLabels: sources.showSourceLabels,
				}),
		}),
		{
			name: "kubecove-settings",
			merge: mergePersistedSettings,
			partialize: partializeSettings,
		},
	),
);
