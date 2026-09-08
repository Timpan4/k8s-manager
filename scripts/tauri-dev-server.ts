const DEV_PORT = 1430;
const DEV_ORIGIN = `http://localhost:${DEV_PORT}`;
const VITE_SERVER_MARKER = '<meta name="kubecove-dev-server" content="vite"';

async function fetchText(url: string): Promise<string | null> {
	const controller = new AbortController();
	const timeout = setTimeout(() => controller.abort(), 750);
	try {
		const response = await fetch(url, { signal: controller.signal });
		if (!response.ok) return null;
		return await response.text();
	} catch {
		return null;
	} finally {
		clearTimeout(timeout);
	}
}

async function hasKubeCoveViteServer(): Promise<boolean> {
	const rootHtml = await fetchText(DEV_ORIGIN);
	return rootHtml?.includes("<title>KubeCove</title>") === true &&
		rootHtml.includes(VITE_SERVER_MARKER) && rootHtml.includes("/@vite/client");
}

async function hasAnyServer(): Promise<boolean> {
	return (await fetchText(DEV_ORIGIN)) !== null;
}

async function stayAlive(): Promise<never> {
	await new Promise(() => undefined);
	throw new Error("unreachable");
}

if (await hasKubeCoveViteServer()) {
	console.log(`[kubecove:dev] Reusing existing Vite server at ${DEV_ORIGIN}.`);
	await stayAlive();
}

if (await hasAnyServer()) {
	console.error(
		`[kubecove:dev] Port ${DEV_PORT} is already in use, but ${DEV_ORIGIN} does not look like the KubeCove Vite dev server.`,
	);
	console.error("[kubecove:dev] Stop that process, then run bun run tauri dev again.");
	process.exit(1);
}

const child = Bun.spawn(["bun", "run", "dev"], {
	stdin: "inherit",
	stdout: "inherit",
	stderr: "inherit",
});

for (const signal of ["SIGINT", "SIGTERM"] as const) {
	process.on(signal, () => {
		child.kill(signal);
	});
}

process.exit(await child.exited);
