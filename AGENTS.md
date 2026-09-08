# Agent Guide

This repository is for KubeCove, a local desktop Kubernetes workspace built with Tauri v2, Svelte, TypeScript, Bun, Rust, and `kube-rs`.

## Current Frontend

KubeCove uses Svelte as its frontend runtime.

### Bits UI changes require explicit permission

- Do not modify, remove, replace, or override Bits UI primitives, their styling or behavior, or local shared wrappers under `src/components/ui/svelte/` without the user's explicit permission for that change.
- A general request to fix or improve a screen or tooltip does not authorize changing shared Bits UI components. Explain the proposed shared change and obtain permission first.
- Use the existing public component API for feature-level changes. Do not bypass this rule with global CSS, dependency patches, replacement components, or indirect overrides.

- Default frontend work should fit the Svelte app and preserve typed Tauri command boundaries.
- Prefer shared typed helpers/models where they reduce drift, but avoid new abstractions unless they are needed for parity with product behavior or testability.

Before making implementation changes, skim the [docs index](docs/README.md). The minimum reading set is:

- `docs/product-vision.md`
- `docs/architecture-blueprint.md`
- `docs/milestones.md`
- `docs/development-workflow.md`
- `docs/decisions/` (current ADRs)

## Core Constraints

- Keep the current beta inspection-first outside ADR-approved live-session or operation paths.
- Do not deploy anything into Kubernetes clusters.
- Do not expose raw kubeconfig contents to the frontend.
- Do not let frontend code run arbitrary shell commands.
- Use `kube-rs` and the Kubernetes API for core list/get/discovery/watch flows.
- Pod and selector-backed Service port-forwarding follow ADR 0003. Broader cluster-changing workflows must follow ADR 0004: typed Rust-side commands, visible target scope, confirmation where needed, and permission-aware UX.
- Treat `kubectl`, Helm, and Argo CD CLIs as future optional sidecars or fallbacks, not as the core data path.
- Treat Argo CD as a native product area. Kubernetes CRDs and tracking metadata remain the default inspection path; opt-in connected HTTP API inspection and operations follow ADR 0013.
- Keep modules small and typed so future agent work can target focused files.

## Preferred Architecture

Rust-side Tauri commands own Kubernetes access. Current command families include:

- `list_kube_contexts`
- `list_namespaces`
- `list_resource_kinds`
- `list_resources`
- `list_dynamic_resources`
- `list_resource_scope`
- `get_resource_yaml`
- `get_resource_details`
- `list_resource_events`
- `list_resource_topology`
- `list_resource_metrics`
- stream commands for watches, events, and pod logs
- live-session commands for Pod and selector-backed Service port-forwarding
- guarded selected-resource YAML apply plus typed scale, rollout-restart, and exact-delete commands
- explicit connected Argo CD discovery, connection, inspection, comparison, preflight, and operation commands
- Argo CD, Helm, RBAC, and usage commands

The frontend apps should call only typed Tauri command wrappers. Kubernetes credentials and kubeconfig parsing belong in Rust modules under `src-tauri/src`.

## File Organization

The engineering handbook at [docs/handbook/](docs/handbook/) is the source of truth for code organization, file size, and hygiene. Skim its [README](docs/handbook/README.md) before making structural changes.

Quick rules:

- New files: feature-specific code goes in `src/features/<area>/`; generic reusables in `src/components/`; pure logic in `src/lib/`. Backend commands go in `src-tauri/src/commands/<domain>.rs` or a focused domain folder. Module READMEs at each folder describe what belongs where.
- File-size caps: `.rs` 500 soft / 800 hard; `.tsx` 400 soft / 700 hard; `.ts` 300 soft / 600 hard. The pre-commit hook warns at soft, fails at hard. See [file-size-and-split.md](docs/handbook/file-size-and-split.md).
- Orphan and superseded files are deleted in the PR that creates the orphan. No "for reference" copies, no stub re-exports. See [hygiene.md](docs/handbook/hygiene.md).
- A new top-level directory requires a one-line entry in [code-organization.md](docs/handbook/code-organization.md) before the directory is created.

