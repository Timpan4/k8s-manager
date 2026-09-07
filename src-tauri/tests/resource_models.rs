use kubecove_lib::models::AppErrorKind;
use kubecove_lib::models::{
    AppError, AppUsageMetrics, AppUsageMetricsBreakdown, ArgoAppProjectDetails,
    ArgoAppProjectSummary, ArgoApplicationDetails, ArgoApplicationSetDetails,
    ArgoApplicationSetSummary, ArgoApplicationSourceSummary, ArgoApplicationSummary,
    ClusterContext, DiscoveredResourceKind, HelmManifestResourceSummary, HelmManifestSummary,
    HelmReleaseDetails, HelmReleaseSummary, HelmValuesSummary, NamespaceSummary,
    PodExecConfirmation, PodExecSessionMessage, PodExecSessionRequest, PodExecSessionSummary,
    PodExecTerminalSize, PodLogStreamRequest, RbacInspectionSummary, RbacRiskIndicator,
    RbacRiskLevel, RbacRoleSummary, RbacRuleSummary, ResourceDetails, ResourceDetailsFull,
    ResourceEventSummary, ResourceHealth, ResourceMetricSummary, ResourceMetricsAvailability,
    ResourceMetricsAvailabilityStatus, ResourceMetricsSummary, ResourceSummary,
    ServiceAccountSummary, StreamMessage, WatchResourceKey, WatchResourceKind,
};
use serde_json::json;

#[test]
fn test_app_error_new() {
    let err = AppError::new("test message", AppErrorKind::Validation);
    assert_eq!(err.message, "test message");
    assert_eq!(err.kind, AppErrorKind::Validation);
}

#[test]
fn test_app_error_kube() {
    let err = AppError::new("kube error", AppErrorKind::Cluster);
    assert_eq!(err.message, "kube error");
    assert_eq!(err.kind, AppErrorKind::Cluster);
}

#[test]
fn test_cluster_context_serde() {
    let ctx = ClusterContext {
        name: "minikube".to_string(),
        is_current: true,
    };
    let json_str = serde_json::to_string(&ctx).unwrap();
    let parsed: ClusterContext = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.name, "minikube");
    assert!(parsed.is_current);
}

#[test]
fn test_namespace_summary_serde() {
    let ns = NamespaceSummary {
        name: "default".to_string(),
        age: "2024-01-01T00:00:00Z".to_string(),
        created_at: None,
    };
    let json_str = serde_json::to_string(&ns).unwrap();
    let parsed: NamespaceSummary = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.name, "default");
    assert_eq!(parsed.age, "2024-01-01T00:00:00Z");
}

#[test]
fn test_resource_summary_serde() {
    let rs = ResourceSummary {
        kind: "Pod".to_string(),
        cluster: "minikube".to_string(),
        name: "nginx".to_string(),
        namespace: Some("default".to_string()),
        age: "2024-01-01T00:00:00Z".to_string(),
        api_version: None,
        group: None,
        version: None,
        plural: None,
        namespaced: None,
        dynamic: None,
        health: ResourceHealth::Unknown,
        health_assessment: Default::default(),
        created_at: None,
        status: None,
        ready: None,
        restarts: None,
        owner_ref: None,
        argo_app: None,
        git_ops_owner: None,
        git_ops_ownership_partial: false,
        helm_release: None,
    };
    let json_str = serde_json::to_string(&rs).unwrap();
    let parsed: ResourceSummary = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.kind, "Pod");
    assert_eq!(parsed.namespace, Some("default".to_string()));
    assert_eq!(parsed.cluster, "minikube");
}

#[test]
fn test_resource_details_serde() {
    let rd = ResourceDetails {
        kind: "Pod".to_string(),
        cluster: "minikube".to_string(),
        name: "nginx".to_string(),
        namespace: Some("default".to_string()),
        yaml: "apiVersion: v1\nkind: Pod".to_string(),
    };
    let json_str = serde_json::to_string(&rd).unwrap();
    let parsed: ResourceDetails = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.yaml, "apiVersion: v1\nkind: Pod");
    assert_eq!(parsed.kind, "Pod");
}

