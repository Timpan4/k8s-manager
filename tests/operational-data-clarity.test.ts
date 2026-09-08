import { describe, expect, test } from "bun:test";
import type { BunPlugin } from "bun";
import type { Component } from "svelte";
import { compile, compileModule } from "svelte/compiler";
import { render } from "svelte/server";
import { copyText, copyTextWithAnnouncement } from "../src/components/copy-text";
import { formatRelativeTimestamp } from "../src/components/timestamp-format";
import {
	CPU_USAGE_DESCRIPTION,
	MEMORY_USAGE_DESCRIPTION,
	READINESS_NOT_REPORTED,
	RESTART_COUNT_NOT_REPORTED,
} from "../src/features/resources/operational-data";

type SvelteTestProps = Record<string, string | boolean | (() => void)>;

async function renderSvelte(path: string, props: SvelteTestProps): Promise<string> {
	const plugin: BunPlugin = {
		name: "svelte-server-test",
		setup(builder) {
			builder.onLoad({ filter: /\.svelte$/ }, async ({ path: componentPath }) => ({
				contents: compile(await Bun.file(componentPath).text(), {
					filename: componentPath,
					generate: "server",
					css: "injected",
				}).js.code,
				loader: "js",
			}));
			builder.onLoad({ filter: /\.svelte\.js$/ }, async ({ path: modulePath }) => ({
				contents: compileModule(await Bun.file(modulePath).text(), {
					filename: modulePath,
					generate: "server",
				}).js.code,
				loader: "js",
			}));
		},
	};
	const result = await Bun.build({
		entrypoints: [path],
		target: "bun",
		format: "esm",
		conditions: ["svelte"],
		plugins: [plugin],
		external: ["svelte", "svelte/*"],
		write: false,
	});
	if (!result.success || !result.outputs[0]) {
		throw new Error(result.logs.map(String).join("\n") || `Could not build ${path}`);
	}
	const code = `${await result.outputs[0].text()}\n//# sourceURL=${crypto.randomUUID()}.js`;
	// SAFETY: Bun compiles each supplied .svelte entrypoint as an ESM module whose default export is a Svelte component.
	const module = (await import(
		`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`
	)) as { default: Component<SvelteTestProps> };
	return render(module.default, { props }).body;
}

describe("operational data clarity", () => {
	test("renders readable timestamps while preserving the exact source value", async () => {
		const timestamp = "2026-08-24T12:34:56.789Z";
		const body = await renderSvelte("src/components/TimestampText.svelte", {
			value: timestamp,
			relative: "2m",
			precision: "millisecond",
		});

		expect(body).toContain(`datetime="${timestamp}"`);
		expect(body).toContain(`title="${timestamp}"`);
		expect(body).toContain(`Exact timestamp ${timestamp}`);
		expect(body).toContain(">2m</time>");
		expect(formatRelativeTimestamp("2m", timestamp, false, "utc", "millisecond")).toBe(
			"2m",
		);
		expect(formatRelativeTimestamp("2m", timestamp, true, "utc", "millisecond")).toBe(
			"2m (2026-08-24 12:34:56.789 UTC)",
		);
	});

	test("explains metric units and unavailable resource state", () => {
		expect(CPU_USAGE_DESCRIPTION).toBe(
			"CPU usage in millicores; 1000m equals one CPU core.",
		);
		expect(MEMORY_USAGE_DESCRIPTION).toBe(
			"Memory usage in binary units such as Ki, Mi, and Gi.",
		);
		expect(READINESS_NOT_REPORTED).toBe("Readiness not reported");
		expect(RESTART_COUNT_NOT_REPORTED).toBe("Restart count not reported");
	});

	test("omits full-value hover titles while copying the untruncated value", async () => {
		const value = "very-long-resource-name-that-is-visually-truncated";
		const body = await renderSvelte("tests/fixtures/CopyableTextHost.svelte", {
			value,
			label: "resource name",
			onActivate: () => {},
			actionLabel: `Open resource ${value}`,
			active: true,
		});
		const writes: string[] = [];
		const announcements: string[] = [];
		const clipboard = { writeText: async (text: string) => void writes.push(text) };
		const announce = (message: string) => void announcements.push(message);
		const message = await copyTextWithAnnouncement(
			announce,
			clipboard,
			value,
			"resource name",
		);
		await copyTextWithAnnouncement(announce, clipboard, value, "resource name");

		expect(body).not.toContain(`title="${value}"`);
		expect(body).toContain(`aria-label="Open resource ${value}" aria-pressed="true"`);
		expect(body).toContain(`aria-label="Copy resource name: ${value}"`);
		expect(body.match(/<button/g)).toHaveLength(2);
		expect(body).toContain('role="status" aria-live="polite"');
		expect(writes).toEqual([value, value]);
		expect(announcements).toEqual([
			"",
			"Copied resource name.",
			"",
			"Copied resource name.",
		]);
		expect(message).toBe("Copied resource name.");
		expect(await copyText(undefined, value, "resource name")).toBe(
			"Could not copy resource name.",
		);
	});
});