Run through [docs/handbook/pr-checklist.md](docs/handbook/pr-checklist.md) before claiming work complete.

## Implementation Style

- Prefer stable, well-maintained libraries with strong ecosystem support. Avoid niche or experimental dependencies unless they clearly solve a project-specific problem.
- Use Bun for frontend package management, scripts, and app scaffolding unless a tool requires a different runner.
- Commit `bun.lock` when frontend dependencies exist.
- Do not introduce duplicate router, table, state, styling, or command-wrapper libraries without an ADR.
- Keep changes reviewable and scoped.
- Add tests or checks where risk justifies them.
- Use clear type names that match the docs unless there is a good reason to change them.
- Add comments only for non-obvious architecture decisions.
- Add TODOs for future features without implementing them early.

## Performance Defaults

- Treat CodSpeed as the primary performance signal for deterministic hot-path logic, especially Rust backend code.
- Prefer CodSpeed-covered Rust benchmarks for backend logic that is local and repeatable, such as resource summarization, topology shaping, YAML/diff/serialization helpers, discovery or resource grouping, cache-key normalization, and command payload transformation.
- Do not benchmark live Kubernetes API calls directly in CodSpeed. Use Settings -> Diagnostics latency reports for real Tauri command, cluster, network, and webview paths.
- Frontend pure hot paths continue to use `bun run bench` and the existing CodSpeed Vitest benchmark suites.
- For perf-sensitive changes, measure the relevant path before and after, optimize one measured bottleneck at a time, and re-measure the same benchmark or diagnostic path.
- Do not copy iteration syntax blindly. In measured or repeated hot paths, inspect collection size, call frequency, allocation, and early-exit behavior; prefer one `for...of` pass over allocation-heavy spread or chained `map`/`filter`/`flatMap` when behavior stays exact. Keep `some`/`find`/`every` where their short-circuit semantics improve clarity, and never use `for...in` for arrays.
- Add a new benchmark only when the changed hot path is not covered by existing benchmarks or diagnostics.

## Cross-Platform

KubeCove ships cross-platform: macOS (universal), Windows x64, and Linux x64 (see `.github/workflows/release.yml`). All changes must work on all three platforms, not just the one you are developing on.

- Never hardcode path separators, path-list separators, or home directories. Use `std::env::split_paths` for path lists (`:` vs `;`) and the existing helpers (e.g. `default_kubeconfig_path` handles `HOME` and `USERPROFILE`).
- Platform-specific code is gated with `#[cfg(target_os = "...")]` at the call site, but its helpers stay compiled on every platform so shared tests cover them — mark those helpers `#[allow(dead_code)]` with the standard comment (see `src-tauri/src/commands/usage_webview.rs`).
- Dead-code and lint results differ per OS: code that is "unused" on your machine may be the production path on another platform. Never delete platform-gated code because local clippy flags it; gate or annotate it instead.
- Frontend code must not assume platform-specific keyboard shortcuts (Cmd vs Ctrl), window chrome, or file-system casing behavior.
- The webview engine differs per platform: WKWebView/WebKit on macOS, WebView2/Chromium on Windows, WebKitGTK on Linux. Rendering-sensitive CSS (infinite animations, `filter`, compositing-heavy effects, especially inside transformed/scaled containers) must be verified on WebKit, not just on Windows — WebKit intermittently rasterizes animated-filter layers blank (see the topology glow fix).
- Prefer Tailwind utilities over bespoke CSS in `App.css`; reserve `App.css` for what utilities cannot express. This does not exempt engine bugs: Tailwind animations compile to the same CSS, so any infinite animation is a cross-platform risk. Keep them out of flow node subtrees, and prefer finite transitions or one-shot animations for state-change feedback.
- CI runs per-platform builds on release; local `cargo clippy`/`cargo test` only validates your current OS. Call out anything platform-sensitive in the PR so it gets attention on the other platforms.

## Tauri Command Contracts

