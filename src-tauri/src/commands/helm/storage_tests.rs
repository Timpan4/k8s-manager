use super::super::manifest::manifest_summary;
use super::super::values::values_summary;
use super::*;
use std::collections::BTreeMap;

#[test]
fn helm_owner_label_marks_release_storage() {
    let labels = BTreeMap::from([("owner".to_string(), "helm".to_string())]);

    assert!(is_helm_owned(Some(&labels)));
}

#[test]
fn missing_helm_owner_label_rejects_storage() {
    let labels = BTreeMap::from([("app".to_string(), "not-helm".to_string())]);

    assert!(!is_helm_owned(Some(&labels)));
    assert!(!is_helm_owned(None));
}

#[test]
fn values_summary_exposes_only_value_keys() {
    let config = serde_json::json!({
        "image": { "tag": "2026.5.22" },
        "replicaCount": 2,
    });

    let summary = values_summary(Some(&config));

    assert!(summary.has_values);
    assert_eq!(summary.value_count, 2);
    assert_eq!(summary.top_level_keys, vec!["image", "replicaCount"]);
}

#[test]
fn values_summary_treats_explicit_null_as_empty() {
    let summary = values_summary(Some(&serde_json::Value::Null));

    assert!(!summary.has_values);
    assert_eq!(summary.value_count, 0);
    assert!(summary.top_level_keys.is_empty());
}

#[test]
fn manifest_summary_extracts_resource_refs_without_manifest_body() {
    let manifest = r"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: payments-api
---
apiVersion: v1
kind: Service
metadata:
  name: payments-api
  namespace: payments
";

    let summary = manifest_summary(Some(manifest), Some("default"));

    assert_eq!(summary.resource_count, 2);
    assert!(!summary.truncated);
    assert_eq!(summary.resources[0].kind.as_deref(), Some("Deployment"));
    assert_eq!(summary.resources[0].namespace, None);
    assert_eq!(summary.resources[1].kind.as_deref(), Some("Service"));
    assert_eq!(summary.resources[1].namespace.as_deref(), Some("payments"));
}

#[test]
fn manifest_summary_preserves_missing_namespace_without_kind_guessing() {
    let manifest = r"
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: payments-reader
---
apiVersion: networking.k8s.io/v1
kind: IngressClass
metadata:
  name: internal
";

    let summary = manifest_summary(Some(manifest), Some("payments"));

    assert_eq!(summary.resource_count, 2);
    assert_eq!(summary.resources[0].kind.as_deref(), Some("ClusterRole"));
    assert_eq!(summary.resources[0].namespace, None);
    assert_eq!(summary.resources[1].kind.as_deref(), Some("IngressClass"));
    assert_eq!(summary.resources[1].namespace, None);
}

#[test]
fn rejects_decoded_release_over_approved_budget() {
    use std::io::Write;
    const LIMIT: usize = 32 * 1024 * 1024;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(b"{\"manifest\":\"").unwrap();
    let block = [b'x'; 1024];
    for _ in 0..LIMIT / block.len() {
        encoder.write_all(&block).unwrap();
    }
    encoder.write_all(b"\"}").unwrap();
    let payload = STANDARD.encode(encoder.finish().unwrap());
    let error = decode_helm_release(payload.as_bytes()).unwrap_err();
    assert_eq!(error.kind, AppErrorKind::Validation);
    assert!(error.message.contains("32 MiB"));
}

#[test]
fn supports_uncompressed_legacy_helm_release() {
    let encoded = STANDARD.encode(br#"{"name":"legacy","namespace":"default"}"#);
    let decoded = decode_helm_release(encoded.as_bytes()).expect("legacy release");
    assert_eq!(decoded.name.as_deref(), Some("legacy"));
}
