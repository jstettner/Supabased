mod session;

use clap::{Parser, Subcommand};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

use supabased_proto::supabased::supabased_client::SupabasedClient;
use supabased_proto::supabased::{AuthRequest, WhoAmIRequest, auth_request::Method};

#[derive(Debug, Parser)]
#[command(name = "supabased", version, about = "Supabased CLI")]
struct Cli {
    #[arg(long, default_value = "http://[::1]:50051", global = true)]
    server: String,

    /// Path to a PEM CA certificate for verifying the server (for self-signed certs)
    #[arg(long, global = true)]
    ca_cert: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Show identity and permissions
    Whoami,
    /// Authenticate with GitHub
    Login,
}

async fn connect(
    server: &str,
    ca_cert: Option<&str>,
) -> Result<SupabasedClient<Channel>, Box<dyn std::error::Error>> {
    let channel = if server.starts_with("https://") {
        let mut tls_config = ClientTlsConfig::new();

        if let Some(ca_path) = ca_cert {
            let ca_pem = std::fs::read(ca_path)?;
            tls_config = tls_config.ca_certificate(Certificate::from_pem(ca_pem));
        } else {
            tls_config = tls_config.with_native_roots();
        }

        Endpoint::from_shared(server.to_string())?
            .tls_config(tls_config)?
            .connect()
            .await?
    } else {
        Endpoint::from_shared(server.to_string())?
            .connect()
            .await?
    };

    Ok(SupabasedClient::new(channel))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let server = &cli.server;
    let ca_cert = cli.ca_cert.as_deref();

    match cli.command {
        Commands::Login => {
            // Prompt for GitHub PAT
            let token = rpassword::prompt_password("Enter your GitHub personal access token: ")?;

            if token.is_empty() {
                eprintln!("Error: no token provided");
                std::process::exit(1);
            }

            // Call Authenticate RPC
            let mut client = connect(server, ca_cert).await?;
            let response = client
                .authenticate(tonic::Request::new(AuthRequest {
                    method: Some(Method::GithubToken(token)),
                }))
                .await?;
            let reply = response.into_inner();

            // Save session
            let sess = session::Session {
                session_token: reply.session_token,
                identity: reply.identity.clone(),
                expires_at: reply.expires_at,
            };
            session::save_session(&sess)?;

            println!("Logged in as {}", reply.identity);
            println!(
                "Session stored at {}",
                session::session_path().display()
            );
        }

        Commands::Whoami => {
            let sess = session::load_session().unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });

            let mut client = connect(server, ca_cert).await?;

            let mut request = tonic::Request::new(WhoAmIRequest {});
            request.metadata_mut().insert(
                "authorization",
                format!("Bearer {}", sess.session_token).parse()?,
            );

            let response = client.who_am_i(request).await?;
            let reply = response.into_inner();

            println!("Identity: {}", reply.identity);
            println!("Permissions: {:?}", reply.permissions);
            println!("Accessible branches: {:?}", reply.accessible_branches);
        }
    }

    Ok(())
}
