mod session;

use clap::{Parser, Subcommand};
use std::io::{self, Write};

use supabased_proto::supabased::supabased_client::SupabasedClient;
use supabased_proto::supabased::{AuthRequest, WhoAmIRequest, auth_request::Method};

#[derive(Debug, Parser)]
#[command(name = "supabased", version, about = "Supabased CLI")]
struct Cli {
    #[arg(long, default_value = "http://[::1]:50051", global = true)]
    server: String,

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Login => {
            // Prompt for GitHub PAT
            eprint!("Enter your GitHub personal access token: ");
            io::stderr().flush()?;

            let mut token = String::new();
            io::stdin().read_line(&mut token)?;
            let token = token.trim().to_string();

            if token.is_empty() {
                eprintln!("Error: no token provided");
                std::process::exit(1);
            }

            // Call Authenticate RPC
            let mut client = SupabasedClient::connect(cli.server).await?;
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

            let mut client = SupabasedClient::connect(cli.server).await?;

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
