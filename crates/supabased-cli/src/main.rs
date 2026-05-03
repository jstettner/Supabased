mod config;
mod dotenv;
mod session;
mod tree;

use std::io::{self, Write};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

use supabased_proto::supabased::supabased_client::SupabasedClient;
use supabased_proto::supabased::{
    CreateBranchRequest, DeleteBranchRequest, FinishGithubDeviceAuthRequest,
    GetBranchCredentialsRequest, ListBranchesRequest, ListProjectsRequest,
    StartGithubDeviceAuthRequest, WhoAmIRequest, finish_github_device_auth_response,
};

const DEFAULT_SERVER_URL: &str = "http://[::1]:50051";

#[derive(Debug, Parser)]
#[command(name = "supabased", version, about = "Supabased CLI")]
struct Cli {
    #[arg(long, global = true)]
    server: Option<String>,

    /// Path to a PEM CA certificate for verifying the server (for self-signed certs)
    #[arg(long, global = true)]
    ca_cert: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Authenticate with GitHub
    Login,
    /// Show identity and permissions
    Whoami,
    /// Project management commands
    #[command(subcommand)]
    Project(ProjectCommands),
    /// Branch management commands
    #[command(subcommand)]
    Branch(BranchCommands),
}

#[derive(Debug, Subcommand)]
enum ProjectCommands {
    /// List configured projects
    List,
}

#[derive(Debug, Subcommand)]
enum BranchCommands {
    /// Create a branch from a configured project
    Create {
        /// Project to branch from
        #[arg(long)]
        project: String,
        /// Name for the new branch
        name: String,
    },
    /// List branches (all projects if --project is omitted)
    List {
        /// Filter by project name
        #[arg(long)]
        project: Option<String>,
    },
    /// Delete a branch
    Delete {
        /// Project the branch belongs to
        #[arg(long)]
        project: String,
        /// Name of the branch to delete
        name: String,
    },
    /// Get credentials for a branch
    Credentials {
        /// Project the branch belongs to
        #[arg(long)]
        project: String,
        /// Name of the branch
        name: String,
    },
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
        Endpoint::from_shared(server.to_string())?.connect().await?
    };

    Ok(SupabasedClient::new(channel))
}

fn auth_metadata(
    session_token: &str,
) -> Result<tonic::metadata::MetadataValue<tonic::metadata::Ascii>, Box<dyn std::error::Error>> {
    Ok(format!("Bearer {session_token}").parse()?)
}

