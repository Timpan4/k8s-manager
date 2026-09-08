# Development Workflow

## Package Manager

Use Bun for frontend dependencies and scripts. Use version pinned by CI; dependency versions belong in lockfiles and manifests, not this guide.

```sh
bun install
bun run tauri dev
bun run typecheck
bun test
bun run docs:check
bun run check
```

## Linux native dependencies

Debian/Ubuntu native builds and headless desktop checks use the same packages as
Linux CI:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config dbus dbus-x11 gnome-keyring libayatana-appindicator3-dev libsecret-1-0 libwebkit2gtk-4.1-dev librsvg2-dev libssl-dev patchelf xvfb xauth
```

For example, missing `glib-2.0.pc` is a system dependency failure, not a Rust
source error. Verify with `pkg-config --modversion glib-2.0 gtk+-3.0 webkit2gtk-4.1`.

On small Linux VMs, Oxlint's JavaScript-plugin allocator can fail to reserve its
large aligned block ([upstream issue #20331](https://github.com/oxc-project/oxc/issues/20331)).
`lint:anti-slop` retries only that specific crash with jemalloc preloaded into the
linter process. It preserves all rules and the retry's exit status; the app and
profiler do not inherit the allocator. Install `libjemalloc2` on Debian/Ubuntu,
or extract the distribution package without root:

```sh
mkdir -p .e2e/tools/allocator
(cd .e2e/tools/allocator && apt-get download libjemalloc2 && dpkg-deb -x libjemalloc2_*.deb root)
bun run lint:anti-slop
```

## Browser Dev Mock Mode

`bun run tauri dev` starts Vite on Bun at `http://localhost:1430`. Opening it in a normal browser runs Svelte with browser-only mock Tauri responses for frontend inspection and automation.

Tauri webview uses real IPC and Rust Kubernetes commands. On Windows, development exposes Chrome DevTools Protocol at `http://127.0.0.1:9222`; set `KUBECOVE_DEVTOOLS_PORT` before launch to change it. Packaged apps do not expose this endpoint.

Browser mock mode never receives kubeconfig contents, calls a local Rust bridge, or accesses a real cluster. Treat all browser data as fake.

## Deterministic E2E and Kind Lab

Use fast E2E while changing frontend behavior:

```sh
bun run e2e:fast
```

It starts Vite on Bun, opens Chrome through WDIO, and uses typed development-browser mocks. It neither builds Rust nor contacts Kubernetes.

Use real E2E for native-command and cluster behavior:

```sh
bun run e2e:real -- --provider auto
bun run e2e:real -- --keep
bun run e2e:cleanup -- --run-id <id>
```

Runner downloads checksum-verified tools and assets, creates one uniquely named Kind cluster, builds E2E-only Tauri flavor, and runs native WDIO serially. It bootstraps production-shaped Cilium, Git, Argo CD, Helm, and tenant fixtures; fake GitOps CRDs and fabricated status are not fixtures.

Runner gathers allowlisted redacted diagnostics, then deletes only its exact cluster on success, failure, SIGINT, or SIGTERM. Local `--keep` preserves exact run for inspection and is rejected in CI. Cleanup requires recorded exact run ID; it never deletes by prefix or reads user kubeconfig.

Generated E2E kubeconfig contains only dedicated contexts. Startup rejects non-absolute paths, unexpected contexts, non-loopback API servers, cluster mismatches, persisted sources, and fallback to user default kubeconfig. Artifacts never include raw kubeconfig, tokens, keys, or certificate data.

For manual development against same fixtures:

```sh
bun run dev:kind
bun run dev:kind:down
bun run e2e:desktop-smoke
```

`dev:kind` uses a workspace-derived cluster name and temporary app profile. Cluster remains until `dev:kind:down`; app profile disappears on exit.

See [ADR 0010](decisions/0010-e2e-only-wdio-security-boundary.md), [ADR 0011](decisions/0011-rolling-kubernetes-support.md), and [ADR 0012](decisions/0012-production-shaped-e2e-lab.md).

## Verification Before Completion

Run smallest checks matching work:

- docs-only: `bun run docs:check` and `git diff --check`
- frontend: `bun run typecheck` and nearest `bun test`
- backend: `bun run rust:check` and nearest `bun run rust:test`
- behavior change: nearest focused test
- Tauri integration: `bun run tauri dev` or smoke test
- release change: `bun run release:dry-run`
- guarded operation: relevant ADR checklist plus nearest frontend/backend checks

If a check cannot run, record exact blocker and remaining unverified behavior.

## Startup and memory baselines

