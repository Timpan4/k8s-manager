use super::{binding_summary, role_summary, service_account_summary};
use crate::{
    commands::helpers::list_params,
    models::{AppError, RbacBindingSummary, RbacRoleSummary, ServiceAccountSummary},
};
use k8s_openapi::api::{
    core::v1::ServiceAccount,
    rbac::v1::{ClusterRole, ClusterRoleBinding, Role, RoleBinding},
};
use kube::{api::Api, Client, Resource};
use serde::de::DeserializeOwned;
use std::fmt::Debug;

pub(super) struct InventoryLoad<T> {
    pub(super) items: Vec<T>,
    pub(super) error: Option<AppError>,
}

impl<T> InventoryLoad<T> {
    fn complete(items: Vec<T>) -> Self {
        Self { items, error: None }
    }

    fn partial(items: Vec<T>, error: AppError) -> Self {
        Self {
            items,
            error: Some(error),
        }
    }

    fn map<U>(self, map: impl FnMut(T) -> U) -> InventoryLoad<U> {
        InventoryLoad {
            items: self.items.into_iter().map(map).collect(),
            error: self.error,
        }
    }
}

async fn list_paged<K>(api: &Api<K>) -> InventoryLoad<K>
where
    K: Clone + Debug + DeserializeOwned + Resource,
{
    let mut items = Vec::new();
    let mut token = None;
    loop {
        let params = token.as_deref().map_or_else(
            || list_params().limit(500),
            |value| list_params().limit(500).continue_token(value),
        );
        match api.list(&params).await {
            Ok(page) => {
                token = page.metadata.continue_;
                items.extend(page.items);
            }
            Err(error) => return InventoryLoad::partial(items, AppError::from(error)),
        }
        if token.as_deref().is_none_or(str::is_empty) {
            return InventoryLoad::complete(items);
        }
    }
}

pub(super) async fn list_service_accounts(
    client: Client,
    cluster: &str,
) -> InventoryLoad<ServiceAccountSummary> {
    let api: Api<ServiceAccount> = Api::all(client);
    list_paged(&api)
        .await
        .map(|item| service_account_summary(cluster, item))
}

pub(super) async fn list_roles(client: Client, cluster: &str) -> InventoryLoad<RbacRoleSummary> {
    let api: Api<Role> = Api::all(client);
    list_paged(&api)
        .await
        .map(|item| role_summary(cluster, "Role", item.metadata, item.rules))
}

pub(super) async fn list_cluster_roles(
    client: Client,
    cluster: &str,
) -> InventoryLoad<RbacRoleSummary> {
    let api: Api<ClusterRole> = Api::all(client);
    list_paged(&api)
        .await
        .map(|item| role_summary(cluster, "ClusterRole", item.metadata, item.rules))
}

pub(super) async fn list_role_bindings(
    client: Client,
    cluster: &str,
) -> InventoryLoad<RbacBindingSummary> {
    let api: Api<RoleBinding> = Api::all(client);
    list_paged(&api).await.map(|item| {
        binding_summary(
            cluster,
            "RoleBinding",
            item.metadata,
            item.role_ref.kind,
            item.role_ref.name,
            item.subjects,
        )
    })
}

pub(super) async fn list_cluster_role_bindings(
    client: Client,
    cluster: &str,
) -> InventoryLoad<RbacBindingSummary> {
    let api: Api<ClusterRoleBinding> = Api::all(client);
    list_paged(&api).await.map(|item| {
        binding_summary(
            cluster,
            "ClusterRoleBinding",
            item.metadata,
            item.role_ref.kind,
            item.role_ref.name,
            item.subjects,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_load_keeps_prior_items() {
        let load = InventoryLoad::partial(
            vec!["first"],
            AppError::new("page two forbidden", crate::models::AppErrorKind::Forbidden),
        );
        assert_eq!(load.items, vec!["first"]);
        assert_eq!(
            load.error.unwrap().kind,
            crate::models::AppErrorKind::Forbidden
        );
    }
}
