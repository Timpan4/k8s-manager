use super::topology::{
    build_resource_topology, input_from_metadata, node_from_input, topology_node_id,
    topology_root_kinds, topology_standalone_kinds, TopologyInputResource,
    DEPLOYMENT_REVISION_LABEL,
};
use super::topology_collection::cluster_scoped_input_visible_in_scope;
use super::topology_network::{
    build_network_flow_topology, NetworkEndpointSlice, NetworkIngressBackend, NetworkService,
    NetworkTopologyInputs,
};
use crate::commands::helpers::update_resource_health;
use crate::models::{
    GitOpsOwnerSummary, OwnerReferenceSummary, ResourceHealth, ResourceSummary, TopologyRelation,
};
use k8s_openapi::api::core::v1::Pod;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, RngSeed};
use std::collections::{BTreeMap, HashSet};

fn resource(kind: &str, name: &str, namespace: &str, uid: &str) -> TopologyInputResource {
    TopologyInputResource {
        uid: uid.to_string(),
        owner: None,
        labels: BTreeMap::new(),
        port_hints: Vec::new(),
        summary: ResourceSummary {
            kind: kind.to_string(),
            cluster: "kind-dev".to_string(),
            name: name.to_string(),
            namespace: Some(namespace.to_string()),
            age: "1m".to_string(),
            api_version: Some(
                match kind {
                    "Deployment" | "DaemonSet" | "ReplicaSet" | "StatefulSet" => "apps/v1",
                    "Job" | "CronJob" => "batch/v1",
                    "Ingress" => "networking.k8s.io/v1",
                    "EndpointSlice" => "discovery.k8s.io/v1",
                    _ => "v1",
                }
                .to_string(),
            ),
            group: None,
            version: None,
            plural: None,
            namespaced: Some(true),
            dynamic: None,
            health: ResourceHealth::Healthy,
            health_assessment: crate::models::HealthAssessment::default(),
            created_at: None,
            status: Some("Running".to_string()),
            ready: Some("true".to_string()),
            restarts: None,
            owner_ref: None,
            argo_app: None,
            helm_release: None,
            git_ops_owner: None,
            git_ops_ownership_partial: false,
        },
    }
}

fn cluster_resource(kind: &str, api_version: &str, name: &str, uid: &str) -> TopologyInputResource {
    let mut input = resource(kind, name, "", uid);
    input.summary.namespace = None;
    input.summary.api_version = Some(api_version.to_string());
    input.summary.namespaced = Some(false);
    input
}

fn labeled_pod(
    name: &str,
    namespace: &str,
    uid: &str,
    labels: &[(&str, &str)],
) -> TopologyInputResource {
    let mut pod = resource("Pod", name, namespace, uid);
    pod.labels = labels
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    pod.port_hints = vec!["http:8080".to_string()];
    pod
}

fn owned(
    kind: &str,
    name: &str,
    namespace: &str,
    uid: &str,
    owner_kind: &str,
    owner_name: &str,
    owner_uid: &str,
) -> TopologyInputResource {
    let mut input = resource(kind, name, namespace, uid);
    input.owner = Some(OwnerReferenceSummary {
        api_version: match owner_kind {
            "Deployment" | "DaemonSet" | "ReplicaSet" | "StatefulSet" => "apps/v1",
            "Job" | "CronJob" => "batch/v1",
            _ => "v1",
        }
        .to_string(),
        kind: owner_kind.to_string(),
        name: owner_name.to_string(),
        uid: owner_uid.to_string(),
    });
    input.summary.owner_ref = Some(owner_name.to_string());
    input
}

fn custom_resource(
    kind: &str,
    api_version: &str,
    name: &str,
    namespace: &str,
    uid: &str,
) -> TopologyInputResource {
    let mut input = resource(kind, name, namespace, uid);
    input.summary.api_version = Some(api_version.to_string());
    input.summary.group = api_version
        .split_once('/')
        .map(|(group, _)| group.to_string());
    input.summary.dynamic = Some(true);
    input
}