- All frontend Kubernetes data must go through typed wrappers in `src/lib/tauri.ts`.
- Every new Tauri command needs a Rust serde model, a TypeScript type, and a typed frontend wrapper.
- Command errors must serialize into user-visible application errors.
- Do not add broad command payloads that leak kubeconfig, tokens, certificate data, or arbitrary filesystem contents.
- Live-session commands must follow [ADR 0003](docs/decisions/0003-guarded-live-sessions.md). Other cluster-changing commands must include explicit target scope and follow [ADR 0004](docs/decisions/0004-guarded-cluster-operations.md).

## Security-Sensitive Changes

Write an ADR before changing:

- the frontend/backend security boundary
- the JavaScript runtime or package manager
- the Kubernetes access path
- Tauri plugin permissions or capabilities
- guarded operation support for Kubernetes objects
- Argo CD API, CLI, sync, rollback, or diff integration beyond ADR 0013
- long-lived local persistence of cluster-derived data

## Product Inspiration

- Use K8Studio as a public feature benchmark for Kubernetes IDE capabilities.
- Use Aptakube as a public benchmark for clean, low-friction Kubernetes UX.
- Do not copy K8Studio or Aptakube code, assets, branding, proprietary layouts, or marketing text.
- Preserve this app's differentiator: context-first and namespace-first workflows.

## Verification

- Before launching or stopping KubeCove, check whether an app instance and its development server are already running. Reuse the existing instance by default; never close or restart a user-launched instance unless the user explicitly asks or a restart is technically required and has been explained first.
- Enable repo hooks with `git config core.hooksPath .githooks`.
- Run the relevant checks before claiming work is complete.
- Frontend changes should pass `bun run typecheck` and `bun run lint` (Biome).
- Rust backend changes should pass `cargo check --manifest-path src-tauri/Cargo.toml`.
- Kubernetes 1.34–1.36 is the tested rolling support window. ADR 0011 and the E2E/release workflow matrices are authoritative.
- If a check cannot run, state the exact blocker and what remains unverified.
- When completing scoped work, check off the corresponding items in `docs/milestones.md`.

## Skill Backlog

- Use `docs/agent-skills.md` as the source of truth for project-specific skill ideas.
- Do not create or install actual Codex skills from that backlog without a focused skill-writing and validation pass.
- Until those skills exist, apply the documented rules manually when working in matching areas.

## Agent skills

### Issue tracker

Issues and PRDs are tracked in GitHub Issues for `Timpan4/kubecove`. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the default five-label triage vocabulary. See `docs/agents/triage-labels.md`.

### Domain docs

This is a single-context repo; use the agent guide, product/architecture docs, and ADRs as the domain sources. See `docs/agents/domain.md`.

## Git Hygiene

- Use small commits.
- Avoid unrelated formatting churn.
- Do not rewrite user changes.
- Prefer branch names with the `codex/` prefix for agent-created feature branches.

## GitButler Workflow

Before any git write, run `git branch --show-current`.

If the current branch is `gitbutler/workspace`:

- Use GitButler mode.
- All git writes must use `but`; never use raw `git add`, `git commit`, `git push`, `git checkout`, `git rebase`, `git merge`, or `git stash`.
- Use `but diff --no-tui` for diffs.
- Never leave the workspace branch.
- If `but` is missing or unclear, run `but --help`; do not fall back to raw git writes.
- Before `but` commit, disable credit with `git config --local gitbutler.gitbutlerCommitter 0` unless requested otherwise.

Outside GitButler mode, normal non-destructive git rules apply. Do not force push, amend, rewrite history, or bypass force-push protection unless explicitly requested and safe.

Branch and commit naming:

- Branch names use `feat|fix|refactor|docs|test|chore|perf` plus a concrete 2-6 word kebab-case slug.
- Avoid vague names such as `tmp`, `misc`, `updates`, `stuff`, or ticket-only names.
- Do not add AI or coauthor attribution unless requested.
- Commit subjects should name one concrete outcome. An optional body may include 1-3 lines explaining why.
