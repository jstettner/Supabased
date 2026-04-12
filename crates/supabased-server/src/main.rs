mod db;
mod auth;
mod github;
mod service;

use tonic::transport::Server;

use supabased_proto::supabased::supabased_server::SupabasedServer;
use service::SupabasedService;
use auth::make_interceptor;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = db::init_db("supabased.db").await?;
    let jwt_secret = db::ensure_jwt_secret(&conn).await?;

    let addr = "[::1]:50051".parse()?;
    let svc = SupabasedService::new(conn, jwt_secret.clone());
    let interceptor = make_interceptor(jwt_secret);

    println!("Server listening on {addr}");

    Server::builder()
        .add_service(SupabasedServer::with_interceptor(svc, interceptor))
        .serve(addr)
        .await?;

    Ok(())
}
