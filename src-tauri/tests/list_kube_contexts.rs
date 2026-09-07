use kubecove_lib::commands::{get_cluster_contexts, list_kube_contexts};
use kubecove_lib::models::AppErrorKind;
use kubecove_lib::models::ClusterContext;

fn assert_contexts_shape(contexts: &[ClusterContext]) {
    for ctx in contexts {
        assert!(!ctx.name.is_empty());
        let _: bool = ctx.is_current;
    }
}

#[test]
fn test_cluster_context_contract_without_kubeconfig() {
    let contexts = vec![ClusterContext {
        name: "minikube".to_string(),
        is_current: true,
    }];

    assert_contexts_shape(&contexts);
}

#[test]
fn test_list_kube_contexts_command() {
    if let Ok(contexts) = list_kube_contexts(None) {
        assert_contexts_shape(&contexts);
    }
}

#[test]
fn test_get_cluster_contexts_returns_cluster_contexts() {
    if let Ok(contexts) = get_cluster_contexts(None) {
        assert_contexts_shape(&contexts);
    }
}

#[test]
fn test_kubeconfig_errors_are_kubeconfig_errors() {
    if let Err(err) = get_cluster_contexts(None) {
        assert_eq!(err.kind, AppErrorKind::Kubeconfig);
        assert!(!err.message.is_empty());
    }
}
