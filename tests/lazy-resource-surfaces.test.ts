import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

describe("lazy resource surfaces", () => {
	test("loads the ownership map only after its panel opens and supports retry", () => {
		const source = readFileSync("src/features/resources/ResourceBrowser.svelte", "utf8");

		expect(source).not.toContain('import OwnershipMap from "./OwnershipMap.svelte"');
		expect(source).toContain('import("./OwnershipMap.svelte")');
		expect(source).toContain("initialOwnershipMapOpen(initialPathState, getSettingsSnapshot().showOwnershipMapByDefault)");
		expect(source).toContain("shouldLoadOwnershipMap(");
		expect(source).toContain("function retryOwnershipMapLoad()");
		expect(source).toContain("function retryOwnershipMap()");
		expect(source).toContain("Loading ownership map");
	});

	test("loads YAML eagerly while keeping Exec lazy with retry", () => {
		const source = readFileSync("src/features/resource-detail/ResourceDetailPanel.svelte", "utf8");

		expect(source).toContain('import ResourceYamlPane from "./ResourceYamlPane.svelte"');
		expect(source).not.toContain('import ExecTab from "./ExecTab.svelte"');
		expect(source).not.toContain('import("./ResourceYamlPane.svelte")');
		expect(source).toContain('import("./ExecTab.svelte")');
		expect(source).toContain('if (activeTab !== "exec" || ExecTabComponent || execTabLoadError) return');
		expect(source).toContain("function retryExecTabLoad()");
		expect(source).toContain("Loading Exec");
	});
});
