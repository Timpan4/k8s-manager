use super::*;
use crate::models::AppErrorKind;
use http::{header::CONTENT_TYPE, Method, Request, Response};
use kube::{client::Body, Client};
use serde_json::json;
use std::time::Duration;

type MockHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;

fn mock_client() -> (Client, MockHandle) {
    let (service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
    (Client::new(service, "default"), handle)
}

fn json_response(status: u16, body: serde_json::Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn service_manifest() -> serde_json::Value {
    json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": "api",
            "namespace": "default"
        },
        "spec": {
            "ports": [{ "port": 80 }]
        }
    })
}

fn base_request_yaml(force_conflicts: bool) -> YamlApplyRequest {
    YamlApplyRequest {
        cluster_context: "kind-kind".to_string(),
        kubeconfig_env_var: None,
        kind: "Service".to_string(),
        api_version: Some("v1".to_string()),
        group: None,
        version: None,
        plural: None,
        namespaced: Some(true),
        name: "api".to_string(),
        namespace: Some("default".to_string()),
        yaml: "apiVersion: v1\nkind: Service\nmetadata:\n  name: api\n  namespace: default\nspec:\n  ports:\n    - port: 80\n"
            .to_string(),
        yaml_encoding: crate::models::YamlEncoding::default(),
        force_conflicts,
    }
}

fn assert_service_path(request: &Request<Body>) {
    assert!(
        request
            .uri()
            .path()
            .ends_with("/namespaces/default/services/api"),
        "unexpected path: {}",
        request.uri().path()
    );
}

fn assert_has_query_param(request: &Request<Body>, param: &str) {
    let query = request.uri().query().unwrap_or_default();
    assert!(query.contains(param), "query missing {param}: {query}");
}

fn assert_lacks_query_param(request: &Request<Body>, param: &str) {
    let query = request.uri().query().unwrap_or_default();
    assert!(
        !query.contains(param),
        "query unexpectedly has {param}: {query}"
    );
}

async fn await_mock<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("mock client response sequence timed out")
}

#[tokio::test]
async fn prepare_issues_get_then_dry_run_patch() {
    let (client, mut handle) = mock_client();
    let validated = validate_yaml_apply(base_request_yaml(false)).unwrap();

    let operation = prepare_yaml_apply_with_client(client, validated);
    let responder = async move {
        let (request, send) = handle.next_request().await.expect("get request");
        assert_eq!(request.method(), Method::GET);
        assert_service_path(&request);
        send.send_response(json_response(200, service_manifest()));

        let (request, send) = handle.next_request().await.expect("patch request");
        assert_eq!(request.method(), Method::PATCH);
        assert_service_path(&request);
        assert_has_query_param(&request, "dryRun=All");
        assert_has_query_param(&request, "fieldManager=kubecove");
        send.send_response(json_response(200, service_manifest()));
    };

    let (preview, ()) = await_mock(async { tokio::join!(operation, responder) }).await;
    let preview = preview.unwrap();

    assert!(preview.current_yaml.contains("name: api"));
    assert!(preview.dry_run_yaml.contains("name: api"));
}

#[tokio::test]
async fn apply_issues_apply_patch_without_dry_run() {
    let (client, mut handle) = mock_client();
    let validated = validate_yaml_apply(base_request_yaml(false)).unwrap();

    let operation = apply_yaml_with_client(client, validated);
    let responder = async move {
        let (request, send) = handle.next_request().await.expect("patch request");
        assert_eq!(request.method(), Method::PATCH);
        assert_service_path(&request);
        assert_has_query_param(&request, "fieldManager=kubecove");
        assert_lacks_query_param(&request, "dryRun=");
        assert_lacks_query_param(&request, "force=true");
        assert_eq!(
            request.headers().get(CONTENT_TYPE).unwrap(),
            "application/apply-patch+yaml"
        );
        send.send_response(json_response(200, service_manifest()));
    };

    let (result, ()) = await_mock(async { tokio::join!(operation, responder) }).await;
    let result = result.unwrap();

    assert!(result.applied_yaml.contains("name: api"));
}

#[tokio::test]
async fn apply_force_conflicts_sets_force_param() {
    let (client, mut handle) = mock_client();
    let validated = validate_yaml_apply(base_request_yaml(true)).unwrap();

    let operation = apply_yaml_with_client(client, validated);
    let responder = async move {
        let (request, send) = handle.next_request().await.expect("patch request");
        assert_eq!(request.method(), Method::PATCH);
        assert_service_path(&request);
        assert_has_query_param(&request, "fieldManager=kubecove");
        assert_has_query_param(&request, "force=true");
        assert_lacks_query_param(&request, "dryRun=");
        send.send_response(json_response(200, service_manifest()));
    };

    let (result, ()) = await_mock(async { tokio::join!(operation, responder) }).await;

    assert!(result.unwrap().applied_yaml.contains("name: api"));
}

#[tokio::test]
async fn apply_classifies_field_manager_conflict() {
    let (client, mut handle) = mock_client();
    let validated = validate_yaml_apply(base_request_yaml(false)).unwrap();

    let operation = apply_yaml_with_client(client, validated);
    let responder = async move {
        let (request, send) = handle.next_request().await.expect("patch request");
        assert_eq!(request.method(), Method::PATCH);
        assert_service_path(&request);
        send.send_response(json_response(
            409,
            json!({
                "kind": "Status",
                "apiVersion": "v1",
                "metadata": {},
                "status": "Failure",
                "message": "Apply failed with conflicts",
                "reason": "Conflict",
                "details": {
                    "causes": [{
                        "reason": "FieldManagerConflict",
                        "message": "conflict with \"helm\"",
                        "field": ".spec.ports"
                    }]
                },
                "code": 409
            }),
        ));
    };

    let (result, ()) = await_mock(async { tokio::join!(operation, responder) }).await;
    let err = result.unwrap_err();

    assert_eq!(err.kind, AppErrorKind::FieldManagerConflict);
    assert_eq!(err.message, "Apply failed with conflicts");
}

#[tokio::test]
async fn apply_maps_forbidden_to_forbidden_kind() {
    let (client, mut handle) = mock_client();
    let validated = validate_yaml_apply(base_request_yaml(false)).unwrap();

    let operation = apply_yaml_with_client(client, validated);
    let responder = async move {
        let (request, send) = handle.next_request().await.expect("patch request");
        assert_eq!(request.method(), Method::PATCH);
        assert_service_path(&request);
        send.send_response(json_response(
            403,
            json!({
                "kind": "Status",
                "apiVersion": "v1",
                "metadata": {},
                "status": "Failure",
                "message": "services \"api\" is forbidden",
                "reason": "Forbidden",
                "code": 403
            }),
        ));
    };

    let (result, ()) = await_mock(async { tokio::join!(operation, responder) }).await;
    let err = result.unwrap_err();

    assert_eq!(err.kind, AppErrorKind::Forbidden);
    assert!(err.message.contains("services \"api\" is forbidden"));
}