#[test]
fn test_app_error_serialize() {
    let err = AppError::new("connection refused", AppErrorKind::Network);
    let json_val = serde_json::to_value(&err).unwrap();
    assert_eq!(json_val["message"], "connection refused");
    assert_eq!(json_val["kind"], "network");
}

#[test]
fn test_cluster_context_from_json() {
    let json_val = json!({ "name": "docker-desktop", "isCurrent": false });
    let ctx: ClusterContext = serde_json::from_value(json_val).unwrap();
    assert_eq!(ctx.name, "docker-desktop");
    assert!(!ctx.is_current);
}

#[test]
fn test_resource_details_full_serde() {
    let summary = ResourceSummary {
        kind: "Pod".to_string(),
        cluster: "minikube".to_string(),
        name: "nginx".to_string(),
        namespace: Some("default".to_string()),
        age: "2024-01-01T00:00:00Z".to_string(),
        api_version: None,
        group: None,
        version: None,
        plural: None,
        namespaced: None,
        dynamic: None,
        health: ResourceHealth::Unknown,
        health_assessment: Default::default(),
        created_at: None,
        status: None,
        ready: None,
        restarts: None,
        owner_ref: None,
        argo_app: None,
        git_ops_owner: None,
        git_ops_ownership_partial: false,
        helm_release: None,
    };
    let rdf = ResourceDetailsFull {
        summary,
        yaml: "apiVersion: v1\nkind: Pod".to_string(),
        metadata: serde_json::json!({ "name": "nginx", "namespace": "default" }),
        status: Some(serde_json::json!({ "phase": "Running" })),
    };
    let json_str = serde_json::to_string(&rdf).unwrap();
    let parsed: ResourceDetailsFull = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.yaml, "apiVersion: v1\nkind: Pod");
    assert_eq!(parsed.summary.kind, "Pod");
    assert!(parsed.status.is_some());
}

#[test]
fn test_resource_event_summary_serde() {
    let event = ResourceEventSummary {
        event_type: "Warning".to_string(),
        reason: "BackOff".to_string(),
        message: "Back-off restarting failed container".to_string(),
        count: 3,
        last_seen: "2m".to_string(),
        last_seen_at: None,
        source: "kubelet".to_string(),
        namespace: Some("default".to_string()),
    };
    let json_val = serde_json::to_value(&event).unwrap();
    assert_eq!(json_val["eventType"], "Warning");
    assert_eq!(json_val["reason"], "BackOff");
    let parsed: ResourceEventSummary = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.count, 3);
    assert_eq!(parsed.namespace, Some("default".to_string()));
}

#[test]
fn test_pod_exec_models_serde() {
    let request = PodExecSessionRequest {
        cluster_context: "kind-dev".to_string(),
        kubeconfig_env_var: None,
        namespace: "payments".to_string(),
        pod_name: "api-0".to_string(),
        container: Some("api".to_string()),
        command: vec!["/bin/sh".to_string()],
        stdin: true,
        tty: true,
        terminal_size: PodExecTerminalSize {
            cols: 100,
            rows: 32,
        },
        confirmation: PodExecConfirmation {
            acknowledged: true,
            target: "kind-dev/payments/Pod/api-0/container/api".to_string(),
            command: "[\"/bin/sh\"]".to_string(),
        },
    };
    let json_val = serde_json::to_value(&request).unwrap();
    assert_eq!(json_val["clusterContext"], "kind-dev");
    assert_eq!(json_val["terminalSize"]["cols"], 100);
    let parsed: PodExecSessionRequest = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.command, vec!["/bin/sh".to_string()]);

    let summary = PodExecSessionSummary {
        id: "pod-exec-1".to_string(),
        cluster_context: "kind-dev".to_string(),
        kubeconfig_env_var: None,
        kubeconfig_source_key: None,
        kubeconfig_source_label: None,
        namespace: "payments".to_string(),
        pod_name: "api-0".to_string(),
        container: Some("api".to_string()),
        command: vec!["/bin/sh".to_string()],
        stdin: true,
        tty: true,
        terminal_cols: 100,
        terminal_rows: 32,
        status: "running".to_string(),
        started_at: "2026-06-01T10:00:00Z".to_string(),
        finished_at: None,
        exit_code: None,
        last_error: None,
    };
    let message = PodExecSessionMessage::Started {
        session_id: summary.id.clone(),
        summary,
    };
    let json_val = serde_json::to_value(&message).unwrap();
    assert_eq!(json_val["type"], "started");
    assert_eq!(json_val["summary"]["podName"], "api-0");
}