`bun run build` writes `.e2e/reports/frontend-bundle.json` outside the shipped
assets. `bun run perf:startup` builds the existing E2E desktop flavor in **release**
mode with the production minified frontend, then records an isolated launcher
scenario in `e2e/artifacts/desktop-<run-id>/startup.json`. Use `--run-id <id>` for an
explicit new run identifier. It creates no cluster, uses only the smoke runner's
synthetic loopback kubeconfig, and removes its temporary profile on completion.
It never reads the user's kubeconfig. Reports remain local and ignored by Git.

On headless Linux/WSL, run inside a disposable D-Bus/keyring and Xvfb session:

```sh
dbus-run-session -- bash -euc '
  eval "$(gnome-keyring-daemon --start --components=secrets)"
  dbus-send --session --print-reply --dest=org.freedesktop.secrets /org/freedesktop/secrets org.freedesktop.Secret.Service.SetAlias string:default objpath:/org/freedesktop/secrets/collection/session
  exec xvfb-run -a bun run perf:startup
'
```

The session keyring is temporary. On a small VM, set `CARGO_BUILD_JOBS=1` for the
build if necessary; record the setting when comparing build times.

The runner checks for KubeCove processes and occupied frontend/automation ports
before building and again before launch. It refuses an existing instance rather
than restarting it. The embedded WDIO service owns the app it starts; interrupt
cleanup signals only this run's process group (or exact process tree on Windows).
No production command, permission, or automation bridge is added. IPC timing is
compiled in only with `KUBECOVE_PUBLIC_PROFILE=true`; ordinary builds retain only
eight one-time User Timing marks. Runtime logs and screenshots are not profile
artifacts.

Readiness times are milliseconds from the WebView's `performance.timeOrigin`,
not from operating-system process creation. Marks record the first occurrence
per document and are reported in observed chronological order:

| Mark | Meaning |
| --- | --- |
| `frontend-entry` | The early entry dependency begins evaluation, before the Svelte application graph evaluates. Parsing/loading before this point is not isolated. |
| `svelte-mount` | Svelte's root `mount` returns with the initial DOM attached; this does not assert a painted frame. |
| `path-restored` | The app has read and applied the saved launcher/workspace path, or completed the no-saved-path branch. |
| `launcher-ready` | The launcher has successful kubeconfig-source and context results. It does not wait for namespace discovery in the creation form. |
| `workspace-ready` | The workspace shell mounts with its initial navigation state. This does not assert resource readiness. |
| `kubeconfig-ready` | The first successful typed `get_kubeconfig_sources` call settles. |
| `base-scope-ready` | The resource browser has source readiness and successful namespace and resource-kind discovery. |
| `first-resource-rows` | A visible table has non-placeholder successful resource data and at least one uncollapsed resource entry after the DOM update. Empty results and collapsed group headers do not count as rows. |

IPC summaries aggregate completed invocations by command name: count, errors,
total duration, and maximum duration. Time includes frontend serialization,
bridge scheduling, backend work, and response decoding; it is not a backend CPU
measurement. The first 1,000 completed calls are retained; `limitReached` exposes
truncation. Arguments, results, errors, resource names, and kubeconfig paths are
never recorded. The memory sample follows the launcher snapshot and uses the
existing `get_app_usage_metrics` contract. Host, WebView, and other-child resident
bytes are process groups, not allocation attribution; shared pages can be counted
more than once. Optional Chromium JS heap bytes are separate and must not be
added to process memory. Missing groups and unsupported heap inspection are
`null` with an explicit reason, never zero.

Bundle sizes include every emitted file, with input ownership and chunk imports.
Public assets are included alongside generated chunks and styles. Schema version 2
records source ownership without per-input byte estimates, which are not comparable
between bundlers. `rawBytes` is
emitted file size; gzip level 9 and Brotli quality 11 are deterministic per-file
compression comparisons, not installer sizes or measured network transfers.
Totals include deferred chunks. Reports exclude timestamps and absolute source
paths so identical builds can be compared directly.

Repeat measurements with the same source, lockfiles, build settings, OS, viewport,
and user path. Source SHA and dirty state are recorded. This initial scenario is
`launcher-fresh-profile`: it includes E2E automation overhead, does not flush OS
filesystem caches, and is not a packaged-installer or restored-workspace test.
Workspace, base-scope, and resource marks are explicitly `not-reached` here.
Do not interpret them as zero-cost or use this launcher run to choose a resource
retention policy. Windows and macOS/Linux WebKit results must be recorded
separately before making cross-platform performance claims.