fn pod_fixture_input() -> TopologyInputResource {
    let pod: Pod = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/kubernetes/pod-running.json"
    ))
    .expect("fixture pod");
    let mut input = input_from_metadata("kind-dev", "Pod", "v1", &pod.metadata);
    if let Some(status) = pod.status {
        input.summary.status = status.phase.filter(|phase| !phase.is_empty());
        input.summary.ready = status
            .conditions
            .as_ref()
            .and_then(|conditions| {
                conditions
                    .iter()
                    .find(|condition| condition.type_ == "Ready")
            })
            .map(|condition| condition.status.clone());
        let restarts: i32 = status.container_statuses.as_ref().map_or(0, |statuses| {
            statuses
                .iter()
                .map(|container| container.restart_count)
                .sum()
        });
        if restarts > 0 {
            input.summary.restarts = Some(restarts);
        }
    }
    update_resource_health(&mut input.summary);
    input
}

#[test]
fn topology_root_kinds_include_daemonset() {
    assert!(topology_root_kinds().contains(&"DaemonSet"));
}

#[test]
fn topology_node_exposes_deployment_revision() {
    let mut input = resource("Deployment", "api", "default", "deploy-1");
    input
        .labels
        .insert(DEPLOYMENT_REVISION_LABEL.to_string(), "7".to_string());

    assert_eq!(node_from_input(&input).deployment_revision, Some(7));
}

#[test]
fn topology_standalone_kinds_include_ownerless_resources() {
    assert!(topology_standalone_kinds().contains(&"Service"));
    assert!(topology_standalone_kinds().contains(&"Ingress"));
    assert!(topology_standalone_kinds().contains(&"ConfigMap"));
    assert!(topology_standalone_kinds().contains(&"Secret"));
}

#[test]
fn ownership_topology_omits_auto_injected_root_ca_configmap() {
    let topology = build_resource_topology(vec![
        resource("ConfigMap", "kube-root-ca.crt", "default", "root-ca"),
        resource("ConfigMap", "app-config", "default", "app-config"),
    ]);

    assert_eq!(topology.nodes.len(), 1);
    assert_eq!(topology.nodes[0].kind, "ConfigMap");
    assert_eq!(topology.nodes[0].name, "app-config");
}

#[test]
fn ownership_topology_keeps_cluster_scoped_standalone_resources() {
    let topology = build_resource_topology(vec![
        cluster_resource(
            "CustomResourceDefinition",
            "apiextensions.k8s.io/v1",
            "certificates.cert-manager.io",
            "crd-certificates",
        ),
        cluster_resource(
            "StorageClass",
            "storage.k8s.io/v1",
            "csi-cinder-sc-retain",
            "storageclass-retain",
        ),
    ]);

    let node_names: HashSet<_> = topology
        .nodes
        .iter()
        .map(|node| {
            (
                node.kind.as_str(),
                node.name.as_str(),
                node.namespace.as_deref(),
            )
        })
        .collect();

    assert!(node_names.contains(&(
        "CustomResourceDefinition",
        "certificates.cert-manager.io",
        None
    )));
    assert!(node_names.contains(&("StorageClass", "csi-cinder-sc-retain", None)));
}

#[test]
fn scoped_topology_keeps_only_owned_cluster_scoped_inputs() {
    let namespaces = vec!["argocd".to_string()];
    let unmanaged = cluster_resource(
        "StorageClass",
        "storage.k8s.io/v1",
        "unmanaged",
        "storageclass-unmanaged",
    );
    let mut managed = cluster_resource(
        "StorageClass",
        "storage.k8s.io/v1",
        "csi-cinder-sc-retain",
        "storageclass-retain",
    );
    managed.summary.git_ops_owner = Some(GitOpsOwnerSummary {
        provider: "argo".to_string(),
        kind: "Application".to_string(),
        name: "cinder-csi".to_string(),
        namespace: Some("argocd".to_string()),
        confidence: "explicit".to_string(),
        provenance: "test-fixture".to_string(),
        partial: false,
    });

    assert!(!cluster_scoped_input_visible_in_scope(
        &unmanaged,
        &namespaces
    ));
    assert!(cluster_scoped_input_visible_in_scope(&managed, &namespaces));
    assert!(cluster_scoped_input_visible_in_scope(&unmanaged, &[]));
}

