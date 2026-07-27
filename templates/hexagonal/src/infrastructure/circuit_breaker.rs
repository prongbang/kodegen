use recloser::{AsyncRecloser, Recloser};
use std::time::Duration;

/// Defaults: opens at ≥50% failures over the last 100 calls, retries after 30s.
pub fn default_breaker() -> Recloser {
    Recloser::custom()
        .error_rate(0.5)
        .closed_len(100)
        .half_open_len(10)
        .open_wait(Duration::from_secs(30))
        .build()
}

/// Error a [`BreakerService`] returns while its circuit is open.
/// Downcast-detectable through `tonic::Status::source()`.
#[derive(Debug)]
pub struct CircuitOpen;

impl std::fmt::Display for CircuitOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("circuit breaker open")
    }
}

impl std::error::Error for CircuitOpen {}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Tower middleware: wraps any client `Service` (a tonic `Channel`, a
/// hyper/reqwest tower stack) so every call goes through the breaker.
/// Any transport-level error counts as a failure.
#[derive(Clone)]
pub struct BreakerLayer {
    breaker: AsyncRecloser,
}

impl BreakerLayer {
    pub fn new(breaker: Recloser) -> Self {
        Self {
            breaker: AsyncRecloser::from(breaker),
        }
    }
}

impl<S> tower::Layer<S> for BreakerLayer {
    type Service = BreakerService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        BreakerService {
            inner,
            breaker: self.breaker.clone(),
        }
    }
}

#[derive(Clone)]
pub struct BreakerService<S> {
    inner: S,
    breaker: AsyncRecloser,
}

impl<S, Req> tower::Service<Req> for BreakerService<S>
where
    S: tower::Service<Req>,
    S::Error: Into<BoxError>,
    S::Future: Send + 'static,
    S::Response: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<S::Response, BoxError>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let fut = self.breaker.call(self.inner.call(req));
        Box::pin(async move {
            fut.await.map_err(|e| match e {
                recloser::Error::Inner(e) => e.into(),
                recloser::Error::Rejected => Box::new(CircuitOpen) as BoxError,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::{Layer, Service, ServiceExt};

    fn tiny_breaker(open_wait: Duration) -> Recloser {
        // ring buffer of 2: recloser starts computing the rate once the buffer
        // has been fully overwritten, so the breaker opens on the 3rd failure
        Recloser::custom()
            .error_rate(0.5)
            .closed_len(2)
            .half_open_len(1)
            .open_wait(open_wait)
            .build()
    }

    fn failing_svc(breaker: Recloser) -> impl Service<(), Response = (), Error = BoxError> {
        BreakerLayer::new(breaker).layer(tower::service_fn(|_: ()| async {
            Err::<(), BoxError>("transport down".into())
        }))
    }

    #[tokio::test]
    async fn opens_and_rejects_with_circuit_open() {
        let mut svc = failing_svc(tiny_breaker(Duration::from_secs(60)));
        for _ in 0..3 {
            let err = svc.ready().await.unwrap().call(()).await.unwrap_err();
            assert!(!err.is::<CircuitOpen>());
        }
        // circuit open: rejected before reaching the inner service
        let err = svc.ready().await.unwrap().call(()).await.unwrap_err();
        assert!(err.is::<CircuitOpen>());
    }

    #[tokio::test]
    async fn half_open_after_cooldown_lets_trial_through() {
        let mut svc = failing_svc(tiny_breaker(Duration::ZERO));
        for _ in 0..3 {
            let err = svc.ready().await.unwrap().call(()).await.unwrap_err();
            assert!(!err.is::<CircuitOpen>());
        }
        // open_wait elapsed: half-open trial reaches the inner service again
        let err = svc.ready().await.unwrap().call(()).await.unwrap_err();
        assert!(!err.is::<CircuitOpen>());
    }
}
