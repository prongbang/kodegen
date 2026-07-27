use crate::domain::{DomainError, Greeter};
use crate::infrastructure::circuit_breaker::{BreakerLayer, BreakerService, CircuitOpen};
use async_trait::async_trait;
use tonic::transport::{Channel, Endpoint};
use tonic::Code;
use tower::Layer;

pub mod proto {
    tonic::include_proto!("greeter");
}

use proto::greeter_client::GreeterClient;
use proto::HelloRequest;

/// gRPC adapter for the `Greeter` port. Every call runs through the
/// circuit-breaker middleware on the channel.
pub struct GrpcGreeter {
    client: GreeterClient<BreakerService<Channel>>,
}

impl GrpcGreeter {
    /// Lazy: no connection is made until the first call.
    pub fn connect_lazy(
        addr: &str,
        breaker: recloser::Recloser,
    ) -> Result<Self, tonic::transport::Error> {
        let channel = Endpoint::from_shared(addr.to_string())?
            .connect_timeout(std::time::Duration::from_secs(2))
            .timeout(std::time::Duration::from_secs(5))
            .connect_lazy();
        let svc = BreakerLayer::new(breaker).layer(channel);
        Ok(Self {
            client: GreeterClient::new(svc),
        })
    }
}

#[async_trait]
impl Greeter for GrpcGreeter {
    async fn say_hello(&self, name: String) -> Result<String, DomainError> {
        let mut client = self.client.clone();
        let reply = client
            .say_hello(HelloRequest { name })
            .await
            .map_err(status_to_domain)?;
        Ok(reply.into_inner().message)
    }
}

fn status_to_domain(s: tonic::Status) -> DomainError {
    if is_circuit_open(&s) {
        return DomainError::Unavailable;
    }
    match s.code() {
        Code::NotFound => DomainError::NotFound,
        Code::InvalidArgument => {
            tracing::warn!(message = %s.message(), "upstream rejected invalid argument");
            DomainError::Validation("invalid input".into())
        }
        Code::Unavailable | Code::DeadlineExceeded => DomainError::Unavailable,
        _ => DomainError::Other(Box::new(s)),
    }
}

fn is_circuit_open(s: &tonic::Status) -> bool {
    let mut source = std::error::Error::source(s);
    while let Some(e) = source {
        if e.is::<CircuitOpen>() {
            return true;
        }
        source = e.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_open_maps_to_unavailable() {
        let status = tonic::Status::from_error(Box::new(CircuitOpen));
        assert!(matches!(status_to_domain(status), DomainError::Unavailable));
    }

    #[test]
    fn not_found_maps_to_not_found() {
        let status = tonic::Status::not_found("x");
        assert!(matches!(status_to_domain(status), DomainError::NotFound));
    }

    #[test]
    fn invalid_argument_maps_to_validation() {
        let status = tonic::Status::invalid_argument("bad");
        assert!(matches!(
            status_to_domain(status),
            DomainError::Validation(_)
        ));
    }
}
