mod auth;
mod config;
mod db;
mod github;
mod rate_limit;
mod service;
mod supabase;

use std::net::SocketAddr;

use tonic::transport::{Identity, Server, ServerTlsConfig};

use auth::make_interceptor;
use service::SupabasedService;
use supabased_proto::supabased::supabased_server::SupabasedServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let conn = db::init_db("supabased.db").await?;
    let jwt_secret = db::ensure_jwt_secret(&conn).await?;

    let github_org = std::env::var("GITHUB_ORG").unwrap_or_else(|_| {
        eprintln!("error: GITHUB_ORG environment variable is required but not set");
        std::process::exit(1);
    });
    let github_oauth_client_id = std::env::var("GITHUB_OAUTH_CLIENT_ID").unwrap_or_else(|_| {
        eprintln!("error: GITHUB_OAUTH_CLIENT_ID environment variable is required but not set");
        std::process::exit(1);
    });

    let supabase_token = std::env::var("SUPABASE_ACCESS_TOKEN").unwrap_or_else(|_| {
        eprintln!("error: SUPABASE_ACCESS_TOKEN environment variable is required but not set");
        std::process::exit(1);
    });

    let config_path =
        std::env::var("SUPABASED_CONFIG").unwrap_or_else(|_| "supabased.toml".to_string());
    let (server_config, config_hash) = config::load_config(&config_path).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });

    let supabase_client = supabase::SupabaseClient::new(supabase_token);

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "[::1]:50051".to_string());
    let addr: SocketAddr = bind_addr.parse()?;
    let svc = SupabasedService::new(
        conn,
        jwt_secret.clone(),
        github_org,
        github_oauth_client_id,
        supabase_client,
        server_config,
        config_hash,
    );
    let interceptor = make_interceptor(jwt_secret);

    let mut server = Server::builder();

    let tls_cert = std::env::var("TLS_CERT").ok();
    let tls_key = std::env::var("TLS_KEY").ok();

    match (&tls_cert, &tls_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert = std::fs::read(cert_path).unwrap_or_else(|e| {
                eprintln!("error: failed to read TLS cert at {cert_path}: {e}");
                std::process::exit(1);
            });
            let key = std::fs::read(key_path).unwrap_or_else(|e| {
                eprintln!("error: failed to read TLS key at {key_path}: {e}");
                std::process::exit(1);
            });

            let tls_config = ServerTlsConfig::new().identity(Identity::from_pem(cert, key));
            server = server.tls_config(tls_config)?;

            println!("Server listening on {addr} (TLS)");
        }
        (None, None) => {
            if !addr.ip().is_loopback() {
                eprintln!(
                    "error: TLS_CERT and TLS_KEY are required when binding plaintext outside loopback"
                );
                std::process::exit(1);
            }
            println!("Server listening on {addr} (plaintext)");
        }
        _ => {
            eprintln!("error: both TLS_CERT and TLS_KEY must be set, or neither");
            std::process::exit(1);
        }
    }

    server
        .add_service(SupabasedServer::with_interceptor(svc, interceptor))
        .serve(addr)
        .await?;

    Ok(())
}
