import { createHash, randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import { chmod, mkdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { arch, platform } from "node:os";
import { basename, delimiter, dirname, join, resolve } from "node:path";
import { parse, stringify } from "yaml";
import { assertDesktopProfileIdle, stopOwnedProfileProcess } from "./harness/desktop-profile";
import { downloadAsset, sha256, verifyAsset } from "./harness/assets";
import { kindConfig, kindDeleteArgs } from "./harness/cluster";
import { safeDiagnosticCommands, safeDiagnosticText } from "./harness/diagnostics";
import { gitSeedIdentity, prepareGitSeed } from "./harness/git-seed";
import { gitRepositoryUrl, platformApplicationNames, tenantApplicationNames } from "./harness/lab";
import { assertOwned, assertOwnedOnDisk, assertOwnedPathOnDisk, expectedCluster, type Ownership, ownershipFromDisk, type Provider } from "./harness/ownership";
import { chartPins, fixturePaths, gitDaemonPins, validateImmutablePins } from "./harness/platform";

type Kubeconfig = {
	apiVersion?: string;
	clusters?: Array<{ name?: string; cluster: { server?: string } }>;
	contexts?: Array<{ name: string; context: { cluster: string } }>;
	users?: Array<{ user: { token?: string } }>;
};

const root = resolve(process.cwd());
const stateDir = join(root, ".e2e");
const args = Bun.argv.slice(3);
const option = (name: string) => {
	const index = args.indexOf(name);
	return index >= 0 ? args[index + 1] : undefined;
};
const action = Bun.argv[2];
const profiling = action === "desktop-profile";
const keep = args.includes("--keep");
const requestedRunId = option("--run-id") ?? process.env.E2E_RUN_ID;
const generatedRunId = `${Date.now()}-${process.pid}-${randomUUID().slice(0, 8)}`;
const runId = requestedRunId ?? generatedRunId;
const requestedKubernetes = option("--kubernetes") ?? "1.35";
const requestedProvider = option("--provider") ?? "auto";
const workspaceHash = createHash("sha256").update(root).digest("hex").slice(0, 12);
const os = platform() === "darwin" ? "darwin" : platform() === "win32" ? "windows" : "linux";
const cpu = arch() === "arm64" ? "arm64" : "amd64";
const suffix = os === "windows" ? ".exe" : "";

if (!/^[a-zA-Z0-9][a-zA-Z0-9-]{0,47}$/.test(runId)) throw new Error("run ID must be 1-48 letters, digits, or hyphens");
const kubernetes = parseKubernetesVersion(requestedKubernetes);
validateImmutablePins();

const images = {
	"1.34": "kindest/node:v1.34.8@sha256:02722c2dedddcfc00febf5d27fbeb9b7b2c14294c82109ff4a85d89ac9ba3256",
	"1.35": "kindest/node:v1.35.5@sha256:ce977ae6d65918d0b58a5f8b5e940429c2ce42fa3a5619ec2bbc60b949c0ac95",
	"1.36": "kindest/node:v1.36.1@sha256:3489c7674813ba5d8b1a9977baea8a6e553784dab7b84759d1014dbd78f7ebd5",
} satisfies Record<string, string>;
function parseKubernetesVersion(value: string): keyof typeof images {
	if (value === "1.34" || value === "1.35" || value === "1.36") return value;
	throw new Error("--kubernetes must be 1.34, 1.35, or 1.36");
}
function imageFor(version: string) {
	return images[parseKubernetesVersion(version)];
}
const pins = {
	kind: { "darwin-amd64": "295ac6d0d634c9819c9907df45e3017d1f13166bd13c3404c45e79f7faa47498", "darwin-arm64": "dca67911095a110c2b5c36e26df6cac860c602033e456c0db47be498cdef1ebb", "linux-amd64": "50030de23cf40a18505f20426f6a8506bedf13c6e509244bd1fa9463721b0f54", "linux-arm64": "b92cd615e97585de8ddade28ed5cd7feb4248d717c233eea5b03c37298900f5d", "windows-amd64": "0bcb2d1cfedc1912d664014db716937e8a0e843e91c6807b4db2025dbc8989fa" },
	kubectl: { "darwin-amd64": "fa6b472ca1e542e171d7daedd9435b8e9650bc18d42a57eb930a51f48ca58874", "darwin-arm64": "1827b555615791c1c1065dd64870eb49a4e00e9dfd389a82a2ef1d31bb46d200", "linux-amd64": "5d11e2ba01ea68ffd053f56e27738e2b4330013ee67f7e46c6da6c585d3c9926", "linux-arm64": "c0f97f31c9ddc22d4951d543a1a7125a9af4b31e895ad4aa99899c4ba2a6ff0b", "windows-amd64": "ce1c21d0e0a64fa249fad3b6372c14479e7901fedf6d5a425d92c46c2bd87442" },
	helm: { "darwin-amd64": "1376ea697140e4db316736e760d5a47d12afc1524dce704476ef06fd7fdeddc6", "darwin-arm64": "f13f959015447b6bc309f9fd506509926543988a39035c088b52522ec95e2acb", "linux-amd64": "97dbeb971be4ac4b27e3839976d9564c0fb35c6f3b1da89dd1e292d236af4096", "linux-arm64": "1f8de130dfbd04de64978e7b852a7a547be1404956a366608276d2520b678670", "windows-amd64": "614f68ddc567ac9bfb0c205f869b1f83ba4e0a9aacd26cbae47743ae6082a579" },
} as const;

const runDir = (id = runId) => join(stateDir, "runs", id);
const devDir = join(stateDir, "dev", workspaceHash);
const recordPath = (dir: string) => join(dir, "ownership.json");
const pinKeys = {
	darwin: { amd64: "darwin-amd64", arm64: "darwin-arm64" },
	linux: { amd64: "linux-amd64", arm64: "linux-arm64" },
	windows: { amd64: "windows-amd64", arm64: "windows-amd64" },
} as const;
const key = () => pinKeys[os][cpu];

const children = new Set<ReturnType<typeof Bun.spawn>>();
function cleanEnvironment(extra: Record<string, string | undefined> = {}) {
	const env = { ...process.env };
	delete env.KUBECONFIG;
	for (const [name, value] of Object.entries(extra)) {
		if (value === undefined) delete env[name];
		else env[name] = value;
	}
	return env;
}
async function execute(cmd: string, commandArgs: string[], extra: Record<string, string | undefined> = {}, capture = false, logFile?: string) {
	const child = Bun.spawn([cmd, ...commandArgs], { cwd: root, detached: profiling && os !== "windows", env: cleanEnvironment(extra), stdout: capture || logFile ? "pipe" : "inherit", stderr: capture || logFile ? "pipe" : "inherit" });
	children.add(child);
	const [code, stdout, stderr] = await Promise.all([child.exited, child.stdout ? new Response(child.stdout).text() : "", child.stderr ? new Response(child.stderr).text() : ""]);
	children.delete(child);
	if (logFile) { await mkdir(dirname(logFile), { recursive: true }); await writeFile(logFile, `${stdout}${stderr}`); }
	if (code !== 0) throw new Error(`${basename(cmd)} failed (${code})`);
	return { stdout, stderr };
}
async function attempt(cmd: string, commandArgs: string[], extra: Record<string, string | undefined> = {}) {
	try { return await execute(cmd, commandArgs, extra, true); } catch { return undefined; }
}
async function runWdio(config: string, extra: Record<string, string | undefined>, logFile?: string) {
	const cli = join(root, "node_modules", "@wdio", "cli", "bin", "wdio.js");
	const version = await execute("node", ["--version"], {}, true);
	const match = /^v(\d+)\.(\d+)\.(\d+)/.exec(version.stdout.trim());
	if (!match) throw new Error(`could not parse Node version ${version.stdout.trim()}`);
	const [major, minor] = match.slice(1, 3).map(Number);
	if ((major === 18 && minor >= 20) || (major > 18 && major <= 24)) return execute("node", [cli, "run", config], extra, false, logFile);
	if (major < 18 || (major === 18 && minor < 20)) throw new Error(`WDIO requires Node 18.20-24; found ${version.stdout.trim()}`);
	const fnm = Bun.which("fnm");
	if (!fnm) throw new Error(`WDIO requires Node 18-24; found ${version.stdout.trim()} and no fnm fallback`);
	return execute(fnm, ["exec", "--using", "24", "node", cli, "run", config], extra, false, logFile);
}
async function verified(name: string, url: string, expected: string) {
	const target = join(stateDir, "tools", name);
	await mkdir(dirname(target), { recursive: true });
	if (existsSync(target) && sha256(new Uint8Array(await readFile(target))) === expected) return target;
	await rm(target, { force: true });
	const bytes = await downloadAsset(name, url);
	verifyAsset(name, bytes, expected);
	const temporary = `${target}.${process.pid}.part`;
	await writeFile(temporary, bytes); await rename(temporary, target);
	if (os !== "windows") await chmod(target, 0o755);
	return target;
}
let toolPromise: Promise<{ kind: string; kubectl: string; helm: string }> | undefined;
function tools() {
	if (toolPromise) return toolPromise;
	toolPromise = (async () => {
	if (!pins.kind[key()] || !pins.kubectl[key()] || !pins.helm[key()]) throw new Error(`unsupported tool platform ${key()}`);
	const kind = await verified(`kind${suffix}`, `https://github.com/kubernetes-sigs/kind/releases/download/v0.32.0/kind-${os}-${cpu}`, pins.kind[key()]);
	const kubectl = await verified(`kubectl${suffix}`, `https://dl.k8s.io/release/v1.35.6/bin/${os}/${cpu}/kubectl${suffix}`, pins.kubectl[key()]);
	const archiveName = `helm-v4.2.0-${os}-${cpu}.${os === "windows" ? "zip" : "tar.gz"}`;
	const archive = await verified(archiveName, `https://get.helm.sh/${archiveName}`, pins.helm[key()]);
	const extractDir = join(stateDir, "tools", `helm-extract-${process.pid}`); await rm(extractDir, { recursive: true, force: true }); await mkdir(extractDir, { recursive: true });
	if (os === "windows") { const escapedArchive = archive.replaceAll("'", "''"); const escapedDir = extractDir.replaceAll("'", "''"); await execute("powershell", ["-NoProfile", "-Command", `Expand-Archive -Force -LiteralPath '${escapedArchive}' -DestinationPath '${escapedDir}'`]); }
	else await execute("tar", ["-xzf", archive, "-C", extractDir]);
	const helm = join(stateDir, "tools", `helm${suffix}`); await Bun.write(helm, Bun.file(join(extractDir, `${os}-${cpu}`, `helm${suffix}`))); if (os !== "windows") await chmod(helm, 0o755); await rm(extractDir, { recursive: true, force: true });
		return { kind, kubectl, helm };
	})();
	return toolPromise;
}

function providerBinary(name: Provider) { return Bun.which(name); }
async function providerEnvironment(name: Provider) { const binary = providerBinary(name); if (!binary) throw new Error(`${name} is unavailable`); return { binary, env: { KIND_EXPERIMENTAL_PROVIDER: name, PATH: `${dirname(binary)}${delimiter}${process.env.PATH ?? ""}` } }; }
async function ensureProvider(name: Provider) { const selected = await providerEnvironment(name); if (await attempt(selected.binary, ["info"])) return selected; if (name === "podman") await execute(selected.binary, ["machine", "start"]); if (!await attempt(selected.binary, ["info"])) throw new Error(`${name} is installed but unavailable`); return selected; }
async function selectProvider() {
	if (requestedProvider === "docker" || requestedProvider === "podman") return ensureProvider(requestedProvider);
	if (requestedProvider !== "auto") throw new Error("--provider must be auto, docker, or podman");
	for (const candidate of ["docker", "podman"] as const) { if (!providerBinary(candidate)) continue; try { return await ensureProvider(candidate); } catch {} }
	throw new Error("no usable Docker or Podman provider found");
}

function parseKubeconfig(source: string): Kubeconfig {
	// SAFETY: callers validate the parsed kubeconfig shape before using ownership or endpoint fields.
	return parse(source) as Kubeconfig;
}
function parseOwnership(source: string): Partial<Ownership> {
	// SAFETY: `ownershipFromDisk` validates every required ownership field before any filesystem action.
	return parse(source) as Partial<Ownership>;
}
async function readOwnership(file: string, kind: Ownership["kind"], dir: string, id: string, requireDefaultCni = kind === "dev") { await assertOwnedPathOnDisk(file, dir); const record = ownershipFromDisk(parseOwnership(await readFile(file, "utf8")), kind, dir, id, workspaceHash, requireDefaultCni); await assertOwnedOnDisk(record, kind, dir, id, workspaceHash); return record; }
async function clusterExists(kind: string, cluster: string, provider: Awaited<ReturnType<typeof ensureProvider>>) { const result = await attempt(kind, ["get", "clusters"], provider.env); return result?.stdout.split(/\r?\n/).includes(cluster) ?? false; }
const removals = new Map<string, Promise<void>>();
async function removeOwnedCluster(record: Ownership) {
	const dir = record.kind === "run" ? runDir(record.runId) : devDir; await assertOwnedOnDisk(record, record.kind, dir, record.runId, workspaceHash);
	const { kind } = await tools(); const provider = await ensureProvider(record.provider);
	if (await clusterExists(kind, record.cluster, provider)) await execute(kind, kindDeleteArgs(record.cluster, record.raw), provider.env);
	await rm(dir, { recursive: true, force: true });
}
async function removeCluster(record: Ownership) {
	const pending = removals.get(record.dir);
	if (pending) return pending;
	const removal = removeOwnedCluster(record);
	removals.set(record.dir, removal);
	try { await removal; } finally { removals.delete(record.dir); }
}

function finalKubeconfig(config: Kubeconfig, record: Ownership, token: string) {
	if (config.clusters?.length !== 1 || config.users?.length !== 1) throw new Error("unexpected Kind kubeconfig shape");
	return { apiVersion: config.apiVersion ?? "v1", kind: "Config", clusters: [{ name: record.cluster, cluster: config.clusters[0].cluster }], contexts: ["admin", "restricted"].map((role) => ({ name: `${record.cluster}-${role}`, context: { cluster: record.cluster, user: `${record.cluster}-${role}` } })), "current-context": `${record.cluster}-admin`, users: [{ name: `${record.cluster}-admin`, user: config.users[0].user }, { name: `${record.cluster}-restricted`, user: { token } }] };
}
function validateGeneratedConfig(config: Kubeconfig, record: Ownership) {
	const contexts = config.contexts?.map(({ name }) => name).sort() ?? [];
	const expected = [`${record.cluster}-admin`, `${record.cluster}-restricted`].sort();
	if (JSON.stringify(contexts) !== JSON.stringify(expected) || config.clusters?.length !== 1 || config.clusters[0].name !== record.cluster || config.contexts?.some(({ context }) => context.cluster !== record.cluster)) throw new Error("generated kubeconfig scope mismatch");
	const server = new URL(config.clusters[0].cluster.server ?? ""); if (!["127.0.0.1", "localhost", "::1"].includes(server.hostname)) throw new Error("generated API server is not loopback");
}
async function bootstrap(record: Ownership, kubectl: string) {
	await assertOwnedOnDisk(record, record.kind, record.dir, record.runId, workspaceHash);
	const rawEnv = { KUBECONFIG: record.raw };
	await execute(kubectl, ["apply", "-f", fixturePaths.rbac], rawEnv);
	const token = (await execute(kubectl, ["create", "token", "restricted", "-n", "e2e-system", "--duration=2h"], rawEnv, true)).stdout.trim();
	const config = finalKubeconfig(parseKubeconfig(await readFile(record.raw, "utf8")), record, token); validateGeneratedConfig(config, record);
	await writeFile(record.kubeconfig, stringify(config)); if (os !== "windows") await chmod(record.kubeconfig, 0o600);
}

async function applyFile(kubectl: string, file: string, env: Record<string, string | undefined>, serverSide = false) {
	await execute(kubectl, ["apply", ...(serverSide ? ["--server-side", "--force-conflicts"] : []), "-f", file], env);
}
async function verifyPlatformSources() {
	for (const [name, pin] of Object.entries(chartPins)) await verified(`platform-${name}-${pin.version}`, "repository" in pin ? pin.repository : pin.manifest, pin.sha256);
}
async function applyTemplate(record: Ownership, helm: string, kubectl: string, name: string, pin: typeof chartPins.cilium | typeof chartPins.argocd | typeof chartPins.traefik, namespace: string, values: string) {
	const chart = await verified(`${name}-${pin.version}.tgz`, pin.repository, pin.sha256);
	const rendered = await execute(helm, ["template", name, chart, "--namespace", namespace, "--values", values, "--include-crds"], { KUBECONFIG: record.kubeconfig }, true);
	const output = join(record.dir, `${name}.yaml`); await writeFile(output, rendered.stdout);
	await applyFile(kubectl, output, { KUBECONFIG: record.kubeconfig }, true);
}
async function materialize(record: Ownership, name: string, source: string, seedCommit = "") {
	const target = join(record.dir, name); const text = await readFile(source, "utf8");
	await writeFile(target, text.replaceAll("__KUBECOVE_GIT_REPO_URL__", gitRepositoryUrl).replaceAll("__KUBECOVE_SEED_COMMIT__", seedCommit));
	return target;
}
async function namespaceFile(record: Ownership, name: string) {
	const target = join(record.dir, `namespace-${name}.yaml`);
	await writeFile(target, `apiVersion: v1\nkind: Namespace\nmetadata:\n  name: ${name}\n`);
	return target;
}
async function seedRepository(record: Ownership, kubectl: string, env: Record<string, string | undefined>) {
	const metrics = await verified(`platform-metricsServer-${chartPins.metricsServer.version}`, chartPins.metricsServer.manifest, chartPins.metricsServer.sha256);
	const storage = await verified(`platform-localPath-${chartPins.localPath.version}`, chartPins.localPath.manifest, chartPins.localPath.sha256);
	const gitDaemonPin = gitDaemonPins[cpu];
	const gitDaemonPackage = await verified(`git-daemon-${gitDaemonPin.version}-${cpu}.apk`, gitDaemonPin.url, gitDaemonPin.sha256);
	const worktree = join(record.dir, "git-seed");
	await prepareGitSeed({ source: fixturePaths.gitops, destination: worktree, repositoryUrl: gitRepositoryUrl, metricsManifest: await readFile(metrics, "utf8"), storageManifest: await readFile(storage, "utf8") });
	for (const [name, pin] of [["argocd", chartPins.argocd], ["cilium", chartPins.cilium], ["traefik", chartPins.traefik]] as const) {
		const archive = await verified(`platform-${name}-${pin.version}`, pin.repository, pin.sha256);
		const destination = join(worktree, "vendor", name);
		await mkdir(destination, { recursive: true });
		await execute("tar", ["-xzf", archive, "-C", destination, "--strip-components=1"]);
	}
	for (const path of ["bootstrap", "platform/metrics", "platform/storage", "tenants/catalog", "tenants/ledger"]) await execute(kubectl, ["kustomize", join(worktree, path)], env, true);
	await execute("git", ["-C", worktree, "init", "--initial-branch=main"], {}, true);
	await execute("git", ["-C", worktree, "config", "user.name", gitSeedIdentity.name]);
	await execute("git", ["-C", worktree, "config", "user.email", gitSeedIdentity.email]);
	await execute("git", ["-C", worktree, "add", "."]);
	const gitDates = { GIT_AUTHOR_DATE: "2000-01-01T00:00:00Z", GIT_COMMITTER_DATE: "2000-01-01T00:00:00Z" };
	await execute("git", ["-C", worktree, "commit", "--no-gpg-sign", "-m", "Seed KubeCove production-shaped lab"], gitDates, true);
	const commit = (await execute("git", ["-C", worktree, "rev-parse", "HEAD"], {}, true)).stdout.trim();
	const bundle = join(record.dir, "repository.bundle");
	await execute("git", ["-C", worktree, "bundle", "create", bundle, "main"], {}, true);
	if ((await stat(bundle)).size > 1_000_000) throw new Error("verified Git seed exceeds the Kubernetes ConfigMap limit");
	const configMap = await execute(kubectl, ["create", "configmap", "gitops-repository", "-n", "e2e-system", `--from-file=repository.bundle=${bundle}`, "--dry-run=client", "-o", "yaml"], env, true);
	const configMapFile = join(record.dir, "gitops-repository.yaml");
	await writeFile(configMapFile, configMap.stdout);
	await writeFile(join(record.dir, "seed-commit.txt"), `${commit}\n`);
	await applyFile(kubectl, configMapFile, env, true);
	const packageConfigMap = await execute(kubectl, ["create", "configmap", "git-daemon-package", "-n", "e2e-system", `--from-file=git-daemon.apk=${gitDaemonPackage}`, "--dry-run=client", "-o", "yaml"], env, true);
	const packageConfigMapFile = join(record.dir, "git-daemon-package.yaml");
	await writeFile(packageConfigMapFile, packageConfigMap.stdout);
	await applyFile(kubectl, packageConfigMapFile, env, true);
	return commit;
}
async function waitApplication(kubectl: string, name: string, env: Record<string, string | undefined>) {
	await execute(kubectl, ["wait", "--for=jsonpath={.status.sync.status}=Synced", `application/${name}`, "-n", "argocd", "--timeout=300s"], env);
	await execute(kubectl, ["wait", "--for=jsonpath={.status.health.status}=Healthy", `application/${name}`, "-n", "argocd", "--timeout=300s"], env);
}
async function poll(description: string, timeoutMs: number, check: () => Promise<boolean>) {
	const deadline = Date.now() + timeoutMs;
	do {
		if (await check()) return;
		await Bun.sleep(1_000);
	} while (Date.now() < deadline);
	throw new Error(`timed out waiting for ${description}`);
}
async function applyLab(record: Ownership) {
	await assertOwnedOnDisk(record, record.kind, record.dir, record.runId, workspaceHash);
	const { kubectl, helm } = await tools(); const env = { KUBECONFIG: record.kubeconfig };
	await verifyPlatformSources();
	await applyTemplate(record, helm, kubectl, "cilium", chartPins.cilium, "kube-system", fixturePaths.ciliumValues);
	await execute(kubectl, ["rollout", "status", "daemonset/cilium", "-n", "kube-system", "--timeout=240s"], env);
	await execute(kubectl, ["rollout", "status", "deployment/cilium-operator", "-n", "kube-system", "--timeout=240s"], env);
	await execute(kubectl, ["wait", "--for=condition=Ready", "node", "--all", "--timeout=240s"], env);
	await execute(kubectl, ["rollout", "status", "deployment/coredns", "-n", "kube-system", "--timeout=240s"], env);
	await applyFile(kubectl, fixturePaths.all, env);
	await applyFile(kubectl, await namespaceFile(record, "argocd"), env);
	const seedCommit = await seedRepository(record, kubectl, env);
	await applyFile(kubectl, await materialize(record, "git-server.yaml", fixturePaths.gitServer, seedCommit), env);
	await execute(kubectl, ["rollout", "status", "deployment/git-server", "-n", "e2e-system", "--timeout=120s"], env);
	await attempt(kubectl, ["delete", "pod", "git-probe", "-n", "argocd", "--ignore-not-found"], env);
	await execute(kubectl, ["run", "git-probe", "-n", "argocd", "--image=alpine/git:v2.47.2@sha256:062a01ad7a0eb17cff382bc5e26086b4d710e56dfdfdf001109a49b6d9bd378c", "--restart=Never", "--command", "--", "git", "ls-remote", gitRepositoryUrl], env);
	await execute(kubectl, ["wait", "--for=jsonpath={.status.phase}=Succeeded", "pod/git-probe", "-n", "argocd", "--timeout=120s"], env);
	await execute(kubectl, ["delete", "pod", "git-probe", "-n", "argocd", "--wait=true"], env);
	await applyTemplate(record, helm, kubectl, "argocd", chartPins.argocd, "argocd", fixturePaths.argoCdValues);
	for (const deployment of ["argocd-server", "argocd-repo-server", "argocd-applicationset-controller"]) await execute(kubectl, ["rollout", "status", `deployment/${deployment}`, "-n", "argocd", "--timeout=240s"], env);
	await execute(kubectl, ["rollout", "status", "statefulset/argocd-application-controller", "-n", "argocd", "--timeout=240s"], env);
	await applyFile(kubectl, await materialize(record, "root-application.yaml", fixturePaths.rootApplication), env);
	await waitApplication(kubectl, "root-application", env);
	for (const name of platformApplicationNames) await waitApplication(kubectl, name, env);
	for (const name of tenantApplicationNames) await waitApplication(kubectl, name, env);
	await execute(kubectl, ["rollout", "status", "deployment/metrics-server", "-n", "kube-system", "--timeout=180s"], env);
	await poll("metrics API", 180_000, async () => Boolean(await attempt(kubectl, ["get", "--raw", "/apis/metrics.k8s.io/v1beta1/nodes"], env)));
	await poll("HPA metrics", 180_000, async () => (await attempt(kubectl, ["get", "hpa", "catalog", "-n", "tenant-catalog", "-o", "jsonpath={.status.conditions[?(@.type=='ScalingActive')].status}"], env))?.stdout === "True");
	await execute(kubectl, ["wait", "--for=jsonpath={.status.phase}=Bound", "pvc/data-ledger-0", "-n", "tenant-ledger", "--timeout=180s"], env);
	await execute(kubectl, ["rollout", "status", "statefulset/ledger", "-n", "tenant-ledger", "--timeout=180s"], env);
	await execute(kubectl, ["wait", "--for=condition=Ready", "pod/declared-client", "pod/isolated-client", "-n", "tenant-catalog", "--timeout=120s"], env);
	await execute(kubectl, ["exec", "-n", "tenant-catalog", "declared-client", "--", "wget", "-qO-", "--timeout=3", "http://catalog"], env, true);
	if (await attempt(kubectl, ["exec", "-n", "tenant-catalog", "isolated-client", "--", "wget", "-qO-", "--timeout=3", "http://catalog"], env)) throw new Error("Cilium failed to deny the isolated client");
	await attempt(kubectl, ["delete", "pod", "ingress-probe", "-n", "tenant-catalog", "--ignore-not-found"], env);
	await execute(kubectl, ["run", "ingress-probe", "-n", "tenant-catalog", "--image=busybox:1.36.1@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662", "--restart=Never", "--command", "--", "sh", "-ec", "wget -qO- --header='Host: catalog.e2e.local' http://traefik.traefik.svc.cluster.local | grep catalog-ingress-marker"], env);
	await execute(kubectl, ["wait", "--for=jsonpath={.status.phase}=Succeeded", "pod/ingress-probe", "-n", "tenant-catalog", "--timeout=120s"], env);
	await execute(kubectl, ["delete", "pod", "ingress-probe", "-n", "tenant-catalog", "--wait=true"], env);
	await execute(helm, ["upgrade", "--install", "operations", fixturePaths.chart, "-n", "operations", "--create-namespace"], env);
	await execute(kubectl, ["rollout", "status", "deployment/operations", "-n", "operations", "--timeout=120s"], env);
	await poll("expected CrashLoop incident", 120_000, async () => {
		const restarts = await attempt(kubectl, ["get", "pods", "-n", "operations", "-l", "app.kubernetes.io/name=operations-crashloop", "-o", "jsonpath={.items[0].status.containerStatuses[0].restartCount}"], env);
		const backoff = await attempt(kubectl, ["get", "events", "-n", "operations", "--field-selector=reason=BackOff", "-o", "name"], env);
		const logs = await attempt(kubectl, ["logs", "-n", "operations", "-l", "app.kubernetes.io/name=operations-crashloop", "--tail=5"], env);
		return Number(restarts?.stdout ?? "0") >= 2 && Boolean(backoff?.stdout.trim()) && logs?.stdout.includes("kubecove-crashloop-marker") === true;
	});
}

let current: Ownership | undefined;
async function create(kindName: Ownership["kind"]) {
	const dir = kindName === "run" ? runDir() : devDir; const id = kindName === "run" ? runId : workspaceHash; const file = recordPath(dir);
	if (kindName === "dev" && existsSync(file)) { const owned = await readOwnership(file, "dev", dir, id); const selected = await ensureProvider(owned.provider); const { kind, kubectl } = await tools(); if (await clusterExists(kind, owned.cluster, selected)) { current = owned; await bootstrap(owned, kubectl); await applyLab(owned); return owned; } await rm(dir, { recursive: true, force: true }); }
	if (existsSync(file)) throw new Error(`run ${runId} already exists`);
	const selected = await selectProvider(); const { kind, kubectl } = await tools(); const record: Ownership = { kind: kindName, runId: id, cluster: expectedCluster(kindName, id, workspaceHash), dir, raw: join(dir, "kind.raw.kubeconfig"), kubeconfig: join(dir, "kubeconfig"), dataDir: join(dir, "data"), kindConfig: join(dir, "kind.yaml"), disableDefaultCNI: true, provider: basename(selected.binary).startsWith("podman") ? "podman" : "docker", kubernetes };
	assertOwned(record, kindName, dir, id, workspaceHash); await mkdir(record.dataDir, { recursive: true }); await writeFile(record.kindConfig, kindConfig()); await writeFile(file, stringify(record)); await assertOwnedOnDisk(record, kindName, dir, id, workspaceHash); current = record;
	try { await execute(kind, ["create", "cluster", "--name", record.cluster, "--image", images[kubernetes], "--config", record.kindConfig, "--kubeconfig", record.raw], selected.env); await bootstrap(record, kubectl); await applyLab(record); return record; } catch (error) { if (kindName === "run") await diagnostics(record).catch((failure) => console.error("diagnostics failed", failure)); await removeCluster(record); current = undefined; throw error; }
}

async function safeArtifact(path: string, work: () => Promise<{ stdout: string; stderr: string } | undefined>) { try { const result = await work(); await writeFile(path, `${result?.stdout ?? ""}${result?.stderr ?? ""}`); } catch (error) { await writeFile(path, `diagnostic unavailable: ${String(error)}\n`); } }
async function diagnostics(record: Ownership) {
	await assertOwnedOnDisk(record, record.kind, record.dir, record.runId, workspaceHash);
	if (!existsSync(record.kubeconfig)) return;
	const artifacts = join(root, "e2e", "artifacts", record.runId); await mkdir(artifacts, { recursive: true }); const { kind, kubectl, helm } = await tools(); const env = { KUBECONFIG: record.kubeconfig };
	for (const [index, command] of safeDiagnosticCommands.entries()) await safeArtifact(join(artifacts, `status-${index}.txt`), async () => {
		const result = await attempt(kubectl, [...command], env);
		return result && { ...result, stdout: safeDiagnosticText(result.stdout), stderr: safeDiagnosticText(result.stderr) };
	});
	await safeArtifact(join(artifacts, "helm-releases.json"), async () => {
		const result = await attempt(helm, ["list", "--all-namespaces", "--output", "json"], env);
		return result && { ...result, stdout: safeDiagnosticText(result.stdout), stderr: safeDiagnosticText(result.stderr) };
	});
	const bytes = new Uint8Array(await readFile(record.kubeconfig)); const config = parseKubeconfig(new TextDecoder().decode(bytes));
	if (!config.contexts || !config.clusters?.[0]?.cluster.server) throw new Error("generated kubeconfig summary is incomplete");
	await writeFile(join(artifacts, "kubeconfig-summary.json"), JSON.stringify({ contexts: config.contexts.map(({ name }) => name), endpoint: new URL(config.clusters[0].cluster.server).host, loopback: true, sha256: sha256(bytes) }, null, 2));
	if (existsSync(join(record.dir, "seed-commit.txt"))) await writeFile(join(artifacts, "seed-commit.txt"), await readFile(join(record.dir, "seed-commit.txt")));
	const versions = await Promise.all([attempt(kind, ["version"]), attempt(kubectl, ["version", "--client", "-o", "yaml"]), attempt(helm, ["version", "--short"])]); await writeFile(join(artifacts, "versions.txt"), `${versions.map((value) => value?.stdout ?? "unavailable").join("\n")}\nimage ${imageFor(record.kubernetes)}\n`);
	await writeFile(join(artifacts, "verified-assets.json"), JSON.stringify({ ...Object.fromEntries(Object.entries(chartPins).map(([name, pin]) => [name, { version: pin.version, appVersion: "appVersion" in pin ? pin.appVersion : undefined, sha256: pin.sha256 }])), gitDaemon: { version: gitDaemonPins[cpu].version, architecture: cpu, sha256: gitDaemonPins[cpu].sha256 } }, null, 2));
}

let shuttingDown = false;
async function shutdown(signal: "SIGINT" | "SIGTERM") { if (shuttingDown) return; shuttingDown = true; for (const child of children) {
	if (profiling) await stopOwnedProfileProcess(child, signal);
	else child.kill(signal);
	} if (current?.kind === "run") { await diagnostics(current).catch((failure) => console.error("diagnostics failed", failure)); if (!keep) await removeCluster(current); } else if (current) await rm(current.dataDir, { recursive: true, force: true }); process.exit(signal === "SIGINT" ? 130 : 143); }
if (["run", "dev-up", "desktop-profile"].includes(action)) for (const signal of ["SIGINT", "SIGTERM"] as const) process.once(signal, () => void shutdown(signal));

async function fast() {
	if (!(await Array.fromAsync(new Bun.Glob("e2e/specs/fast/**/*.e2e.ts").scan({ cwd: root }))).length) throw new Error("fast suite has no specs");
	const { createServer } = await import("vite");
	const frontend = await createServer({ root, server: { host: "127.0.0.1", port: 1420, strictPort: true } });
	try {
		// Wait for Vite itself; HTTP polling can connect to its temporary port-check socket.
		await frontend.listen();
		await runWdio("e2e/wdio.fast.conf.ts", { KUBECOVE_E2E_ARTIFACTS: join(root, "e2e", "artifacts", "fast") });
	} finally { await frontend.close(); }
}
async function buildAndDrive(env: Record<string, string | undefined>, smoke = false) {
	const artifacts = env.KUBECOVE_E2E_ARTIFACTS;
	if (!artifacts) throw new Error("E2E artifacts directory is required");
	const targetDir = profiling ? join(stateDir, "profile-target") : join(root, "src-tauri", "target");
	const buildEnv = profiling ? { ...env, CARGO_TARGET_DIR: targetDir, KUBECOVE_PUBLIC_PROFILE: "true", KUBECOVE_PROFILE: "1" } : env;
	await execute("bun", ["run", "tauri", "build", ...(profiling ? [] : ["--debug"]), "--no-bundle", "--config", "src-tauri/tauri.e2e.conf.json", "--features", "e2e"], buildEnv, false, profiling ? undefined : join(artifacts, "build.log"));
	if (profiling) {
		await assertDesktopProfileIdle();
		await mkdir(artifacts, { recursive: true });
		await writeFile(join(artifacts, "frontend-bundle.json"), await readFile(join(stateDir, "reports", "frontend-bundle.json")));
		const sha = (await execute("git", ["rev-parse", "HEAD"], {}, true)).stdout.trim();
		buildEnv.KUBECOVE_PROFILE_SHA = /^[a-f0-9]{40,64}$/.test(sha) ? sha : undefined;
		buildEnv.KUBECOVE_PROFILE_DIRTY = (await execute("git", ["status", "--porcelain"], {}, true)).stdout.trim() ? "true" : "false";
	}
	await runWdio("e2e/wdio.real.conf.ts", { ...buildEnv, KUBECOVE_E2E_BINARY: join(targetDir, profiling ? "release" : "debug", `kubecove${suffix}`), KUBECOVE_E2E_SMOKE: smoke ? "1" : undefined }, profiling ? undefined : join(artifacts, "wdio.log"));
}
async function real() { if (keep && process.env.CI) throw new Error("--keep is forbidden in CI"); const record = await create("run"); const artifacts = join(root, "e2e", "artifacts", record.runId); const env = { KUBECOVE_E2E: "1", KUBECOVE_KUBECONFIG: record.kubeconfig, KUBECOVE_DATA_DIR: record.dataDir, KUBECOVE_E2E_CLUSTER: record.cluster, KUBECOVE_E2E_KUBECTL: (await tools()).kubectl, KUBECOVE_E2E_ARTIFACTS: artifacts }; try { await buildAndDrive(env); } finally { await diagnostics(record).catch((failure) => console.error("diagnostics failed", failure)); if (!keep) await removeCluster(record); current = undefined; } }
async function desktopSmoke() { if (profiling) await assertDesktopProfileIdle(); const dir = join(stateDir, "desktop-smoke", runId); const cluster = `kubecove-e2e-smoke-${runId}`; const kubeconfig = join(dir, "kubeconfig"); const dataDir = join(dir, "data"); const artifacts = join(root, "e2e", "artifacts", `desktop-${runId}`); if (profiling && (existsSync(dir) || existsSync(artifacts))) throw new Error("Profile run already exists; use a new run ID"); await mkdir(dataDir, { recursive: true }); const config = { apiVersion: "v1", kind: "Config", clusters: [{ name: cluster, cluster: { server: "https://127.0.0.1:65535" } }], contexts: ["admin", "restricted"].map((role) => ({ name: `${cluster}-${role}`, context: { cluster, user: `${cluster}-${role}` } })), "current-context": `${cluster}-admin`, users: ["admin", "restricted"].map((role) => ({ name: `${cluster}-${role}`, user: { token: "redacted-smoke-value" } })) }; await writeFile(kubeconfig, stringify(config)); const env = { KUBECOVE_E2E: "1", KUBECOVE_KUBECONFIG: kubeconfig, KUBECOVE_DATA_DIR: dataDir, KUBECOVE_E2E_CLUSTER: cluster, KUBECOVE_E2E_ARTIFACTS: artifacts }; try { await buildAndDrive(env, true); } finally { await rm(dir, { recursive: true, force: true }); } }
async function dev() { try { if ((await fetch("http://127.0.0.1:1430")).ok) throw new Error("an existing KubeCove dev server is running; dev:kind will not restart it"); } catch (error) { if (error instanceof Error && error.message.startsWith("an existing")) throw error; } const record = await create("dev"); await rm(record.dataDir, { recursive: true, force: true }); await mkdir(record.dataDir, { recursive: true }); try { await execute("bun", ["run", "tauri", "dev", "--config", "src-tauri/tauri.dev-kind.conf.json"], { KUBECONFIG: record.kubeconfig, KUBECOVE_DEV_KIND: "1", KUBECOVE_DATA_DIR: record.dataDir }); } finally { await rm(record.dataDir, { recursive: true, force: true }); current = undefined; } }

if (action === "fast") await fast();
else if (action === "run") await real();
else if (action === "desktop-smoke" || action === "desktop-profile") await desktopSmoke();
else if (action === "cleanup") { if (!requestedRunId) throw new Error("cleanup requires --run-id <id>"); const dir = runDir(requestedRunId); if (existsSync(recordPath(dir))) await removeCluster(await readOwnership(recordPath(dir), "run", dir, requestedRunId)); }
else if (action === "dev-up") await dev();
else if (action === "dev-down") { if (existsSync(recordPath(devDir))) await removeCluster(await readOwnership(recordPath(devDir), "dev", devDir, workspaceHash, false)); }
else throw new Error("use fast, run, desktop-smoke, desktop-profile, cleanup, dev-up, or dev-down");
