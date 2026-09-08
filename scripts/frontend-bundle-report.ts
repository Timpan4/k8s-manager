import { mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";
import { type Plugin, type Rolldown, version } from "vite";

function portablePath(root: string, file: string): string {
	const path = relative(root, resolve(root, file));
	if (isAbsolute(path) || path === ".." || path.startsWith(`..${sep}`)) {
		throw new Error("Bundle report input is outside the repository");
	}
	return path.split(sep).join("/");
}

function owner(path: string): string {
	const dependency = path.lastIndexOf("node_modules/");
	if (dependency >= 0) {
		const parts = path.slice(dependency + "node_modules/".length).split("/");
		return parts[0].startsWith("@") ? parts.slice(0, 2).join("/") : parts[0];
	}
	if (path.startsWith("src/features/")) return path.split("/").slice(0, 3).join("/");
	if (path.startsWith("src/")) return path.split("/").slice(0, 2).join("/");
	return "app-assets";
}

export async function frontendBundleReport(root: string, outdir: string, bundle: Rolldown.OutputBundle, publicDir: string | false) {
	const entries = await readdir(outdir, { recursive: true, withFileTypes: true });
	const files = await Promise.all(entries.filter((entry) => entry.isFile()).map(async (entry) => {
		const absolute = resolve(entry.parentPath, entry.name);
		const fileName = portablePath(outdir, absolute);
		const output = bundle[fileName];
		const bytes = await readFile(absolute);
		const sources = output?.type === "chunk"
			? Object.keys(output.modules).filter((id) => !id.startsWith("\0")).map((id) => id.split("?")[0])
			: output?.originalFileNames ?? [];
		if (!output && publicDir) sources.push(resolve(publicDir, fileName));
		const inputs = [...new Set(sources)].map((source) => {
			const path = portablePath(root, source);
			return { path, owner: owner(path) };
		}).sort((a, b) => a.path.localeCompare(b.path));
		return {
			path: portablePath(root, absolute),
			kind: output?.type === "chunk" ? (output.isEntry ? "entry-point" : "chunk") : "asset",
			rawBytes: bytes.length,
			gzipBytes: gzipSync(bytes, { level: 9 }).length,
			brotliBytes: brotliCompressSync(bytes, { params: { [constants.BROTLI_PARAM_QUALITY]: 11 } }).length,
			entryPoint: output?.type === "chunk" && output.facadeModuleId ? portablePath(root, output.facadeModuleId) : null,
			inputs,
			imports: output?.type === "chunk" ? [
				...output.imports.map((path) => ({ path: portablePath(root, resolve(outdir, path)), kind: "import-statement" })),
				...output.dynamicImports.map((path) => ({ path: portablePath(root, resolve(outdir, path)), kind: "dynamic-import" })),
			].sort((a, b) => a.path.localeCompare(b.path)) : [],
		};
	}));
	files.sort((a, b) => a.path.localeCompare(b.path));
	return {
		schemaVersion: 2,
		bundler: `vite-${version}`,
		compression: { gzipLevel: 9, brotliQuality: 11, scope: "each-file" },
		files,
		totals: files.reduce((total, file) => ({
			rawBytes: total.rawBytes + file.rawBytes,
			gzipBytes: total.gzipBytes + file.gzipBytes,
			brotliBytes: total.brotliBytes + file.brotliBytes,
		}), { rawBytes: 0, gzipBytes: 0, brotliBytes: 0 }),
	};
}

export function frontendBundleReportPlugin(releaseChannel: string, profiling: boolean): Plugin {
	let root: string;
	let publicDir: string | false;
	let outdir: string;
	return {
		name: "kubecove-bundle-report",
		apply: "build",
		configResolved(config) {
			root = config.root;
			publicDir = config.publicDir || false;
			outdir = resolve(root, config.build.outDir);
		},
		async writeBundle(_options, bundle) {
			const report = await frontendBundleReport(root, outdir, bundle, publicDir);
			const directory = resolve(root, ".e2e/reports");
			await mkdir(directory, { recursive: true });
			await writeFile(resolve(directory, "frontend-bundle.json"), `${JSON.stringify({ ...report, releaseChannel, profiling }, null, 2)}\n`);
		},
	};
}