#[test]
fn test_discovered_resource_kind_serde() {
    let kind = DiscoveredResourceKind {
        group: "apps".to_string(),
        version: "v1".to_string(),
        api_version: "apps/v1".to_string(),
        kind: "Deployment".to_string(),
        plural: "deployments".to_string(),
        short_names: vec!["deploy".to_string()],
        namespaced: true,
    };
    let json_val = serde_json::to_value(&kind).unwrap();
    assert_eq!(json_val["apiVersion"], "apps/v1");
    assert_eq!(json_val["shortNames"], json!(["deploy"]));
    assert_eq!(json_val["namespaced"], true);
    let parsed: DiscoveredResourceKind = serde_json::from_value(json_val.clone()).unwrap();
    assert_eq!(parsed.kind, "Deployment");
    let mut legacy = json_val;
    legacy
        .as_object_mut()
        .expect("discovered kind object")
        .remove("shortNames");
    let parsed: DiscoveredResourceKind = serde_json::from_value(legacy).unwrap();
    assert!(parsed.short_names.is_empty());
}

#[test]
fn test_crd_discovered_resource_kind_serde() {
    let kind = DiscoveredResourceKind {
        group: "apiextensions.k8s.io".to_string(),
        version: "v1".to_string(),
        api_version: "apiextensions.k8s.io/v1".to_string(),
        kind: "CustomResourceDefinition".to_string(),
        plural: "customresourcedefinitions".to_string(),
        short_names: vec!["crd".to_string()],
        namespaced: false,
    };
    let json_val = serde_json::to_value(&kind).unwrap();
    assert_eq!(json_val["apiVersion"], "apiextensions.k8s.io/v1");
    assert_eq!(json_val["plural"], "customresourcedefinitions");
    assert_eq!(json_val["namespaced"], false);
    let parsed: DiscoveredResourceKind = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.kind, "CustomResourceDefinition");
    assert!(!parsed.namespaced);
}

#[test]
fn test_dynamic_resource_summary_fields_serde() {
    let summary = ResourceSummary {
        kind: "Widget".to_string(),
        cluster: "kind-prod".to_string(),
        name: "sample".to_string(),
        namespace: Some("default".to_string()),
        age: "1h".to_string(),
        api_version: Some("example.com/v1".to_string()),
        group: Some("example.com".to_string()),
        version: Some("v1".to_string()),
        plural: Some("widgets".to_string()),
        namespaced: Some(true),
        dynamic: Some(true),
        health: ResourceHealth::Healthy,
        health_assessment: Default::default(),
        created_at: None,
        status: Some("Running".to_string()),
        ready: None,
        restarts: None,
        owner_ref: Some("sample-owner".to_string()),
        argo_app: None,
        git_ops_owner: None,
        git_ops_ownership_partial: false,
        helm_release: None,
    };

    let json_val = serde_json::to_value(&summary).unwrap();
    assert_eq!(json_val["apiVersion"], "example.com/v1");
    assert_eq!(json_val["dynamic"], true);
    assert_eq!(json_val["health"], "healthy");
    assert_eq!(json_val["plural"], "widgets");
    let parsed: ResourceSummary = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.group, Some("example.com".to_string()));
    assert_eq!(parsed.namespaced, Some(true));
    assert_eq!(parsed.owner_ref, Some("sample-owner".to_string()));
}

