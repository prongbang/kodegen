//! Minimal Greeter gRPC server for local testing:
//!   cargo run --example greeter_server
//! then hit the main service's GET /hello/{name}.

use hexagonal_rs::infrastructure::grpc_greeter::proto::greeter_server::{Greeter, GreeterServer};
use hexagonal_rs::infrastructure::grpc_greeter::proto::{HelloReply, HelloRequest};
use tonic::{transport::Server, Request, Response, Status};

#[derive(Default)]
struct MyGreeter;

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(&self, req: Request<HelloRequest>) -> Result<Response<HelloReply>, Status> {
        let name = req.into_inner().name;
        Ok(Response::new(HelloReply {
            message: format!("Hello {name}!"),
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = std::env::var("GREETER_PORT")
        .unwrap_or_else(|_| "50051".into())
        .parse()
        .map(|port: u16| std::net::SocketAddr::from(([0, 0, 0, 0], port)))?;
    println!("greeter server listening on {addr}");
    Server::builder()
        .add_service(GreeterServer::new(MyGreeter))
        .serve(addr)
        .await?;
    Ok(())
}
