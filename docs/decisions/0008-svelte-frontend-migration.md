# ADR 0008: Parallel Svelte Frontend Migration

## Status

Accepted. Superseded by the completed Svelte cutover: Svelte is now the only frontend runtime.

The stack below records the migration-era decision. Current frontend development and production builds use Vite running on Bun with the official Svelte plugin.

## Context

KubeCove is a Tauri desktop app whose frontend is stateful: workspaces, selected cluster scope, resource inspection, YAML, topology, GitOps, Helm, RBAC, incidents, settings, live sessions, and diagnostics all share local UI state and typed Tauri command wrappers.

A Svelte topology spike showed promising manual performance and bundle-size signals. The spike was not enough to justify replacing the whole frontend by itself. The migration had to prove app-wide value and preserve the current security boundary.

Astro remains out of scope because KubeCove is a stateful desktop app shell, not a content site.

## Decision

KubeCove pursued the Svelte frontend migration as a parallel app before cutover.

The migration rules are:

- Build Svelte behind a user-visible Settings toggle during migration.
- Preserve existing settings and workspace storage schemas through cutover.
- Keep typed Tauri wrappers, TypeScript contract types, query keys, and pure resource/topology/workspace helpers shared.
- Do not change Tauri command names, payloads, Kubernetes access paths, or cluster-changing behavior as part of this migration.
- Do not embed old-runtime islands inside Svelte.

The planned Svelte stack is:

- `svelte`
- `@sveltejs/vite-plugin-svelte`
- `svelte-check`
- `@tanstack/svelte-query`
- `@tanstack/svelte-table`
- Bits UI for headless primitives
- `lucide-svelte`
- `@xyflow/svelte` for topology, if it reaches UX parity

## Cutover Gates

Svelte could become the default UI only when all of these were true:

- Daily core parity passes for launcher/workspaces, cluster and scope navigation, resource list, resource detail, YAML, topology, GitOps and Helm read paths, settings, and live-session list/status.
- App-wide launch, resource list, YAML, and topology measurements are better than or neutral to the previous runtime.
- At least one major app-wide bottleneck improves by 25% or more before the project continues beyond early migration slices.
- The runtime badge and Settings toggle make fallback state obvious during migration.

The previous runtime could be removed only after tail parity landed for command palette, incidents, RBAC, app updates, usage footer, and current edge states.

If `@xyflow/svelte` cannot match the required topology UX, Svelte cannot become the default UI. A custom topology renderer would need a separate decision.

## Consequences

The migration temporarily carried two frontend runtimes and duplicate UI shell code. That cost was intentional: it kept the shipped app usable while Svelte earned default status.

The migration was delivered through small vertical PRs. Each slice kept the shipped app usable until cutover.

The frontend remains an untrusted UI surface. Kubernetes credentials stay behind Rust-side typed Tauri commands, and future cluster-changing work remains governed by the existing guarded-operation ADRs.