#[test]
fn test_argo_application_models_serde() {
    let summary = ArgoApplicationSummary {
        cluster: "kind-prod".to_string(),
        name: "payments".to_string(),
        age: "1h".to_string(),
        created_at: Some("2026-05-15T10:00:00Z".to_string()),
        resource_version: None,
        uid: None,
        namespace: Some("argocd".to_string()),
        project: Some("default".to_string()),
        sync_status: Some("Synced".to_string()),
        health_status: Some("Healthy".to_string()),
        health_assessment: Default::default(),
        destination_namespace: Some("payments".to_string()),
        destination_server: Some("https://kubernetes.default.svc".to_string()),
        source_repo: Some("https://example.invalid/repo.git".to_string()),
        source_revision: Some("main".to_string()),
        source_mode: Some("git".to_string()),
        source_count: None,
        sources: vec![ArgoApplicationSourceSummary {
            repo_url: Some("https://example.invalid/repo.git".to_string()),
            target_revision: Some("main".to_string()),
            resolved_revision: Some("abc123".to_string()),
            path: Some("apps/payments".to_string()),
            chart: None,
            source_mode: Some("git".to_string()),
            reference: None,
        }],
        resource_namespaces: vec!["payments".to_string()],
        tracked_resource_count: Some(3),
    };
    let details = ArgoApplicationDetails {
        summary,
        yaml: "kind: Application".to_string(),
        metadata: json!({ "name": "payments" }),
        status: Some(json!({ "sync": { "status": "Synced" } })),
    };

    let json_val = serde_json::to_value(&details).unwrap();
    assert_eq!(json_val["summary"]["syncStatus"], "Synced");
    assert_eq!(json_val["summary"]["sourceMode"], "git");
    assert_eq!(json_val["summary"]["sources"][0]["path"], "apps/payments");
    assert_eq!(json_val["summary"]["resourceNamespaces"][0], "payments");
    assert_eq!(json_val["summary"]["trackedResourceCount"], 3);
    let parsed: ArgoApplicationDetails = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.summary.health_status, Some("Healthy".to_string()));
    assert_eq!(parsed.summary.resource_namespaces, vec!["payments"]);
}

#[test]
fn test_argo_appset_models_serde() {
    let summary = ArgoApplicationSetSummary {
        cluster: "kind-prod".to_string(),
        name: "payments-set".to_string(),
        age: "2h".to_string(),
        created_at: None,
        namespace: Some("argocd".to_string()),
        project: Some("default".to_string()),
        status: Some("ResourcesUpToDate".to_string()),
        sync_status: Some("Synced".to_string()),
        health_status: Some("Healthy".to_string()),
        health_assessment: Default::default(),
        destination_namespace: Some("payments".to_string()),
        destination_server: Some("https://kubernetes.default.svc".to_string()),
        source_repo: Some("https://example.invalid/repo.git".to_string()),
        source_revision: Some("main".to_string()),
    };
    let details = ArgoApplicationSetDetails {
        summary,
        yaml: "kind: ApplicationSet".to_string(),
        metadata: json!({ "name": "payments-set" }),
    };

    let json_val = serde_json::to_value(&details).unwrap();
    assert_eq!(json_val["summary"]["healthStatus"], "Healthy");
    let parsed: ArgoApplicationSetDetails = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.summary.status, Some("ResourcesUpToDate".to_string()));
}

#[test]
fn test_argo_appproject_models_serde() {
    let summary = ArgoAppProjectSummary {
        cluster: "kind-prod".to_string(),
        name: "default".to_string(),
        age: "3h".to_string(),
        created_at: None,
        namespace: Some("argocd".to_string()),
        description: Some("default project".to_string()),
        status: Some("Healthy".to_string()),
        health_assessment: Default::default(),
    };
    let details = ArgoAppProjectDetails {
        summary,
        yaml: "kind: AppProject".to_string(),
        metadata: json!({ "name": "default" }),
    };

    let json_val = serde_json::to_value(&details).unwrap();
    assert_eq!(json_val["summary"]["description"], "default project");
    let parsed: ArgoAppProjectDetails = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.summary.name, "default");
}