#[test]
fn sanitized_pod_fixture_normalizes_to_topology_contract() {
    let topology = build_resource_topology(vec![pod_fixture_input()]);
    let node = topology.nodes.first().expect("pod node");
    let summary_json = serde_json::to_string(&node.summary).expect("summary json");

    assert_eq!(node.kind, "Pod");
    assert_eq!(node.name, "payments-api-7d9-x");
    assert_eq!(node.namespace.as_deref(), Some("payments"));
    assert_eq!(node.status.as_deref(), Some("Running"));
    assert_eq!(node.health, ResourceHealth::Healthy);
    assert_eq!(node.summary.ready.as_deref(), Some("True"));
    assert_eq!(node.summary.owner_ref.as_deref(), Some("payments-api-7d9"));
    assert_eq!(node.summary.argo_app, None);
    assert_eq!(node.summary.helm_release.as_deref(), Some("payments-api"));
    assert!(!summary_json.contains("token"));
    assert!(!summary_json.contains("client-certificate-data"));
    assert!(!summary_json.contains("client-key-data"));
    assert!(!summary_json.contains("certificate-authority-data"));
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 50,
        rng_seed: RngSeed::Fixed(20_260_521),
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_topologies_keep_unique_nodes_and_valid_edges(pod_counts in proptest::collection::vec(0usize..4, 0..5)) {
        let mut inputs = vec![resource("Deployment", "api", "default", "deploy-0")];

        for (replica_set_index, pod_count) in pod_counts.iter().enumerate() {
            let replica_set_name = format!("api-{replica_set_index}");
            let replica_set_uid = format!("rs-{replica_set_index}");
            inputs.push(owned(
                "ReplicaSet",
                &replica_set_name,
                "default",
                &replica_set_uid,
                "Deployment",
                "api",
                "deploy-0",
            ));

            for pod_index in 0..*pod_count {
                let pod_name = format!("api-{replica_set_index}-{pod_index}");
                let pod_uid = format!("pod-{replica_set_index}-{pod_index}");
                inputs.push(owned(
                    "Pod",
                    &pod_name,
                    "default",
                    &pod_uid,
                    "ReplicaSet",
                    &replica_set_name,
                    &replica_set_uid,
                ));
            }
        }

        let topology = build_resource_topology(inputs.clone());
        let second_topology = build_resource_topology(inputs);
        let node_ids: HashSet<_> = topology.nodes.iter().map(|node| node.id.as_str()).collect();
        let node_order: Vec<_> = topology.nodes.iter().map(|node| node.id.as_str()).collect();
        let second_node_order: Vec<_> = second_topology.nodes.iter().map(|node| node.id.as_str()).collect();
        let edge_order: Vec<_> = topology.edges.iter().map(|edge| edge.id.as_str()).collect();
        let second_edge_order: Vec<_> = second_topology.edges.iter().map(|edge| edge.id.as_str()).collect();

        prop_assert_eq!(node_ids.len(), topology.nodes.len());
        prop_assert_eq!(node_order, second_node_order);
        prop_assert_eq!(edge_order, second_edge_order);
        for edge in &topology.edges {
            prop_assert!(node_ids.contains(edge.source.as_str()));
            prop_assert!(node_ids.contains(edge.target.as_str()));
        }
    }
}

