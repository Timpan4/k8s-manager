import { afterEach, expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { build as viteBuild } from "vite";
import { assertPortAvailable, hasRunningApp, stopOwnedProfileProcess } from "../e2e/harness/desktop-profile";
import { startupReport } from "../e2e/harness/startup-report";
import { frontendBundleReport } from "../scripts/frontend-bundle-report";
import { IPC_PREFIX, markProfileIpc, markStartup, STARTUP_PREFIX } from "../src/lib/startup-marks";

const previousProfile = process.env.KUBECOVE_PUBLIC_PROFILE;
afterEach(() => {
	if (previousProfile === undefined) delete process.env.KUBECOVE_PUBLIC_PROFILE;
	else process.env.KUBECOVE_PUBLIC_PROFILE = previousProfile;
});

test("startup marks are bounded and normal startup does not record IPC", () => {
	markStartup("path-restored");
	markStartup("path-restored");
	expect(performance.getEntriesByName(`${STARTUP_PREFIX}path-restored`)).toHaveLength(1);
	delete process.env.KUBECOVE_PUBLIC_PROFILE;
	markProfileIpc("list_kube_contexts", performance.now(), true);
	expect(performance.getEntriesByName(`${IPC_PREFIX}list_kube_contexts:ok`)).toHaveLength(0);
	process.env.KUBECOVE_PUBLIC_PROFILE = "true";
	markProfileIpc("list_kube_contexts", performance.now(), false);
	expect(performance.getEntriesByName(`${IPC_PREFIX}list_kube_contexts:error`)).toHaveLength(1);
	markProfileIpc("private/path", performance.now(), true);
	expect(performance.getEntriesByName(`${IPC_PREFIX}private/path:ok`)).toHaveLength(0);
});

test("report orders milestones, aggregates IPC, and excludes arbitrary diagnostic fields", () => {
	const report = startupReport([
		{ name: `${STARTUP_PREFIX}launcher-ready`, startTime: 80, duration: 0 },
		{ name: `${STARTUP_PREFIX}frontend-entry`, startTime: 2, duration: 0 },
		{ name: `${IPC_PREFIX}get_kubeconfig_sources:ok`, startTime: 4, duration: 10 },
		{ name: `${IPC_PREFIX}get_kubeconfig_sources:error`, startTime: 6, duration: 20 },
		{ name: "secret-token-private-namespace", startTime: 0, duration: 0 },
	], null, null);
	expect(report.milestones.slice(0, 2).map(({ name }) => name)).toEqual(["frontend-entry", "launcher-ready"]);
	expect(report.milestones.find(({ name }) => name === "first-resource-rows")).toEqual({ name: "first-resource-rows", atMs: null, unavailable: "not-reached" });
	expect(report.ipc.commands).toEqual([{ command: "get_kubeconfig_sources", count: 2, errors: 1, totalMs: 30, maxMs: 20 }]);
	expect(report.memory.webview.bytes).toBeNull();
	expect(report.memory.jsHeap.bytes).toBeNull();
	expect(JSON.stringify(report)).not.toContain("secret-token");
});

test("desktop guard recognizes app names on all platforms without matching unrelated tools", () => {
	for (const name of ["kubecove", "/usr/bin/kubecove", "/Applications/KubeCove.app/Contents/MacOS/kubecove", "C:\\Apps\\KubeCove.exe", "KubeCove"]) {
		expect(hasRunningApp(`other\n${name}\n`)).toBe(true);
	}
	expect(hasRunningApp("kubecove-helper\nbun\nps")).toBe(false);
});

test("occupied servers are refused and remain running", async () => {
	const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response("alive") });
	try {
		if (!server.port) throw new Error("Expected a TCP port");
		await expect(assertPortAvailable(server.port)).rejects.toThrow("will not stop");
		expect(await (await fetch(server.url)).text()).toBe("alive");
	} finally {
		await server.stop(true);
	}
});

test("bundle report has deterministic compression, portable ownership, and dynamic chunks", async () => {
	const root = await mkdtemp(join(tmpdir(), "kubecove-bundle-test-"));
	try {
		await writeFile(join(root, "entry.ts"), 'console.log("entry"); void import("./lazy.ts");');
		await writeFile(join(root, "lazy.ts"), 'console.log("lazy");');
		const build = await viteBuild({ configFile: false, root, logLevel: "silent", build: { rolldownOptions: { input: join(root, "entry.ts") } } });
		if (!("output" in build)) throw new Error("Expected a single build output");
		const bundle = Object.fromEntries(build.output.map((output) => [output.fileName, output]));
		const report = await frontendBundleReport(root, join(root, "dist"), bundle, false);
		expect(await frontendBundleReport(root, join(root, "dist"), bundle, false)).toEqual(report);
		expect(report.files.length).toBeGreaterThan(1);
		expect(report.files.some((file) => file.imports.some((entry) => entry.kind === "dynamic-import"))).toBe(true);
		expect(report.totals.rawBytes).toBe(build.output.reduce((sum, output) => sum + Buffer.byteLength(output.type === "chunk" ? output.code : output.source), 0));
		expect(report.totals.gzipBytes).toBeGreaterThan(0);
		expect(JSON.stringify(report)).not.toContain(root);
		expect(report.files.some((file) => file.inputs.some((input) => input.path === "entry.ts"))).toBe(true);
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});


test("interrupt cleanup stops only a process owned by the profile", async () => {
	const options = { detached: process.platform !== "win32", stdout: "ignore", stderr: "ignore" } as const;
	const owned = Bun.spawn([process.execPath, "-e", "setInterval(() => {}, 1000)"], options);
	const unrelated = Bun.spawn([process.execPath, "-e", "setInterval(() => {}, 1000)"], options);
	try {
		await stopOwnedProfileProcess(owned, "SIGTERM");
		await owned.exited;
		expect(unrelated.exitCode).toBeNull();
		process.kill(unrelated.pid, 0);
		await stopOwnedProfileProcess(owned, "SIGTERM");
	} finally {
		owned.kill();
		unrelated.kill();
		await Promise.all([owned.exited, unrelated.exited]);
	}
});

test("copied public assets receive source ownership in the Vite report", async () => {
	const root = await mkdtemp(join(tmpdir(), "kubecove-html-test-"));
	try {
		await mkdir(join(root, "public"));
		await writeFile(join(root, "public", "logo.svg"), '<svg xmlns="http://www.w3.org/2000/svg"></svg>');
		await writeFile(join(root, "index.html"), '<html><head><link rel="icon" href="/logo.svg"></head><body></body></html>');
		const build = await viteBuild({ configFile: false, root, logLevel: "silent" });
		if (!("output" in build)) throw new Error("Expected a single build output");
		const bundle = Object.fromEntries(build.output.map((output) => [output.fileName, output]));
		const report = await frontendBundleReport(root, join(root, "dist"), bundle, join(root, "public"));
		const asset = report.files.find((file) => file.path.endsWith(".svg"));
		expect(asset?.inputs[0].path).toBe("public/logo.svg");
		expect(asset?.inputs[0].owner).toBe("app-assets");
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});
