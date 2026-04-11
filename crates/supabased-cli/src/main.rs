use clap::{Parser, Subcommand};

use supabased_proto::supabased::supabased_client::SupabasedClient;
use supabased_proto::supabased::WhoAmIRequest;

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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Whoami => {
            let mut client = SupabasedClient::connect(cli.server).await?;
            let response = client
                .who_am_i(tonic::Request::new(WhoAmIRequest {}))
                .await?;
            let reply = response.into_inner();
            println!("Identity: {}", reply.identity);
            println!("Permissions: {:?}", reply.permissions);
            println!("Accessible branches: {:?}", reply.accessible_branches);
        }
    }

    Ok(())
}
