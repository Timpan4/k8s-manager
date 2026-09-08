import { fileURLToPath } from "node:url";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";
import { frontendBundleReportPlugin } from "./scripts/frontend-bundle-report.ts";

export default defineConfig(({ command }) => {
	const host = process.env.TAURI_DEV_HOST?.trim();
	const releaseChannel = process.env.KUBECOVE_PUBLIC_RELEASE_CHANNEL === "stable" ? "stable" : "dev";
	const profiling = process.env.KUBECOVE_PUBLIC_PROFILE === "true";
	return {
		plugins: [svelte(), tailwindcss(), frontendBundleReportPlugin(releaseChannel, profiling)],
		resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
		define: {
			"process.env.KUBECOVE_PUBLIC_DEV": JSON.stringify(String(command === "serve")),
			"process.env.KUBECOVE_PUBLIC_PROFILE": JSON.stringify(String(profiling)),
			"process.env.KUBECOVE_PUBLIC_RELEASE_CHANNEL": JSON.stringify(releaseChannel),
		},
		server: {
			port: 1430,
			strictPort: true,
			host: host || "localhost",
			hmr: host ? { host } : undefined,
			watch: { ignored: ["**/src-tauri/**", "**/.e2e/**", "**/e2e/artifacts/**"] },
		},
		preview: { host: "127.0.0.1", port: Number(process.env.PORT ?? 4173), strictPort: true },
	};
});
