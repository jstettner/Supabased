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
    CreateBranchRequest, DeleteBranchRequest, DeleteDemoStateRequest,
    FinishGithubDeviceAuthRequest, GetBranchCredentialsRequest, ListBranchesRequest,
    ListProjectsRequest, LogoutRequest, RefreshSessionRequest, RestoreDemoStateRequest,
    SaveDemoStateRequest, StartGithubDeviceAuthRequest, WhoAmIRequest,
    finish_github_device_auth_response,
};
use supabased_proto::supabased::{DeleteDemoStateResponse, ListDemoStatesRequest};

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
    /// Revoke the current refresh session and remove local credentials
    Logout,
    /// Show identity and permissions
    Whoami,
    /// Project management commands
    #[command(subcommand)]
    Project(ProjectCommands),
    /// Branch management commands
    #[command(subcommand)]
    Branch(BranchCommands),
    /// Demo-state commands
    #[command(subcommand)]
    Demo(DemoCommands),
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

#[derive(Debug, Subcommand)]
enum DemoCommands {
    /// Save main project's current public schema data as a named demo state
    Save {
        /// Project to snapshot
        #[arg(long)]
        project: String,
        /// Demo state name
        name: String,
    },
    /// List saved demo states for a project
    List {
        /// Project to list
        #[arg(long)]
        project: String,
    },
    /// Delete a saved demo state and its backing branch
    Delete {
        /// Project the demo state belongs to
        #[arg(long)]
        project: String,
        /// Demo state name
        name: String,
    },
    /// Restore a saved demo state's public schema data onto main
    Restore {
        /// Project to restore onto
        #[arg(long)]
        project: String,
        /// Demo state name
        name: String,
        /// Confirm that main public schema data may be overwritten
        #[arg(long)]
        confirm_overwrite_main: bool,
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

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn require_cli_restore_confirmation(confirm_overwrite_main: bool) -> Result<(), &'static str> {
    if confirm_overwrite_main {
        Ok(())
    } else {
        Err("restore refused: pass --confirm-overwrite-main to overwrite main public schema data")
    }
}

fn demo_delete_success_message(response: &DeleteDemoStateResponse) -> String {
    if response.remote_branch_missing {
        format!(
            "Deleted demo state '{}' for project '{}'; branch '{}' was already gone.",
            response.name, response.project_name, response.branch_name
        )
    } else {
        format!(
            "Deleted demo state '{}' for project '{}' and branch '{}'.",
            response.name, response.project_name, response.branch_name
        )
    }
}

fn branch_credentials_terminal_output(
    project_name: &str,
    branch_name: &str,
    api_url: &str,
    publishable_key: &str,
) -> String {
    format!(
        "Branch: {project_name}/{branch_name}\n  SUPABASE_URL={api_url}\n  SUPABASE_PUBLISHABLE_KEY={publishable_key}"
    )
}

async fn refresh_cli_session(
    client: &mut SupabasedClient<tonic::transport::Channel>,
    sess: &mut session::Session,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .refresh_session(tonic::Request::new(RefreshSessionRequest {
            refresh_token: sess.refresh_token.clone(),
        }))
        .await
        .map_err(|_| "session expired — run `supabased login` again")?
        .into_inner();

    sess.session_token = response.session_token;
    sess.identity = response.identity;
    sess.expires_at = response.expires_at;
    sess.refresh_token = response.refresh_token;
    sess.refresh_expires_at = response.refresh_expires_at;
    session::save_session(sess)?;
    Ok(())
}

async fn load_fresh_session(
    client: &mut SupabasedClient<tonic::transport::Channel>,
) -> Result<session::Session, Box<dyn std::error::Error>> {
    let mut sess = session::load_session().unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    });

    if sess.expires_at <= now_unix() + 300 {
        if let Err(e) = refresh_cli_session(client, &mut sess).await {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }

    Ok(sess)
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
                refresh_token: reply.refresh_token,
                refresh_expires_at: reply.refresh_expires_at,
            };
            session::save_session(&sess)?;

            println!("Logged in as {}", reply.identity);
            println!("Session stored at {}", session::session_path().display());
        }

        Commands::Logout => {
            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);

            if let Ok(sess) = session::load_session() {
                let mut client = connect(server, ca_cert.as_deref()).await?;
                let _ = client
                    .logout(tonic::Request::new(LogoutRequest {
                        refresh_token: sess.refresh_token,
                    }))
                    .await;
            }

            session::delete_session()?;
            println!("Logged out.");
        }

        Commands::Whoami => {
            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;
            let sess = load_fresh_session(&mut client).await?;

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
            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;
            let sess = load_fresh_session(&mut client).await?;

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
            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;
            let sess = load_fresh_session(&mut client).await?;

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
            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;
            let sess = load_fresh_session(&mut client).await?;

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
            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;
            let sess = load_fresh_session(&mut client).await?;

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
            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;
            let sess = load_fresh_session(&mut client).await?;

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

            println!(
                "{}",
                branch_credentials_terminal_output(
                    &creds.project_name,
                    &creds.branch_name,
                    &creds.api_url,
                    &creds.publishable_key,
                )
            );

            let block = dotenv::format_supabase_block(
                &creds.project_name,
                &creds.branch_name,
                &creds.api_url,
                &creds.publishable_key,
                &creds.secret_key,
            );
            let dotenv_path = std::env::current_dir()?.join(".env");
            dotenv::update_dotenv(&dotenv_path, &block)?;
            println!(
                "\nWrote Supabase credentials, including the secret key, to {}",
                dotenv_path.display()
            );
        }

        Commands::Demo(DemoCommands::Save { project, name }) => {
            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;
            let sess = load_fresh_session(&mut client).await?;

            let mut request = tonic::Request::new(SaveDemoStateRequest {
                project_name: project,
                name,
            });
            request
                .metadata_mut()
                .insert("authorization", auth_metadata(&sess.session_token)?);

            let response = client.save_demo_state(request).await?;
            let version = extract_config_version(response.metadata());
            let reply = response.into_inner();
            refresh_project_cache(&mut client, &sess.session_token, version).await;

            if let Some(state) = reply.state {
                println!(
                    "Saved demo state '{}' for project '{}' as branch '{}'",
                    state.name, state.project_name, state.branch_name
                );
            }
        }

        Commands::Demo(DemoCommands::List { project }) => {
            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;
            let sess = load_fresh_session(&mut client).await?;

            let mut request = tonic::Request::new(ListDemoStatesRequest {
                project_name: project,
            });
            request
                .metadata_mut()
                .insert("authorization", auth_metadata(&sess.session_token)?);

            let response = client.list_demo_states(request).await?;
            let version = extract_config_version(response.metadata());
            let reply = response.into_inner();
            refresh_project_cache(&mut client, &sess.session_token, version).await;

            if reply.states.is_empty() {
                println!("No demo states saved.");
            } else {
                println!("Saved demo states:");
                for state in reply.states {
                    let restored = if state.last_restored_at.is_empty() {
                        "never".to_string()
                    } else {
                        state.last_restored_at
                    };
                    println!(
                        "  {}  branch={}  created={}  last_restored={}",
                        state.name, state.branch_name, state.created_at, restored
                    );
                }
            }
        }

        Commands::Demo(DemoCommands::Delete { project, name }) => {
            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;
            let sess = load_fresh_session(&mut client).await?;

            let mut request = tonic::Request::new(DeleteDemoStateRequest {
                project_name: project,
                name,
            });
            request
                .metadata_mut()
                .insert("authorization", auth_metadata(&sess.session_token)?);

            let response = client.delete_demo_state(request).await?;
            let version = extract_config_version(response.metadata());
            let reply = response.into_inner();
            refresh_project_cache(&mut client, &sess.session_token, version).await;

            println!("{}", demo_delete_success_message(&reply));
        }

        Commands::Demo(DemoCommands::Restore {
            project,
            name,
            confirm_overwrite_main,
        }) => {
            require_cli_restore_confirmation(confirm_overwrite_main)?;

            let server = cli
                .server
                .as_deref()
                .or(cfg.server_url.as_deref())
                .unwrap_or(DEFAULT_SERVER_URL);
            let mut client = connect(server, ca_cert.as_deref()).await?;
            let sess = load_fresh_session(&mut client).await?;

            let mut request = tonic::Request::new(RestoreDemoStateRequest {
                project_name: project,
                name,
                confirm_overwrite_main,
            });
            request
                .metadata_mut()
                .insert("authorization", auth_metadata(&sess.session_token)?);

            let response = client.restore_demo_state(request).await?;
            let version = extract_config_version(response.metadata());
            let reply = response.into_inner();
            refresh_project_cache(&mut client, &sess.session_token, version).await;

            if let Some(state) = reply.state {
                println!(
                    "Restored demo state '{}' onto project '{}'",
                    state.name, state.project_name
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_confirmation_flag_is_required_locally() {
        let err = require_cli_restore_confirmation(false).unwrap_err();
        assert!(err.contains("--confirm-overwrite-main"));
        assert!(require_cli_restore_confirmation(true).is_ok());
    }

    #[test]
    fn branch_credentials_terminal_output_omits_secret_key() {
        let output = branch_credentials_terminal_output(
            "staging",
            "feature",
            "https://branch-ref.supabase.co",
            "sb_publishable_value",
        );

        assert_eq!(
            output,
            "Branch: staging/feature\n  SUPABASE_URL=https://branch-ref.supabase.co\n  SUPABASE_PUBLISHABLE_KEY=sb_publishable_value"
        );
        assert!(!output.contains("SUPABASE_SECRET_KEY"));
        assert!(!output.contains("sb_secret_"));
    }

    #[test]
    fn demo_delete_message_reports_remote_delete() {
        let response = DeleteDemoStateResponse {
            deleted: true,
            project_name: "staging".to_string(),
            name: "happy path".to_string(),
            branch_name: "demo/happy-path".to_string(),
            branch_ref: "branch-ref".to_string(),
            remote_branch_deleted: true,
            remote_branch_missing: false,
        };

        assert_eq!(
            demo_delete_success_message(&response),
            "Deleted demo state 'happy path' for project 'staging' and branch 'demo/happy-path'."
        );
    }

    #[test]
    fn demo_delete_message_reports_missing_remote_branch() {
        let response = DeleteDemoStateResponse {
            deleted: true,
            project_name: "staging".to_string(),
            name: "happy path".to_string(),
            branch_name: "demo/happy-path".to_string(),
            branch_ref: "branch-ref".to_string(),
            remote_branch_deleted: false,
            remote_branch_missing: true,
        };

        assert_eq!(
            demo_delete_success_message(&response),
            "Deleted demo state 'happy path' for project 'staging'; branch 'demo/happy-path' was already gone."
        );
    }

    #[test]
    fn demo_delete_command_parses_project_and_name() {
        let cli = Cli::try_parse_from([
            "supabased",
            "demo",
            "delete",
            "--project",
            "staging",
            "my-demo",
        ])
        .unwrap();

        match cli.command {
            Commands::Demo(DemoCommands::Delete { project, name }) => {
                assert_eq!(project, "staging");
                assert_eq!(name, "my-demo");
            }
            _ => panic!("expected demo delete command"),
        }
    }
}