#[test]
fn test_helm_release_models_serde() {
    let summary = HelmReleaseSummary {
        cluster: "kind-prod".to_string(),
        name: "payments".to_string(),
        namespace: "payments".to_string(),
        age: "5m".to_string(),
        updated_at: Some("2026-05-21T10:00:00Z".to_string()),
        created_at: Some("2026-05-21T09:55:00Z".to_string()),
        chart: Some("payments-1.2.3".to_string()),
        app_version: Some("2026.5.21".to_string()),
        revision: Some(7),
        status: Some("deployed".to_string()),
        storage_kind: "Secret".to_string(),
        storage_name: "sh.helm.release.v1.payments.v7".to_string(),
    };
    let details = HelmReleaseDetails {
        summary,
        yaml: "kind: Secret".to_string(),
        metadata: json!({ "name": "sh.helm.release.v1.payments.v7" }),
        values_summary: HelmValuesSummary {
            has_values: true,
            top_level_keys: vec!["image".to_string()],
            value_count: 1,
        },
        manifest_summary: HelmManifestSummary {
            resource_count: 1,
            resources: vec![HelmManifestResourceSummary {
                api_version: Some("apps/v1".to_string()),
                kind: Some("Deployment".to_string()),
                name: Some("payments".to_string()),
                namespace: Some("payments".to_string()),
            }],
            truncated: false,
        },
        release: Some(json!({ "name": "payments", "version": 7 })),
    };

    let json_val = serde_json::to_value(&details).unwrap();
    assert_eq!(json_val["summary"]["appVersion"], "2026.5.21");
    assert_eq!(json_val["summary"]["storageKind"], "Secret");
    assert_eq!(json_val["valuesSummary"]["topLevelKeys"][0], "image");
    assert_eq!(
        json_val["manifestSummary"]["resources"][0]["kind"],
        "Deployment"
    );
    let parsed: HelmReleaseDetails = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.summary.revision, Some(7));
    assert_eq!(parsed.summary.namespace, "payments");
    assert_eq!(parsed.values_summary.value_count, 1);
    assert_eq!(parsed.manifest_summary.resource_count, 1);
}

#[test]
fn test_rbac_models_serde() {
    let risk = RbacRiskIndicator {
        level: RbacRiskLevel::High,
        label: "Secrets access".to_string(),
        reason: "Rule can read or change Secret resources.".to_string(),
    };
    let role = RbacRoleSummary {
        cluster: "kind-dev".to_string(),
        kind: "Role".to_string(),
        name: "secret-reader".to_string(),
        namespace: Some("payments".to_string()),
        age: "1h".to_string(),
        created_at: None,
        rules_count: 1,
        risks: vec![risk.clone()],
        rules: vec![RbacRuleSummary {
            verbs: vec!["get".to_string(), "list".to_string()],
            api_groups: vec!["".to_string()],
            resources: vec!["secrets".to_string()],
            resource_names: Vec::new(),
            non_resource_urls: Vec::new(),
            risks: vec![risk.clone()],
        }],
    };
    let service_account = ServiceAccountSummary {
        cluster: "kind-dev".to_string(),
        name: "api".to_string(),
        namespace: "payments".to_string(),
        age: "1h".to_string(),
        created_at: None,
        automount_token: Some(false),
        secrets_count: 0,
        image_pull_secrets_count: 0,
        risks: Vec::new(),
    };
    let inspection = RbacInspectionSummary {
        cluster: "kind-dev".to_string(),
        refreshed_at: "2026-07-18T00:00:00Z".to_string(),
        warnings: vec!["ClusterRoles unavailable: forbidden by RBAC.".to_string()],
        coverage: Vec::new(),
        service_accounts: vec![service_account],
        roles: vec![role],
        cluster_roles: Vec::new(),
        role_bindings: Vec::new(),
        cluster_role_bindings: Vec::new(),
        namespace_access: Vec::new(),
    };

    let json_val = serde_json::to_value(&inspection).unwrap();
    assert_eq!(json_val["serviceAccounts"][0]["automountToken"], false);
    assert_eq!(
        json_val["warnings"][0],
        "ClusterRoles unavailable: forbidden by RBAC."
    );
    assert_eq!(json_val["roles"][0]["rulesCount"], 1);
    assert_eq!(json_val["roles"][0]["risks"][0]["level"], "high");
    let parsed: RbacInspectionSummary = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.roles[0].namespace, Some("payments".to_string()));
}

