# Rust engineering audit

Reviewed on 2026-09-08 in the `t3code/1c5235a9` worktree.

## Scope and method

Read all 128 Rust files present at the start, including tests, benchmarks, build code, models, and feature-gated E2E code. Read the Cargo manifest, the Rust engineering skill and its eight engineering references, the repository handbook, product and architecture documents, and current ADRs. Reviewed both new Rust files and the terminal caller changes.

The contract was to fix demonstrated defects while preserving valid behavior, typed Tauri boundaries, inspection defaults, and existing operation confirmations. The review covered errors, numeric ranges, ingress validation, ownership, cancellation, task cleanup, resource admission, parser behavior, portability, and test evidence. A file marked "read" is coverage evidence, not a claim that every possible defect is absent.

## Changes and evidence

| Area | Defect and resulting behavior | Evidence |
| --- | --- | --- |
| Errors | `AppErrorKind` replaces free-form category strings. `AppError` implements `Display` and `Error`, retains cloneable backend sources, and still serializes only `message` and `kind`. Kubernetes status, transport, cancellation, metric, and permission classification use typed information. Native keyring, I/O, and serde failures retain sources where mapped into application errors. | Error category, misleading-message, cancellation-chain, serialized-payload, and credential-source regressions. |
| Kubernetes request scope | Raw path segments could redirect requests through caller-supplied names, namespaces, or discovery fields. Shared ingress validation rejects URI delimiters and inconsistent API resources before constructing Kubernetes requests. | Operation, dynamic resource, and apply path-redirection regressions failed before the fix and passed afterward. |
| Secrets | Dynamic Secret redaction trusted caller labels; the last-applied annotation could retain secret values. Redaction now follows the actual core-v1 Secret endpoint and covers last-applied metadata. Apply rejects effective v1 Secret targets, including inferred API versions. | Three Secret regressions failed before the fix and passed afterward. |
| Metrics and counts | Non-finite or negative quantities and overflowing sums could produce misleading values or panic. Checked sums return unavailable values; later samples cannot revive an overflowed total. Restart counts reject negative or overflowing totals. | Quantity, container total, workload total, and restart-count boundary tests. |
| Helm | Decoding could expand without a decoded-size limit and rejected legacy uncompressed Helm payloads. Decode failures remain visible when opening details or reconciliation; release listings retain metadata. | Oversized gzip and legacy JSON regressions failed before the fix, then passed with the storage suite. |
| Logs | Unbounded line reads could grow with remote input. Both log paths retain a bounded prefix, mark truncation, drain the rest, and resume at the next line. Departed sources release replay bookkeeping; actual owned task completion controls cleanup. | CRLF, EOF, UTF-8 boundary, oversized-line continuation, and departed-source regressions. |
| Exec | Input lacked bounded admission and acknowledged backpressure. Output/status tasks could outlive their owner or report successful exit after incomplete work. Input now has bounded byte admission, ordered acknowledgements, explicit rejection, and disposal on session changes. Output readers are owned and drained before exit reporting. Exact argv values survive backend validation. | Backend admission/lifecycle/argv tests and four frontend queue tests cover order, UTF-8 chunks, overflow, disposal, and failed writes. |
| Streams and port forwards | Late registration could discard a running task handle. Resource/event registration now rejects stale owners and aborts unused handles. Port-forward cleanup has an aborting owner on return or cancellation; diagnostic EOF does not end data transfer early. | Registry ownership regression and existing session tests; native dependency cleanup contracts inspected. Live-cluster cleanup remains unverified. |
| Watch recovery | Streamed HTTP 410 status events reset the resource version, but direct API 410 errors did not. Both resource and event watches now reset on typed API 410 errors too. | Existing mock reconnect test and new HTTP error classification test. |
| Kubeconform | Sequential stdin writes and output reads could deadlock, and dropping the future did not request child termination. Input and output now run concurrently with `kill_on_drop`. JSON status enums control results; null resource lists are valid, and incomplete validation remains visible. | A native child fixture writes output before consuming input; its parent test passes. Structured-status/null-resource regression failed before the parser fix and passed afterward. |
| Argo operations | Revalidation checked `DynamicObject.data`, which omits object metadata, and could reject a valid Kubernetes application. Revalidation now includes metadata and retains identity/version checks. | Kubernetes application identity regression failed before the fix and passed afterward. |
| YAML apply parsing | Single-object apply retained every document before rejecting multiple documents. Parsing now rejects after the second non-null document. | Existing multi-document, empty/identity, and apply validation coverage. |

