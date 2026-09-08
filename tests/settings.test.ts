import { afterEach, describe, expect, test } from "bun:test";
import {
	mergePersistedSettings,
	normalizeSavedArgoProfile,
	normalizeGitOpsViewMode,
	normalizeHelmViewMode,
	partializeSettings,
	useSettingsState,
} from "../src/lib/settings";

afterEach(() => {
	useSettingsState.getState().setDebugModeEnabled(false);
	useSettingsState.getState().setShowOwnershipMapByDefault(true);
	useSettingsState.getState().setShowFullTopologyOnSelection(false);
	useSettingsState.getState().setGitOpsViewMode("cards");
	useSettingsState.getState().setHelmViewMode("cards");
});

describe("settings", () => {
	test("opens the map by default and persists an explicit visibility preference", () => {
		expect(useSettingsState.getState().showOwnershipMapByDefault).toBe(true);
		useSettingsState.getState().setShowOwnershipMapByDefault(false);
		const saved = partializeSettings(useSettingsState.getState());
		expect(saved.showOwnershipMapByDefault).toBe(false);
		useSettingsState.getState().setShowOwnershipMapByDefault(true);
		const current = useSettingsState.getState();
		expect(mergePersistedSettings(saved, current).showOwnershipMapByDefault).toBe(false);
		expect(mergePersistedSettings({}, current).showOwnershipMapByDefault).toBe(true);
		expect(mergePersistedSettings({ showOwnershipMapByDefault: "false" }, current).showOwnershipMapByDefault).toBe(true);
	});

	test("defaults diagnostics mode off and toggles it explicitly", () => {
		expect(useSettingsState.getState().debugModeEnabled).toBe(false);

		useSettingsState.getState().setDebugModeEnabled(true);

		expect(useSettingsState.getState().debugModeEnabled).toBe(true);
	});

	test("focuses topology selections by default and can keep the full map visible", () => {
		expect(useSettingsState.getState().showFullTopologyOnSelection).toBe(false);

		useSettingsState.getState().setShowFullTopologyOnSelection(true);

		expect(useSettingsState.getState().showFullTopologyOnSelection).toBe(true);
	});

	test("defaults GitOps to cards and remembers an explicit list choice", () => {
		expect(useSettingsState.getState().gitOpsViewMode).toBe("cards");

		useSettingsState.getState().setGitOpsViewMode("list");

		expect(useSettingsState.getState().gitOpsViewMode).toBe("list");
	});

	test("falls back to cards for invalid persisted GitOps view modes", () => {
		expect(normalizeGitOpsViewMode("list")).toBe("list");
		expect(normalizeGitOpsViewMode("cards")).toBe("cards");
		expect(normalizeGitOpsViewMode("grid")).toBe("cards");
		expect(normalizeGitOpsViewMode(undefined)).toBe("cards");
	});

	test("persists and validates GitOps view mode through the configured settings storage", () => {
		const current = useSettingsState.getState();

		expect(partializeSettings({ ...current, gitOpsViewMode: "list" })).toMatchObject({
			gitOpsViewMode: "list",
		});
		expect(mergePersistedSettings({ gitOpsViewMode: "list" }, current)).toMatchObject({
			gitOpsViewMode: "list",
		});
		expect(mergePersistedSettings({ gitOpsViewMode: "grid" }, current)).toMatchObject({
			gitOpsViewMode: "cards",
		});
	});

	test("persists and validates workspace Argo connection preferences", () => {
		const current = useSettingsState.getState();
		const saved: typeof current = {
			...current,
			argoConnectionPreferences: {
				"workspace-a": { kind: "connected", profileId: "profile-a" },
			},
		};

		expect(partializeSettings(saved)).toMatchObject({
			argoConnectionPreferences: {
				"workspace-a": { kind: "connected", profileId: "profile-a" },
			},
		});
		expect(
			mergePersistedSettings(
				{
					argoConnectionPreferences: {
						"workspace-a": { kind: "kubernetes" },
						"workspace-b": { kind: "connected", profileId: "profile-b" },
						"workspace-c": { kind: "connected", profileId: " " },
						"": { kind: "connected", profileId: "ignored" },
					},
				},
				current,
			).argoConnectionPreferences,
		).toEqual({
			"workspace-a": { kind: "kubernetes" },
			"workspace-b": { kind: "connected", profileId: "profile-b" },
			"workspace-c": { kind: "automatic" },
		});
	});

	test("migrates legacy Argo URLs to endpoint-only nonsecret profiles", () => {
		const profile = normalizeSavedArgoProfile({
			id: "legacy-argo",
			url: "https://argo.example.com/argo-cd",
			clusterContext: "kind-dev",
			workspaceId: "workspace-a",
			rememberCredential: true,
			token: "must-not-persist",
			customCaPem: [1, 2, 3],
			insecureTls: true,
		});
		if (!profile) throw new Error("Expected the legacy Argo profile to normalize");

		expect(profile).toEqual({
			id: "legacy-argo",
			endpoint: { kind: "externalHttps", url: "https://argo.example.com/argo-cd" },
			clusterContext: "kind-dev",
			workspaceId: "workspace-a",
			rememberCredential: true,
		});
		expect(JSON.stringify(partializeSettings({ ...useSettingsState.getState(), argoProfiles: [profile] }))).not.toMatch(
			/token|customCaPem|insecureTls/,
		);
	});

	test("preserves canonical Argo kubeconfig source identity", () => {
		const profile = normalizeSavedArgoProfile({
			id: "source-bound",
			endpoint: { kind: "externalHttps", url: "https://argo.example.com" },
			clusterContext: "kind-dev",
			workspaceId: "workspace-a",
			kubeconfigSourceKey: "  kubeconfigSource=source-a  ",
			rememberCredential: false,
		});

		if (!profile) throw new Error("source-bound profile should normalize");
		expect(profile.kubeconfigSourceKey).toBe("kubeconfigSource=source-a");
		expect(
			partializeSettings({
				...useSettingsState.getState(),
				argoProfiles: [profile],
			}).argoProfiles,
		).toEqual([profile]);
		expect(
			normalizeSavedArgoProfile({
				id: "legacy-source",
				url: "https://argo.example.com",
				clusterContext: "kind-dev",
				workspaceId: "workspace-a",
				rememberCredential: false,
			}),
		).not.toHaveProperty("kubeconfigSourceKey");
	});

	test("defaults Helm to cards and persists only valid view modes", () => {
		expect(useSettingsState.getState().helmViewMode).toBe("cards");
		useSettingsState.getState().setHelmViewMode("list");
		expect(useSettingsState.getState().helmViewMode).toBe("list");
		expect(normalizeHelmViewMode("list")).toBe("list");
		expect(normalizeHelmViewMode("grid")).toBe("cards");

		const current = useSettingsState.getState();
		expect(partializeSettings(current)).toMatchObject({ helmViewMode: "list" });
		expect(mergePersistedSettings({ helmViewMode: "list" }, current)).toMatchObject({
			helmViewMode: "list",
		});
		expect(mergePersistedSettings({ helmViewMode: "grid" }, current)).toMatchObject({
			helmViewMode: "cards",
		});
	});
});