#[test]
fn builds_deployment_to_replicaset_to_pod_edges_from_owner_uids() {
    let topology = build_resource_topology(vec![
        resource("Deployment", "api", "default", "deploy-1"),
        owned(
            "ReplicaSet",
            "api-7d9",
            "default",
            "rs-1",
            "Deployment",
            "api",
            "deploy-1",
        ),
        owned(
            "Pod",
            "api-7d9-x",
            "default",
            "pod-1",
            "ReplicaSet",
            "api-7d9",
            "rs-1",
        ),
    ]);

    let deploy_id = topology_node_id("kind-dev", "apps/v1", "Deployment", Some("default"), "api");
    let rs_id = topology_node_id(
        "kind-dev",
        "apps/v1",
        "ReplicaSet",
        Some("default"),
        "api-7d9",
    );
    let pod_id = topology_node_id("kind-dev", "v1", "Pod", Some("default"), "api-7d9-x");
    let replica_set = topology.nodes.iter().find(|node| node.id == rs_id).unwrap();
    assert!(replica_set.selectable);
    assert_eq!(replica_set.summary.kind, "ReplicaSet");
    assert_eq!(replica_set.summary.name, "api-7d9");
    assert_eq!(replica_set.summary.namespace.as_deref(), Some("default"));

    assert!(topology.edges.iter().any(|edge| {
        edge.source == deploy_id && edge.target == rs_id && edge.relation == TopologyRelation::Owns
    }));
    assert!(topology.edges.iter().any(|edge| {
        edge.source == rs_id && edge.target == pod_id && edge.relation == TopologyRelation::Owns
    }));
    assert!(topology.warnings.is_empty());
}

#[test]
fn builds_custom_resource_to_workload_edges_from_owner_uids() {
    let topology = build_resource_topology(vec![
        custom_resource(
            "Cluster",
            "postgresql.cnpg.io/v1",
            "main",
            "default",
            "cluster-1",
        ),
        owned(
            "StatefulSet",
            "main-rw",
            "default",
            "ss-1",
            "Cluster",
            "main",
            "cluster-1",
        ),
    ]);

    let cluster_id = topology_node_id(
        "kind-dev",
        "postgresql.cnpg.io/v1",
        "Cluster",
        Some("default"),
        "main",
    );
    let statefulset_id = topology_node_id(
        "kind-dev",
        "apps/v1",
        "StatefulSet",
        Some("default"),
        "main-rw",
    );

    assert!(topology.edges.iter().any(|edge| {
        edge.source == cluster_id
            && edge.target == statefulset_id
            && edge.relation == TopologyRelation::Owns
    }));
    assert!(topology.warnings.is_empty());
}

#[test]
fn deduplicates_nodes_by_resource_identity() {
    let topology = build_resource_topology(vec![
        resource("Service", "api", "default", "svc-1"),
        resource("Service", "api", "default", "svc-1"),
    ]);

    assert_eq!(topology.nodes.len(), 1);
    assert_eq!(topology.nodes[0].kind, "Service");
    assert_eq!(topology.nodes[0].name, "api");
    assert!(topology.edges.is_empty());
}

#[test]
fn builds_daemonset_to_pod_edges_from_owner_uids() {
    let topology = build_resource_topology(vec![
        resource("DaemonSet", "alloy", "default", "ds-1"),
        owned(
            "Pod",
            "alloy-bqpd5",
            "default",
            "pod-1",
            "DaemonSet",
            "alloy",
            "ds-1",
        ),
    ]);

    let daemonset_id =
        topology_node_id("kind-dev", "apps/v1", "DaemonSet", Some("default"), "alloy");
    let pod_id = topology_node_id("kind-dev", "v1", "Pod", Some("default"), "alloy-bqpd5");

    assert!(topology.edges.iter().any(|edge| {
        edge.source == daemonset_id
            && edge.target == pod_id
            && edge.relation == TopologyRelation::Owns
    }));
    assert!(topology.warnings.is_empty());
}

#[test]
fn builds_cronjob_to_job_to_pod_edges_from_owner_uids() {
    let topology = build_resource_topology(vec![
        resource("CronJob", "nightly", "default", "cron-1"),
        owned(
            "Job",
            "nightly-287",
            "default",
            "job-1",
            "CronJob",
            "nightly",
            "cron-1",
        ),
        owned(
            "Pod",
            "nightly-287-x",
            "default",
            "pod-1",
            "Job",
            "nightly-287",
            "job-1",
        ),
    ]);

    let cronjob_id = topology_node_id(
        "kind-dev",
        "batch/v1",
        "CronJob",
        Some("default"),
        "nightly",
    );
    let job_id = topology_node_id(
        "kind-dev",
        "batch/v1",
        "Job",
        Some("default"),
        "nightly-287",
    );
    let pod_id = topology_node_id("kind-dev", "v1", "Pod", Some("default"), "nightly-287-x");

    assert!(topology.edges.iter().any(|edge| {
        edge.source == cronjob_id
            && edge.target == job_id
            && edge.relation == TopologyRelation::Owns
    }));
    assert!(topology.edges.iter().any(|edge| {
        edge.source == job_id && edge.target == pod_id && edge.relation == TopologyRelation::Owns
    }));
    assert!(topology.warnings.is_empty());
}