#[test]
fn test_stream_models_serde() {
    let key = WatchResourceKey {
        resource_kind: WatchResourceKind {
            kind: "Pod".to_string(),
            group: None,
            version: None,
            api_version: None,
            plural: None,
            namespaced: None,
        },
        namespace: Some("default".to_string()),
    };
    let json_val = serde_json::to_value(&key).unwrap();
    assert_eq!(json_val["resourceKind"]["kind"], "Pod");
    assert_eq!(json_val["namespace"], "default");

    let request = PodLogStreamRequest {
        cluster_context: "admin@solid-k8s".to_string(),
        kubeconfig_env_var: None,
        namespace: "default".to_string(),
        pod_name: "api-0".to_string(),
        container: Some("api".to_string()),
        tail_lines: Some(200),
        since_seconds: None,
    };
    let json_val = serde_json::to_value(&request).unwrap();
    assert_eq!(json_val["clusterContext"], "admin@solid-k8s");
    assert_eq!(json_val["podName"], "api-0");
    assert_eq!(json_val["tailLines"], 200);

    let message = StreamMessage::LogLine {
        stream_id: "logs-1".to_string(),
        line: "2026-05-18T10:00:00Z ready".to_string(),
        source: None,
    };
    let json_val = serde_json::to_value(&message).unwrap();
    assert_eq!(json_val["type"], "logLine");
    assert_eq!(json_val["streamId"], "logs-1");
}

#[test]
fn test_app_usage_metrics_serde() {
    let metrics = AppUsageMetrics {
        cpu_percent: 2.4,
        memory_bytes: 184 * 1024 * 1024,
        process_count: 3,
        sampled_at: "2026-05-20T10:00:00Z".to_string(),
        breakdown: vec![AppUsageMetricsBreakdown {
            label: "WebView".to_string(),
            description: "Embedded WebView browser runtime".to_string(),
            cpu_percent: 1.8,
            memory_bytes: 128 * 1024 * 1024,
            process_count: 2,
            children: vec![AppUsageMetricsBreakdown {
                label: "WebView process 1".to_string(),
                description: "Embedded WebView browser runtime".to_string(),
                cpu_percent: 1.2,
                memory_bytes: 96 * 1024 * 1024,
                process_count: 1,
                children: Vec::new(),
            }],
        }],
    };

    let json_val = serde_json::to_value(&metrics).unwrap();
    assert!((json_val["cpuPercent"].as_f64().unwrap() - 2.4).abs() < 0.001);
    assert_eq!(json_val["memoryBytes"], 184 * 1024 * 1024);
    assert_eq!(json_val["processCount"], 3);
    assert_eq!(json_val["breakdown"][0]["label"], "WebView");
    assert_eq!(
        json_val["breakdown"][0]["description"],
        "Embedded WebView browser runtime"
    );
    assert_eq!(
        json_val["breakdown"][0]["children"][0]["label"],
        "WebView process 1"
    );
    let parsed: AppUsageMetrics = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.process_count, 3);
    assert_eq!(parsed.breakdown[0].process_count, 2);
    assert_eq!(parsed.breakdown[0].children[0].process_count, 1);
}

#[test]
fn test_resource_metrics_models_serde() {
    let summary = ResourceMetricsSummary {
        cluster: "kind-dev".to_string(),
        availability: ResourceMetricsAvailability {
            status: ResourceMetricsAvailabilityStatus::Available,
            message: Some("metrics available".to_string()),
        },
        pods: vec![ResourceMetricSummary {
            kind: "Pod".to_string(),
            cluster: "kind-dev".to_string(),
            name: "api-0".to_string(),
            namespace: Some("payments".to_string()),
            cpu_millicores: Some(125.0),
            memory_bytes: Some(64 * 1024 * 1024),
            sampled_at: Some("2026-05-22T12:00:00Z".to_string()),
            source_pods: Vec::new(),
        }],
        nodes: Vec::new(),
        workloads: Vec::new(),
        warnings: Vec::new(),
    };

    let json_val = serde_json::to_value(&summary).unwrap();
    assert_eq!(json_val["availability"]["status"], "available");
    assert_eq!(json_val["pods"][0]["cpuMillicores"], 125.0);
    assert_eq!(json_val["pods"][0]["memoryBytes"], 64 * 1024 * 1024);
    let parsed: ResourceMetricsSummary = serde_json::from_value(json_val).unwrap();
    assert_eq!(parsed.pods[0].namespace, Some("payments".to_string()));
    assert_eq!(
        parsed.availability.status,
        ResourceMetricsAvailabilityStatus::Available
    );
}