The resource limits below were explicitly approved in this conversation:

- Helm decoded release payload: 32 MiB. The existing Kubernetes storage-object contract supplies the encoded-input ceiling.
- Log line retained payload: 256 KiB, with a visible truncation notice and remainder draining.
- Pending terminal input: 256 KiB, including an in-flight write. Frames use the existing 16 KiB stdin buffer size. The bounded command queue follows from those values.

These changes have cost-based justification, not measured latency claims. No benchmark speedup is claimed.

Helm compatibility was checked against [Helm's storage decoder](https://github.com/helm/helm/blob/v4.2.4/pkg/storage/driver/util.go) and the [Kubernetes Secret size contract](https://kubernetes.io/docs/concepts/configuration/secret/). Kubeconform parsing follows its [JSON output implementation](https://github.com/yannh/kubeconform/blob/v0.7.0/pkg/output/json.go).

## Verification

- Baseline full Rust tests: 340 passed, 1 ignored.
- Final full debug Rust tests: 338 unit tests and 26 integration tests passed; 2 ignored. The kubeconform child fixture is ignored as a standalone test but explicitly exercised by its passing parent test. The other ignored test requires a live cluster.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features --locked -- -D warnings`: passed after correcting the reported lints.
- `cargo fmt --manifest-path src-tauri/Cargo.toml`: completed.
- `bun run typecheck`: passed.
- `bun run svelte:check`: passed with zero errors and warnings.
- `bun run lint`: passed with five existing `App.css` warnings. New terminal-test lint notices were corrected; targeted Biome check passed.
- `bun test src/features/resource-detail/execInput.test.ts`: 4 passed.
- `git diff --check`: passed.
- Release numeric tests: all 9 metric tests and the restart-count boundary test passed.
- `cargo check --manifest-path src-tauri/Cargo.toml --locked`: passed on the final tree.
- `bun run docs:check`: passed.

The independent reviewer checked errors, Helm decoding, exec admission, port-forward ownership, URI validation, Secret redaction, and changed log reads. It found no actionable issue in that scope. Aggregate-log cleanup and kubeconform's final parser changes were reviewed locally and tested separately.

Red-before-green proof was run for the demonstrated parser, permission, numeric, Secret, URI, argv, Argo identity, and aggregate-log regressions noted above. It was not run for every source-preservation edit. Tests that would hang indefinitely on the old implementation were not left running to manufacture a failing result.

## Remaining gaps and deliberately excluded work

- No application-wide policy was supplied for concurrent sessions, outstanding cache loads, or total retained operation-session records. The approved per-item limits do not prove an aggregate memory bound. New global caps require an owner decision.
- Native desktop startup and live-cluster exec, port-forward, watch, and guarded Argo operation smoke tests were not run. No cluster was modified. macOS and Linux runtime behavior remain unverified from this Windows workspace.
- Existing workspace/settings writes still use direct file writes. Crash-atomic persistence was not implemented or claimed; it requires separate failure-injection and cross-platform replacement semantics.
- Legacy oversized Argo files remain exempt under the existing hook policy. A future split should move credential storage and connection state out of `connected.rs`, and separate operation revalidation/execution from `operations.rs`. Other soft-cap modules retain their existing responsibilities; broad reorganization was excluded from this behavior-preserving audit.
- Existing internal lock-poison invariants and string-only upstream protocol diagnostics were not converted into invented domain failures. The typed application boundary does not imply every diagnostic has a native source.
- No unsafe code was introduced. Miri, sanitizers, fuzzing, and performance benchmarks were not run; this audit makes no memory-safety or performance proof claim.
- No dependency versions, public commands, release configuration, or cluster-operation permissions changed.

## File coverage

Paths below are relative to `src-tauri`. "Updated" includes typed-error migrations and adjusted tests. The findings and verification limits above apply to the reviewed tree as a whole.

| File | Review result |
| --- | --- |
| [benches/backend_hot_paths.rs](../src-tauri/benches/backend_hot_paths.rs) | Read; unchanged |
| [build.rs](../src-tauri/build.rs) | Read; unchanged |
| [src/commands/argo/applications.rs](../src-tauri/src/commands/argo/applications.rs) | Read; updated |
| [src/commands/argo/appprojects.rs](../src-tauri/src/commands/argo/appprojects.rs) | Read; updated |
| [src/commands/argo/appsets.rs](../src-tauri/src/commands/argo/appsets.rs) | Read; updated |
| [src/commands/argo/comparison.rs](../src-tauri/src/commands/argo/comparison.rs) | Read; updated |
| [src/commands/argo/connected.rs](../src-tauri/src/commands/argo/connected.rs) | Read; updated |
| [src/commands/argo/mod.rs](../src-tauri/src/commands/argo/mod.rs) | Read; unchanged |
| [src/commands/argo/operations.rs](../src-tauri/src/commands/argo/operations.rs) | Read; updated |
| [src/commands/argo/scope.rs](../src-tauri/src/commands/argo/scope.rs) | Read; updated |
| [src/commands/argo/session.rs](../src-tauri/src/commands/argo/session.rs) | Read; updated |
| [src/commands/argo/transport.rs](../src-tauri/src/commands/argo/transport.rs) | Read; updated |
| [src/commands/argo/tunnel.rs](../src-tauri/src/commands/argo/tunnel.rs) | Read; updated |
| [src/commands/bench_support.rs](../src-tauri/src/commands/bench_support.rs) | Read; unchanged |
| [src/commands/cancellation.rs](../src-tauri/src/commands/cancellation.rs) | Read; updated |
| [src/commands/contexts.rs](../src-tauri/src/commands/contexts.rs) | Read; updated |
| [src/commands/diagnostics.rs](../src-tauri/src/commands/diagnostics.rs) | Read; updated |
| [src/commands/discovery.rs](../src-tauri/src/commands/discovery.rs) | Read; updated |
| [src/commands/events.rs](../src-tauri/src/commands/events.rs) | Read; updated |
| [src/commands/flux/mod.rs](../src-tauri/src/commands/flux/mod.rs) | Read; updated |
| [src/commands/gitops_crd.rs](../src-tauri/src/commands/gitops_crd.rs) | Read; updated |
| [src/commands/helm/manifest.rs](../src-tauri/src/commands/helm/manifest.rs) | Read; unchanged |
| [src/commands/helm/mod.rs](../src-tauri/src/commands/helm/mod.rs) | Read; unchanged |
| [src/commands/helm/reconciliation.rs](../src-tauri/src/commands/helm/reconciliation.rs) | Read; updated |
| [src/commands/helm/reconciliation_tests.rs](../src-tauri/src/commands/helm/reconciliation_tests.rs) | Read; unchanged |
| [src/commands/helm/redaction.rs](../src-tauri/src/commands/helm/redaction.rs) | Read; unchanged |
| [src/commands/helm/storage.rs](../src-tauri/src/commands/helm/storage.rs) | Read; updated |
| [src/commands/helm/storage_tests.rs](../src-tauri/src/commands/helm/storage_tests.rs) | Read; updated |
| [src/commands/helm/time.rs](../src-tauri/src/commands/helm/time.rs) | Read; unchanged |
| [src/commands/helm/values.rs](../src-tauri/src/commands/helm/values.rs) | Read; unchanged |
| [src/commands/helpers.rs](../src-tauri/src/commands/helpers.rs) | Read; updated |
| [src/commands/helpers/client_cache.rs](../src-tauri/src/commands/helpers/client_cache.rs) | Read; unchanged |
| [src/commands/helpers/client_cache_tests.rs](../src-tauri/src/commands/helpers/client_cache_tests.rs) | Read; unchanged |
| [src/commands/helpers/client_cancellation.rs](../src-tauri/src/commands/helpers/client_cancellation.rs) | Read; updated |
| [src/commands/helpers/health.rs](../src-tauri/src/commands/helpers/health.rs) | Read; updated |
| [src/commands/helpers/metadata.rs](../src-tauri/src/commands/helpers/metadata.rs) | Read; updated |
| [src/commands/helpers/metadata/flux_inventory.rs](../src-tauri/src/commands/helpers/metadata/flux_inventory.rs) | Read; updated |
| [src/commands/helpers/metadata/ownership.rs](../src-tauri/src/commands/helpers/metadata/ownership.rs) | Read; unchanged |
| [src/commands/helpers/serialization.rs](../src-tauri/src/commands/helpers/serialization.rs) | Read; updated |
| [src/commands/helpers/time.rs](../src-tauri/src/commands/helpers/time.rs) | Read; unchanged |
| [src/commands/helpers/validation.rs](../src-tauri/src/commands/helpers/validation.rs) | Added; reviewed |
| [src/commands/incidents.rs](../src-tauri/src/commands/incidents.rs) | Read; updated |
| [src/commands/incidents/health.rs](../src-tauri/src/commands/incidents/health.rs) | Read; unchanged |
| [src/commands/incidents/restarts.rs](../src-tauri/src/commands/incidents/restarts.rs) | Read; unchanged |
| [src/commands/kubeconfig.rs](../src-tauri/src/commands/kubeconfig.rs) | Read; updated |
| [src/commands/kubeconfig_clients.rs](../src-tauri/src/commands/kubeconfig_clients.rs) | Read; updated |
| [src/commands/kubeconfig_tests.rs](../src-tauri/src/commands/kubeconfig_tests.rs) | Read; updated |
| [src/commands/live_sessions.rs](../src-tauri/src/commands/live_sessions.rs) | Read; updated |
| [src/commands/live_store.rs](../src-tauri/src/commands/live_store.rs) | Read; updated |
| [src/commands/live_store_tests.rs](../src-tauri/src/commands/live_store_tests.rs) | Read; updated |
| [src/commands/metrics.rs](../src-tauri/src/commands/metrics.rs) | Read; updated |
| [src/commands/metrics_tests.rs](../src-tauri/src/commands/metrics_tests.rs) | Added; reviewed |
| [src/commands/mod.rs](../src-tauri/src/commands/mod.rs) | Read; unchanged |
| [src/commands/namespaces.rs](../src-tauri/src/commands/namespaces.rs) | Read; updated |
| [src/commands/operations.rs](../src-tauri/src/commands/operations.rs) | Read; updated |
| [src/commands/pod_exec.rs](../src-tauri/src/commands/pod_exec.rs) | Read; updated |
| [src/commands/pod_exec/registry.rs](../src-tauri/src/commands/pod_exec/registry.rs) | Read; updated |
| [src/commands/pod_exec/runner.rs](../src-tauri/src/commands/pod_exec/runner.rs) | Read; updated |
| [src/commands/pod_exec/tests.rs](../src-tauri/src/commands/pod_exec/tests.rs) | Read; updated |
| [src/commands/pod_exec/validation.rs](../src-tauri/src/commands/pod_exec/validation.rs) | Read; updated |
| [src/commands/rbac.rs](../src-tauri/src/commands/rbac.rs) | Read; unchanged |
| [src/commands/rbac_inventory.rs](../src-tauri/src/commands/rbac_inventory.rs) | Read; updated |
| [src/commands/rbac_review.rs](../src-tauri/src/commands/rbac_review.rs) | Read; updated |
| [src/commands/rbac_risk.rs](../src-tauri/src/commands/rbac_risk.rs) | Read; unchanged |
| [src/commands/rbac_tests.rs](../src-tauri/src/commands/rbac_tests.rs) | Read; unchanged |
| [src/commands/resources.rs](../src-tauri/src/commands/resources.rs) | Read; unchanged |
| [src/commands/resources/apply.rs](../src-tauri/src/commands/resources/apply.rs) | Read; unchanged |
| [src/commands/resources/apply/execution.rs](../src-tauri/src/commands/resources/apply/execution.rs) | Read; updated |
| [src/commands/resources/apply/linting.rs](../src-tauri/src/commands/resources/apply/linting.rs) | Read; unchanged |
| [src/commands/resources/apply/validation.rs](../src-tauri/src/commands/resources/apply/validation.rs) | Read; updated |
| [src/commands/resources/apply_client_tests.rs](../src-tauri/src/commands/resources/apply_client_tests.rs) | Read; updated |
| [src/commands/resources/details.rs](../src-tauri/src/commands/resources/details.rs) | Read; updated |
| [src/commands/resources/details/cluster.rs](../src-tauri/src/commands/resources/details/cluster.rs) | Read; updated |
| [src/commands/resources/details/core.rs](../src-tauri/src/commands/resources/details/core.rs) | Read; updated |
| [src/commands/resources/details/workloads.rs](../src-tauri/src/commands/resources/details/workloads.rs) | Read; updated |
| [src/commands/resources/dynamic.rs](../src-tauri/src/commands/resources/dynamic.rs) | Read; updated |
| [src/commands/resources/ingress_status.rs](../src-tauri/src/commands/resources/ingress_status.rs) | Read; unchanged |
| [src/commands/resources/kubeconform.rs](../src-tauri/src/commands/resources/kubeconform.rs) | Read; updated |
| [src/commands/resources/revisions.rs](../src-tauri/src/commands/resources/revisions.rs) | Read; updated |
| [src/commands/resources/scope.rs](../src-tauri/src/commands/resources/scope.rs) | Read; updated |
| [src/commands/resources/summary.rs](../src-tauri/src/commands/resources/summary.rs) | Read; updated |
| [src/commands/resources/summary_cluster.rs](../src-tauri/src/commands/resources/summary_cluster.rs) | Read; updated |
| [src/commands/resources/summary_core.rs](../src-tauri/src/commands/resources/summary_core.rs) | Read; updated |
| [src/commands/resources/summary_workloads.rs](../src-tauri/src/commands/resources/summary_workloads.rs) | Read; updated |
| [src/commands/resources/topology.rs](../src-tauri/src/commands/resources/topology.rs) | Read; updated |
| [src/commands/resources/topology_collection.rs](../src-tauri/src/commands/resources/topology_collection.rs) | Read; updated |
| [src/commands/resources/topology_dynamic.rs](../src-tauri/src/commands/resources/topology_dynamic.rs) | Read; updated |
| [src/commands/resources/topology_network.rs](../src-tauri/src/commands/resources/topology_network.rs) | Read; unchanged |
| [src/commands/resources/topology_tests.rs](../src-tauri/src/commands/resources/topology_tests.rs) | Read; unchanged |
| [src/commands/resources/yaml.rs](../src-tauri/src/commands/resources/yaml.rs) | Read; updated |
| [src/commands/sessions.rs](../src-tauri/src/commands/sessions.rs) | Read; updated |
| [src/commands/sessions/registry.rs](../src-tauri/src/commands/sessions/registry.rs) | Read; updated |
| [src/commands/sessions/runner.rs](../src-tauri/src/commands/sessions/runner.rs) | Read; updated |
| [src/commands/sessions/service.rs](../src-tauri/src/commands/sessions/service.rs) | Read; updated |
| [src/commands/sessions/target.rs](../src-tauri/src/commands/sessions/target.rs) | Read; updated |
| [src/commands/sessions/tests.rs](../src-tauri/src/commands/sessions/tests.rs) | Read; updated |
| [src/commands/streams.rs](../src-tauri/src/commands/streams.rs) | Read; updated |
| [src/commands/streams/aggregate_logs.rs](../src-tauri/src/commands/streams/aggregate_logs.rs) | Read; updated |
| [src/commands/streams/kinds.rs](../src-tauri/src/commands/streams/kinds.rs) | Read; updated |
| [src/commands/streams/logs.rs](../src-tauri/src/commands/streams/logs.rs) | Read; updated |
| [src/commands/streams/registry.rs](../src-tauri/src/commands/streams/registry.rs) | Read; updated |
| [src/commands/streams/watch.rs](../src-tauri/src/commands/streams/watch.rs) | Read; updated |
| [src/commands/usage.rs](../src-tauri/src/commands/usage.rs) | Read; updated |
| [src/commands/usage_webview.rs](../src-tauri/src/commands/usage_webview.rs) | Read; unchanged |
| [src/commands/workspace_files.rs](../src-tauri/src/commands/workspace_files.rs) | Read; updated |
| [src/e2e.rs](../src-tauri/src/e2e.rs) | Read; updated |
| [src/lib.rs](../src-tauri/src/lib.rs) | Read; unchanged |
| [src/main.rs](../src-tauri/src/main.rs) | Read; unchanged |
| [src/models/argo.rs](../src-tauri/src/models/argo.rs) | Read; updated |
| [src/models/cancellation.rs](../src-tauri/src/models/cancellation.rs) | Read; unchanged |
| [src/models/cluster.rs](../src-tauri/src/models/cluster.rs) | Read; unchanged |
| [src/models/diagnostics.rs](../src-tauri/src/models/diagnostics.rs) | Read; unchanged |
| [src/models/discovery.rs](../src-tauri/src/models/discovery.rs) | Read; unchanged |
| [src/models/error.rs](../src-tauri/src/models/error.rs) | Read; updated |
| [src/models/events.rs](../src-tauri/src/models/events.rs) | Read; unchanged |
| [src/models/flux.rs](../src-tauri/src/models/flux.rs) | Read; unchanged |
| [src/models/health.rs](../src-tauri/src/models/health.rs) | Read; unchanged |
| [src/models/helm.rs](../src-tauri/src/models/helm.rs) | Read; unchanged |
| [src/models/incidents.rs](../src-tauri/src/models/incidents.rs) | Read; unchanged |
| [src/models/metrics.rs](../src-tauri/src/models/metrics.rs) | Read; unchanged |
| [src/models/mod.rs](../src-tauri/src/models/mod.rs) | Read; updated |
| [src/models/namespace.rs](../src-tauri/src/models/namespace.rs) | Read; unchanged |
| [src/models/operations.rs](../src-tauri/src/models/operations.rs) | Read; unchanged |
| [src/models/rbac.rs](../src-tauri/src/models/rbac.rs) | Read; unchanged |
| [src/models/resource.rs](../src-tauri/src/models/resource.rs) | Read; unchanged |
| [src/models/sessions.rs](../src-tauri/src/models/sessions.rs) | Read; unchanged |
| [src/models/streams.rs](../src-tauri/src/models/streams.rs) | Read; unchanged |
| [src/models/usage.rs](../src-tauri/src/models/usage.rs) | Read; unchanged |
| [tests/list_kube_contexts.rs](../src-tauri/tests/list_kube_contexts.rs) | Read; updated |
| [tests/resource_models.rs](../src-tauri/tests/resource_models.rs) | Read; updated |

Also reviewed [Cargo.toml](../src-tauri/Cargo.toml), the changed [ExecTab.svelte](../src/features/resource-detail/ExecTab.svelte), and the added [execInput.ts](../src/features/resource-detail/execInput.ts) and [execInput.test.ts](../src/features/resource-detail/execInput.test.ts).
