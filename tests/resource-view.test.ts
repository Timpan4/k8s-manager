import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

describe("resource view safeguards", () => {
	test("keeps the ownership map user-requested and unmountable", () => {
		const shellSource = readFileSync("src/app/svelte/WorkspaceShell.svelte", "utf8");
		const ownershipMapSource = readFileSync(
			"src/features/resources/OwnershipMap.svelte",
			"utf8",
		);
		const browserSource = readFileSync(
			"src/features/resources/ResourceBrowser.svelte",
			"utf8",
		);
		const readSpecsSource = readFileSync(
			"src/features/resources/resourceBrowserReadSpecs.ts",
			"utf8",
		);
		const settingsSource = readFileSync("src/lib/settings.ts", "utf8");
		const settingsSurfaceSource = readFileSync(
			"src/app/svelte/SettingsSurface.svelte",
			"utf8",
		);

		expect(browserSource).toContain("mapPanelOpen");
		expect(browserSource).toContain("tablePanelOpen");
		expect(readSpecsSource).toContain(
			"topologyEnabled: Boolean(clusterContext && mapPanelOpen) && sourceReady",
		);
		expect(ownershipMapSource).toContain("aria-pressed={true}");
		expect(ownershipMapSource).toContain("onclick={onMapToggle}");
		expect(shellSource).toContain("selectedResource={focusedResource}");
		expect(browserSource).toContain(
			"grid-rows-[minmax(400px,1fr)_minmax(400px,1fr)]",
		);
		expect(browserSource).toContain(
			"min-[1101px]:grid-cols-[minmax(0,0.42fr)_minmax(0,0.58fr)]",
		);
		expect(browserSource).toContain("min-h-[400px]");
		expect(browserSource).toContain("Collapse resource table");
		expect(browserSource).not.toContain('resourceView === "map"');
		expect(browserSource).not.toContain('resourceView === "table"');
		expect(settingsSource).toContain("showOwnershipMapByDefault");
		expect(settingsSurfaceSource).toContain("Show ownership map by default");
	});

	test("sizes resource controls from their pane instead of the viewport", () => {
		const topBarSource = readFileSync(
			"src/features/resources/ResourceBrowserTopBar.svelte",
			"utf8",
		);

		expect(topBarSource).toContain('class="@container rounded-lg');
		expect(topBarSource).toContain("@5xl:grid-cols-2");
		expect(topBarSource).toContain("@7xl:grid-cols-7");
		expect(topBarSource).not.toContain(" xl:grid-cols-[minmax(12rem");
	});

	test("reuses one searchable selector for namespace and kind scope", () => {
		const topBarSource = readFileSync(
			"src/features/resources/ResourceBrowserTopBar.svelte",
			"utf8",
		);
		const selectorSource = readFileSync(
			"src/features/resources/ResourceScopeSelector.svelte",
			"utf8",
		);

		expect(topBarSource.match(/<ResourceScopeSelector/g)).toHaveLength(2);
		expect(topBarSource).toContain("Select all namespaces");
		expect(topBarSource).toContain("Select all kinds");
		expect(topBarSource).toContain('searchAriaLabel="Search namespaces"');
		expect(topBarSource).toContain('searchAriaLabel="Search resource kinds"');
		expect(selectorSource).toContain("disabled={allSelected}");
		expect(selectorSource).toContain("onclick={onSelectAll}");
		expect(selectorSource).toContain('class="flex flex-col gap-1 p-1"');
	});
});
