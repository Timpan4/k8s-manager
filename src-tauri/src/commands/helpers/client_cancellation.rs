use kube::client::Body;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio_util::sync::CancellationToken;
use tower::{BoxError, Layer, Service};

#[derive(Clone)]
pub(crate) struct RequestCancellationLayer {
    token: CancellationToken,
}

impl RequestCancellationLayer {
    pub(crate) fn new(token: CancellationToken) -> Self {
        Self { token }
    }
}

impl<S> Layer<S> for RequestCancellationLayer {
    type Service = RequestCancellationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestCancellationService {
            inner,
            token: self.token.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RequestCancellationService<S> {
    inner: S,
    token: CancellationToken,
}

use crate::models::WorkspaceRequestCancelled;

impl<S, B> Service<http::Request<Body>> for RequestCancellationService<S>
where
    S: Service<http::Request<Body>, Response = http::Response<B>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    B: Send + 'static,
{
    type Response = http::Response<B>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let future = self.inner.call(request);
        let token = self.token.clone();
        Box::pin(async move {
            tokio::select! {
                result = future => result.map_err(Into::into),
                () = token.cancelled() => Err(Box::new(WorkspaceRequestCancelled) as BoxError),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future;
    use http::Response;
    use tower::{service_fn, ServiceExt};

    #[tokio::test]
    async fn cancels_pending_service_request() {
        let token = CancellationToken::new();
        let service = service_fn(|_: http::Request<Body>| async {
            future::pending::<Result<Response<Body>, BoxError>>().await
        });
        let mut service = RequestCancellationLayer::new(token.clone()).layer(service);
        token.cancel();

        let error = service
            .ready()
            .await
            .expect("service ready")
            .call(http::Request::new(Body::empty()))
            .await
            .expect_err("request should be cancelled");

        assert!(error.is::<WorkspaceRequestCancelled>());
    }

    #[tokio::test]
    async fn replacement_token_does_not_cancel_with_previous_generation() {
        let previous = CancellationToken::new();
        let replacement = CancellationToken::new();
        previous.cancel();
        let service = service_fn(|_: http::Request<Body>| async {
            Ok::<_, BoxError>(Response::new(Body::empty()))
        });
        let response = RequestCancellationLayer::new(replacement)
            .layer(service)
            .oneshot(http::Request::new(Body::empty()))
            .await;

        assert!(response.is_ok());
    }
}