#[test]
fn links_statefulset_to_matching_pvc_without_owner_reference() {
    let topology = build_resource_topology(vec![
        resource("StatefulSet", "postgres", "default", "ss-1"),
        owned(
            "Pod",
            "postgres-0",
            "default",
            "pod-1",
            "StatefulSet",
            "postgres",
            "ss-1",
        ),
        resource(
            "PersistentVolumeClaim",
            "data-postgres-0",
            "default",
            "pvc-1",
        ),
    ]);

    let ss_id = topology_node_id(
        "kind-dev",
        "apps/v1",
        "StatefulSet",
        Some("default"),
        "postgres",
    );
    let pvc_id = topology_node_id(
        "kind-dev",
        "v1",
        "PersistentVolumeClaim",
        Some("default"),
        "data-postgres-0",
    );

    assert!(topology.edges.iter().any(|edge| {
        edge.source == ss_id && edge.target == pvc_id && edge.relation == TopologyRelation::Creates
    }));
}

#[test]
fn warns_when_owner_reference_target_is_missing() {
    let topology = build_resource_topology(vec![owned(
        "Pod",
        "api-orphan",
        "default",
        "pod-1",
        "ReplicaSet",
        "api-missing",
        "rs-missing",
    )]);

    assert_eq!(topology.edges.len(), 0);
    assert_eq!(topology.warnings.len(), 1);
    assert!(topology.warnings[0].contains("ReplicaSet/api-missing"));
}

#[test]
fn marks_incomplete_ready_ratios_as_attention() {
    let mut deployment = resource("Deployment", "api", "default", "deploy-1");
    deployment.summary.ready = Some("0/3".to_string());
    deployment.summary.status = Some("Available: 0".to_string());
    update_resource_health(&mut deployment.summary);

    let topology = build_resource_topology(vec![deployment]);

    assert_eq!(topology.nodes[0].health, ResourceHealth::Attention);
}

#[test]
fn builds_network_flow_from_ingress_to_service_slice_and_pod() {
    let selector = BTreeMap::from([("app".to_string(), "api".to_string())]);
    let topology = build_network_flow_topology(NetworkTopologyInputs {
        resources: vec![
            resource("Ingress", "api", "default", "ing-1"),
            resource("Service", "api", "default", "svc-1"),
            resource("EndpointSlice", "api-abc", "default", "slice-1"),
            labeled_pod("api-7d9-x", "default", "pod-1", &[("app", "api")]),
        ],
        services: vec![NetworkService {
            namespace: "default".to_string(),
            name: "api".to_string(),
            service_type: "ClusterIP".to_string(),
            selector,
        }],
        ingress_backends: vec![NetworkIngressBackend {
            namespace: "default".to_string(),
            ingress_name: "api".to_string(),
            service_name: "api".to_string(),
        }],
        endpoint_slices: vec![NetworkEndpointSlice {
            namespace: "default".to_string(),
            name: "api-abc".to_string(),
            service_name: Some("api".to_string()),
            target_pods: vec!["api-7d9-x".to_string()],
        }],
        warnings: Vec::new(),
    });

    let ingress_id = topology_node_id(
        "kind-dev",
        "networking.k8s.io/v1",
        "Ingress",
        Some("default"),
        "api",
    );
    let service_id = topology_node_id("kind-dev", "v1", "Service", Some("default"), "api");
    let slice_id = topology_node_id(
        "kind-dev",
        "discovery.k8s.io/v1",
        "EndpointSlice",
        Some("default"),
        "api-abc",
    );
    let pod_id = topology_node_id("kind-dev", "v1", "Pod", Some("default"), "api-7d9-x");

    assert!(topology.edges.iter().any(|edge| {
        edge.source == ingress_id
            && edge.target == service_id
            && edge.relation == TopologyRelation::RoutesTo
    }));
    assert!(topology.edges.iter().any(|edge| {
        edge.source == service_id
            && edge.target == slice_id
            && edge.relation == TopologyRelation::Targets
    }));
    assert!(topology.edges.iter().any(|edge| {
        edge.source == slice_id
            && edge.target == pod_id
            && edge.relation == TopologyRelation::Targets
    }));
    assert!(topology.warnings.is_empty());
}

