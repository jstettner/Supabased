mod service;

use tonic::transport::Server;

use supabased_proto::supabased::supabased_server::SupabasedServer;
use service::SupabasedService;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse()?;
    let svc = SupabasedService::default();

    println!("Server listening on {addr}");

    Server::builder()
        .add_service(SupabasedServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
