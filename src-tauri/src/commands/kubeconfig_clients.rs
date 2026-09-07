use super::kubeconfig::{KubeconfigSource, FINITE_REQUEST_READ_TIMEOUT};
#[cfg(test)]
use crate::models::AppErrorKind;
use crate::{
    commands::helpers::{client_cache, client_cancellation::RequestCancellationLayer},
    models::AppError,
};
use kube::{client::ClientBuilder, Client};
use std::future::Future;
use tokio_util::sync::CancellationToken;

async fn resolve_before_cancel<T, F>(
    future: F,
    cancellation: &CancellationToken,
) -> Result<T, AppError>
where
    F: Future<Output = Result<T, AppError>>,
{
    tokio::select! {
        result = future => result,
        () = cancellation.cancelled() => Err(AppError::cancelled()),
    }
}

impl KubeconfigSource {
    pub async fn client_for_context(&self, cluster_context: &str) -> Result<Client, AppError> {
        Ok(self.client_and_default_namespace(cluster_context).await?.0)
    }

    pub async fn client_and_default_namespace(
        &self,
        cluster_context: &str,
    ) -> Result<(Client, String), AppError> {
        let fingerprint = client_cache::fingerprint_files(&self.effective_kubeconfig_paths()?);
        let source_key = self.key();
        let generation = client_cache::finite_client_generation();
        if let Some(cached) =
            client_cache::lookup_client(&source_key, cluster_context, fingerprint, generation.id)
        {
            return Ok(cached);
        }
        let mut config =
            resolve_before_cancel(self.config_for_context(cluster_context), &generation.token)
                .await?;
        config.read_timeout = Some(FINITE_REQUEST_READ_TIMEOUT);
        let default_namespace = config.default_namespace.clone();
        let builder = ClientBuilder::try_from(config).map_err(AppError::from)?;
        let client = builder
            .with_layer(&RequestCancellationLayer::new(generation.token.clone()))
            .build();
        if !client_cache::store_client(
            source_key,
            cluster_context.to_string(),
            fingerprint,
            generation.id,
            client.clone(),
            default_namespace.clone(),
        ) {
            return Err(AppError::cancelled());
        }
        Ok((client, default_namespace))
    }

    pub async fn operation_client_for_context(
        &self,
        cluster_context: &str,
    ) -> Result<Client, AppError> {
        let fingerprint = client_cache::fingerprint_files(&self.effective_kubeconfig_paths()?);
        let source_key = self.key();
        if let Some(client) =
            client_cache::lookup_operation_client(&source_key, cluster_context, fingerprint)
        {
            return Ok(client);
        }
        let mut config = self.config_for_context(cluster_context).await?;
        config.read_timeout = Some(FINITE_REQUEST_READ_TIMEOUT);
        let client = Client::try_from(config).map_err(AppError::from)?;
        client_cache::store_operation_client(
            source_key,
            cluster_context.to_string(),
            fingerprint,
            client.clone(),
        );
        Ok(client)
    }

    pub async fn live_client_for_context(&self, cluster_context: &str) -> Result<Client, AppError> {
        Ok(self
            .live_client_and_default_namespace(cluster_context)
            .await?
            .0)
    }

    async fn live_client_and_default_namespace(
        &self,
        cluster_context: &str,
    ) -> Result<(Client, String), AppError> {
        let fingerprint = client_cache::fingerprint_files(&self.effective_kubeconfig_paths()?);
        let source_key = self.key();
        if let Some(cached) =
            client_cache::lookup_live_client(&source_key, cluster_context, fingerprint)
        {
            return Ok(cached);
        }
        let config = self.config_for_context(cluster_context).await?;
        let default_namespace = config.default_namespace.clone();
        let client = Client::try_from(config).map_err(AppError::from)?;
        client_cache::store_live_client(
            source_key,
            cluster_context.to_string(),
            fingerprint,
            client.clone(),
            default_namespace.clone(),
        );
        Ok((client, default_namespace))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustls_provider_is_unambiguous() {
        let config = kube::Config::new("https://127.0.0.1".parse().expect("valid test URL"));

        ClientBuilder::try_from(config).expect("Rustls should select exactly one crypto provider");
    }

    #[tokio::test]
    async fn cancellation_interrupts_pending_config_resolution() {
        let cancellation = CancellationToken::new();
        let waiting = resolve_before_cancel(
            std::future::pending::<Result<(), AppError>>(),
            &cancellation,
        );
        cancellation.cancel();

        let error = waiting.await.expect_err("resolution should be cancelled");

        assert_eq!(error.kind, AppErrorKind::Cancelled);
    }
}