#[test]
fn network_flow_falls_back_to_service_selector_and_warns_on_misses() {
    let topology = build_network_flow_topology(NetworkTopologyInputs {
        resources: vec![
            resource("Service", "api", "default", "svc-1"),
            resource("Service", "missing", "default", "svc-2"),
            labeled_pod("api-0", "default", "pod-1", &[("app", "api")]),
        ],
        services: vec![
            NetworkService {
                namespace: "default".to_string(),
                name: "api".to_string(),
                service_type: "ClusterIP".to_string(),
                selector: BTreeMap::from([("app".to_string(), "api".to_string())]),
            },
            NetworkService {
                namespace: "default".to_string(),
                name: "missing".to_string(),
                service_type: "ClusterIP".to_string(),
                selector: BTreeMap::from([("app".to_string(), "missing".to_string())]),
            },
        ],
        ingress_backends: Vec::new(),
        endpoint_slices: Vec::new(),
        warnings: Vec::new(),
    });

    let service_id = topology_node_id("kind-dev", "v1", "Service", Some("default"), "api");
    let pod_id = topology_node_id("kind-dev", "v1", "Pod", Some("default"), "api-0");

    assert!(topology.edges.iter().any(|edge| {
        edge.source == service_id
            && edge.target == pod_id
            && edge.relation == TopologyRelation::Selects
    }));
    assert!(topology
        .warnings
        .iter()
        .any(|warning| warning.contains("Service default/missing has no matching Pod endpoints")));
}

#[test]
fn network_flow_skips_missing_endpoint_warning_for_external_name_service() {
    let topology = build_network_flow_topology(NetworkTopologyInputs {
        resources: vec![resource("Service", "external-api", "default", "svc-1")],
        services: vec![NetworkService {
            namespace: "default".to_string(),
            name: "external-api".to_string(),
            service_type: "ExternalName".to_string(),
            selector: BTreeMap::new(),
        }],
        ingress_backends: Vec::new(),
        endpoint_slices: Vec::new(),
        warnings: Vec::new(),
    });

    assert_eq!(topology.nodes.len(), 1);
    assert_eq!(topology.nodes[0].kind, "Service");
    assert!(topology.warnings.is_empty());
}

#[test]
fn network_flow_skips_missing_pod_warning_for_selectorless_endpoint_slices() {
    let topology = build_network_flow_topology(NetworkTopologyInputs {
        resources: vec![
            resource("Service", "api", "default", "svc-1"),
            resource("EndpointSlice", "api-manual", "default", "slice-1"),
        ],
        services: vec![NetworkService {
            namespace: "default".to_string(),
            name: "api".to_string(),
            service_type: "ClusterIP".to_string(),
            selector: BTreeMap::new(),
        }],
        ingress_backends: Vec::new(),
        endpoint_slices: vec![NetworkEndpointSlice {
            namespace: "default".to_string(),
            name: "api-manual".to_string(),
            service_name: Some("api".to_string()),
            target_pods: Vec::new(),
        }],
        warnings: Vec::new(),
    });

    let service_id = topology_node_id("kind-dev", "v1", "Service", Some("default"), "api");
    let slice_id = topology_node_id(
        "kind-dev",
        "discovery.k8s.io/v1",
        "EndpointSlice",
        Some("default"),
        "api-manual",
    );

    assert!(topology.edges.iter().any(|edge| {
        edge.source == service_id
            && edge.target == slice_id
            && edge.relation == TopologyRelation::Targets
    }));
    assert!(topology.warnings.is_empty());
}