fn extract_config_version(metadata: &tonic::metadata::MetadataMap) -> Option<String> {
    metadata
        .get("x-config-version")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

async fn refresh_project_cache(
    client: &mut SupabasedClient<tonic::transport::Channel>,
    session_token: &str,
    server_version: Option<String>,
) {
    let mut cfg = config::load_config();

    // Only refresh if version actually changed
    if server_version.as_deref() == cfg.config_version.as_deref() {
        return;
    }

    let mut request = tonic::Request::new(ListProjectsRequest {});
    if let Ok(val) = auth_metadata(session_token) {
        request.metadata_mut().insert("authorization", val);
    } else {
        return;
    }

    if let Ok(response) = client.list_projects(request).await {
        let projects = response.into_inner().projects;
        cfg.cached_projects = Some(
            projects
                .iter()
                .map(|p| config::CachedProject {
                    name: p.name.clone(),
                })
                .collect(),
        );
        cfg.config_version = server_version;
        let _ = config::save_config(&cfg);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let cfg = config::load_config();
    let ca_cert = cli.ca_cert.or(cfg.ca_cert);

    match cli.command {
        Commands::Login => {
            // Prompt for server URL
            let current_server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            eprint!("Server URL [{current_server}]: ");
            io::stderr().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();

            let server = if input.is_empty() {
                current_server
            } else {
                input
            };

            let mut client = connect(server, ca_cert.as_deref()).await?;
            let start = client
                .start_github_device_auth(tonic::Request::new(StartGithubDeviceAuthRequest {}))
                .await?
                .into_inner();

            println!("First copy your one-time code: {}", start.user_code);
            println!("Opening {} in your browser...", start.verification_uri);
            if let Err(e) = open::that(&start.verification_uri) {
                eprintln!("Could not open browser automatically: {e}");
                eprintln!("Open this URL manually: {}", start.verification_uri);
            }

            let mut interval = start.interval.max(1) as u64;
            let deadline = Instant::now() + Duration::from_secs(start.expires_in.max(1) as u64);
            let reply = loop {
                if Instant::now() >= deadline {
                    return Err("GitHub authorization expired; run `supabased login` again".into());
                }

                tokio::time::sleep(Duration::from_secs(interval)).await;
                let response = client
                    .finish_github_device_auth(tonic::Request::new(FinishGithubDeviceAuthRequest {
                        auth_session_id: start.auth_session_id.clone(),
                    }))
                    .await?
                    .into_inner();

                match response.result {
                    Some(finish_github_device_auth_response::Result::Auth(auth)) => break auth,
                    Some(finish_github_device_auth_response::Result::Pending(pending)) => {
                        interval = pending.interval.max(1) as u64;
                        eprintln!("Waiting for GitHub authorization...");
                    }
                    None => return Err("server returned an empty OAuth polling response".into()),
                }
            };

            let mut updated_cfg = config::load_config();
            updated_cfg.server_url = Some(server.to_string());
            updated_cfg.ca_cert = ca_cert;
            config::save_config(&updated_cfg)?;

            let sess = session::Session {
                session_token: reply.session_token,
                identity: reply.identity.clone(),
                expires_at: reply.expires_at,
            };
            session::save_session(&sess)?;

            println!("Logged in as {}", reply.identity);
            println!("Session stored at {}", session::session_path().display());
        }

        Commands::Whoami => {
            let sess = session::load_session().unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });

            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;

            let mut request = tonic::Request::new(WhoAmIRequest {});
            request.metadata_mut().insert(
                "authorization",
                format!("Bearer {}", sess.session_token).parse()?,
            );

            let response = client.who_am_i(request).await?;
            let version = extract_config_version(response.metadata());
            let reply = response.into_inner();
            refresh_project_cache(&mut client, &sess.session_token, version).await;

            println!("Identity: {}", reply.identity);
            println!("Permissions: {:?}", reply.permissions);
            println!("Accessible branches: {:?}", reply.accessible_branches);
        }

        Commands::Project(ProjectCommands::List) => {
            let sess = session::load_session().unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });

            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;

            let mut request = tonic::Request::new(ListProjectsRequest {});
            request
                .metadata_mut()
                .insert("authorization", auth_metadata(&sess.session_token)?);

            let response = client.list_projects(request).await?;
            let version = extract_config_version(response.metadata());
            let reply = response.into_inner();

            // Update cache
            let mut cfg_updated = config::load_config();
            cfg_updated.cached_projects = Some(
                reply
                    .projects
                    .iter()
                    .map(|p| config::CachedProject {
                        name: p.name.clone(),
                    })
                    .collect(),
            );
            cfg_updated.config_version = version;
            let _ = config::save_config(&cfg_updated);

            if reply.projects.is_empty() {
                println!("No projects configured.");
            } else {
                println!("Configured projects:");
                for p in &reply.projects {
                    println!("  {}", p.name);
                }
            }
        }

        Commands::Branch(BranchCommands::Create { project, name }) => {
            let sess = session::load_session().unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });

            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;

            let mut request = tonic::Request::new(CreateBranchRequest {
                project_name: project,
                branch_name: name,
            });
            request
                .metadata_mut()
                .insert("authorization", auth_metadata(&sess.session_token)?);

            let response = client.create_branch(request).await?;
            let version = extract_config_version(response.metadata());
            let reply = response.into_inner();
            refresh_project_cache(&mut client, &sess.session_token, version).await;

            if let Some(branch) = reply.branch {
                println!(
                    "Created branch '{}' in project '{}' (status: {})",
                    branch.branch_name, branch.project_name, branch.status
                );
            }
        }

        Commands::Branch(BranchCommands::List { project }) => {
            let sess = session::load_session().unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });

            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;

            let mut request = tonic::Request::new(ListBranchesRequest {
                project_name: project.unwrap_or_default(),
            });
            request
                .metadata_mut()
                .insert("authorization", auth_metadata(&sess.session_token)?);

            let response = client.list_branches(request).await?;
            let version = extract_config_version(response.metadata());
            let reply = response.into_inner();
            refresh_project_cache(&mut client, &sess.session_token, version).await;

            println!("{}", tree::render_branch_tree(&reply.branches));
        }

        Commands::Branch(BranchCommands::Delete { project, name }) => {
            let sess = session::load_session().unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });

            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;

            let mut request = tonic::Request::new(DeleteBranchRequest {
                project_name: project.clone(),
                branch_name: name.clone(),
            });
            request
                .metadata_mut()
                .insert("authorization", auth_metadata(&sess.session_token)?);

            let response = client.delete_branch(request).await?;
            let version = extract_config_version(response.metadata());
            refresh_project_cache(&mut client, &sess.session_token, version).await;
            println!("Deleted branch '{}' from project '{}'", name, project);
        }

        Commands::Branch(BranchCommands::Credentials { project, name }) => {
            let sess = session::load_session().unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });

            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;

            let mut request = tonic::Request::new(GetBranchCredentialsRequest {
                project_name: project,
                branch_name: name,
            });
            request
                .metadata_mut()
                .insert("authorization", auth_metadata(&sess.session_token)?);

            let response = client.get_branch_credentials(request).await?;
            let version = extract_config_version(response.metadata());
            let creds = response.into_inner();
            refresh_project_cache(&mut client, &sess.session_token, version).await;

            println!("Branch: {}/{}", creds.project_name, creds.branch_name);
            println!("  SUPABASE_URL={}", creds.api_url);
            println!("  SUPABASE_KEY={}", creds.anon_key);

            let block = dotenv::format_supabase_block(
                &creds.project_name,
                &creds.branch_name,
                &creds.api_url,
                &creds.anon_key,
                &creds.service_role_key,
            );
            let dotenv_path = std::env::current_dir()?.join(".env");
            dotenv::update_dotenv(&dotenv_path, &block)?;
            println!(
                "\nWrote Supabase credentials, including the service-role key, to {}",
                dotenv_path.display()
            );
        }
    }

    Ok(())
}
