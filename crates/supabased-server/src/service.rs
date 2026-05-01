use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::{RngCore, rngs::OsRng};

use tokio_rusqlite::Connection;
use tonic::{Request, Response, Status};

use supabased_proto::supabased::{
    AuthResponse, BranchCredentials, CreateBranchRequest, CreateBranchResponse,
    DeleteBranchRequest, DeleteBranchResponse, FinishGithubDeviceAuthRequest,
    FinishGithubDeviceAuthResponse, GetBranchCredentialsRequest, GithubDeviceAuthPending,
    ListBranchesRequest, ListBranchesResponse, ListProjectsRequest, ListProjectsResponse,
    StartGithubDeviceAuthRequest, StartGithubDeviceAuthResponse, WhoAmIRequest, WhoAmIResponse,
    finish_github_device_auth_response, supabased_server::Supabased,
};

use crate::auth::{self, AuthContext, require_permission, require_permission_or_owner};
use crate::config::ServerConfig;
use crate::db;
use crate::github;
use crate::rate_limit::RateLimiter;
use crate::supabase;
use crate::supabase::SupabaseClient;

#[derive(Clone)]
struct GithubDeviceSession {
    device_code: String,
    expires_at: Instant,
    interval: i64,
}

pub struct SupabasedService {
    pub db: Connection,
    pub jwt_secret: Vec<u8>,
    pub github_org: String,
    pub github_oauth_client_id: String,
    github_device_sessions: Arc<Mutex<HashMap<String, GithubDeviceSession>>>,
    pub rate_limiter: RateLimiter,
    pub supabase_client: SupabaseClient,
    pub config: ServerConfig,
    pub config_hash: String,
}

impl SupabasedService {
    pub fn new(
        db: Connection,
        jwt_secret: Vec<u8>,
        github_org: String,
        github_oauth_client_id: String,
        supabase_client: SupabaseClient,
        config: ServerConfig,
        config_hash: String,
    ) -> Self {
        let rate_limiter = RateLimiter::new(5, Duration::from_secs(60));
        rate_limiter.spawn_cleanup_task();
        Self {
            db,
            jwt_secret,
            github_org,
            github_oauth_client_id,
            github_device_sessions: Arc::new(Mutex::new(HashMap::new())),
            rate_limiter,
            supabase_client,
            config,
            config_hash,
        }
    }

    fn with_config_version<T>(&self, mut response: Response<T>) -> Response<T> {
        if let Ok(val) = self.config_hash.parse() {
            response.metadata_mut().insert("x-config-version", val);
        }
        response
    }

    fn prune_expired_device_sessions(&self) {
        let now = Instant::now();
        self.github_device_sessions
            .lock()
            .unwrap()
            .retain(|_, session| session.expires_at > now);
    }

    fn lookup_device_session(&self, auth_session_id: &str) -> Result<GithubDeviceSession, Status> {
        let mut sessions = self.github_device_sessions.lock().unwrap();
        let Some(session) = sessions.get(auth_session_id) else {
            return Err(Status::not_found("unknown GitHub OAuth session"));
        };
        if Instant::now() >= session.expires_at {
            sessions.remove(auth_session_id);
            return Err(Status::deadline_exceeded(
                "GitHub OAuth device authorization expired",
            ));
        }
        Ok(session.clone())
    }

    fn remove_device_session(&self, auth_session_id: &str) {
        self.github_device_sessions
            .lock()
            .unwrap()
            .remove(auth_session_id);
    }

    fn update_device_session_interval(&self, auth_session_id: &str, interval: i64) {
        if let Some(session) = self
            .github_device_sessions
            .lock()
            .unwrap()
            .get_mut(auth_session_id)
        {
            session.interval = interval;
        }
    }
}

fn generate_auth_session_id() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn pending_response(interval: i64) -> FinishGithubDeviceAuthResponse {
    FinishGithubDeviceAuthResponse {
        result: Some(finish_github_device_auth_response::Result::Pending(
            GithubDeviceAuthPending {
                interval: interval.max(1),
                message: "Waiting for GitHub authorization".to_string(),
            },
        )),
    }
}

#[tonic::async_trait]
impl Supabased for SupabasedService {
    async fn who_am_i(
        &self,
        request: Request<WhoAmIRequest>,
    ) -> Result<Response<WhoAmIResponse>, Status> {
        let ctx = request
            .extensions()
            .get::<AuthContext>()
            .ok_or_else(|| Status::unauthenticated("authentication required"))?;
        Ok(self.with_config_version(Response::new(WhoAmIResponse {
            identity: ctx.identity.clone(),
            permissions: ctx.permissions.clone(),
            accessible_branches: vec![],
        })))
    }

    async fn start_github_device_auth(
        &self,
        request: Request<StartGithubDeviceAuthRequest>,
    ) -> Result<Response<StartGithubDeviceAuthResponse>, Status> {
        if let Some(addr) = request.remote_addr() {
            self.rate_limiter.check_rate_limit(addr.ip())?;
        }
        self.prune_expired_device_sessions();
        let start = github::start_device_auth(&self.github_oauth_client_id, "read:org").await?;
        let auth_session_id = generate_auth_session_id();
        let expires_in = start.expires_in.max(1);
        self.github_device_sessions.lock().unwrap().insert(
            auth_session_id.clone(),
            GithubDeviceSession {
                device_code: start.device_code,
                expires_at: Instant::now() + Duration::from_secs(expires_in as u64),
                interval: start.interval,
            },
        );
        Ok(Response::new(StartGithubDeviceAuthResponse {
            auth_session_id,
            user_code: start.user_code,
            verification_uri: start.verification_uri,
            expires_in,
            interval: start.interval,
        }))
    }

    async fn finish_github_device_auth(
        &self,
        request: Request<FinishGithubDeviceAuthRequest>,
    ) -> Result<Response<FinishGithubDeviceAuthResponse>, Status> {
        let auth_session_id = request.into_inner().auth_session_id;
        if auth_session_id.is_empty() {
            return Err(Status::invalid_argument("auth_session_id required"));
        }
        let session = self.lookup_device_session(&auth_session_id)?;
        let poll =
            github::poll_device_auth(&self.github_oauth_client_id, &session.device_code).await;
        if poll.is_err() {
            self.remove_device_session(&auth_session_id);
        }
        match poll? {
            github::DeviceAuthPoll::Pending { interval } => Ok(Response::new(pending_response(
                interval.unwrap_or(session.interval),
            ))),
            github::DeviceAuthPoll::SlowDown { interval } => {
                let updated_interval = interval.unwrap_or(session.interval + 5);
                self.update_device_session_interval(&auth_session_id, updated_interval);
                Ok(Response::new(pending_response(updated_interval)))
            }
            github::DeviceAuthPoll::Complete { access_token, .. } => {
                let user = github::validate_token(&access_token).await?;
                github::check_org_membership(&access_token, &self.github_org, &user.login).await?;
                let identity = format!("github:{}", user.login);
                let permissions = auth::DEFAULT_PERMISSIONS
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>();
                let (token, expires_at) =
                    auth::create_token(&self.jwt_secret, &identity, &permissions)
                        .map_err(|e| Status::internal(format!("token creation failed: {e}")))?;
                self.remove_device_session(&auth_session_id);
                Ok(Response::new(FinishGithubDeviceAuthResponse {
                    result: Some(finish_github_device_auth_response::Result::Auth(
                        AuthResponse {
                            session_token: token,
                            identity,
                            permissions,
                            expires_at,
                        },
                    )),
                }))
            }
        }
    }

    async fn list_projects(
        &self,
        request: Request<ListProjectsRequest>,
    ) -> Result<Response<ListProjectsResponse>, Status> {
        // Require any valid auth — user must be authenticated
        request
            .extensions()
            .get::<AuthContext>()
            .ok_or_else(|| Status::unauthenticated("authentication required"))?;

        let projects = self
            .config
            .projects
            .iter()
            .map(|p| supabased_proto::supabased::ProjectInfo {
                name: p.name.clone(),
            })
            .collect();

        Ok(self.with_config_version(Response::new(ListProjectsResponse { projects })))
    }

    async fn create_branch(
        &self,
        request: Request<CreateBranchRequest>,
    ) -> Result<Response<CreateBranchResponse>, Status> {
        let ctx = request
            .extensions()
            .get::<AuthContext>()
            .ok_or_else(|| Status::unauthenticated("authentication required"))?
            .clone();

        require_permission(&ctx, "branches.create")?;

        let req = request.into_inner();
        let project = self
            .config
            .resolve_project(&req.project_name)
            .ok_or_else(|| Status::not_found(format!("unknown project: {}", req.project_name)))?;

        let branch_resp = self
            .supabase_client
            .create_branch(&project.project_ref, &req.branch_name)
            .await?;

        let branch_ref = branch_resp
            .project_ref
            .as_deref()
            .unwrap_or(&branch_resp.id);

        db::record_branch(
            &self.db,
            &req.branch_name,
            &req.project_name,
            &ctx.identity,
            branch_ref,
        )
        .await
        .map_err(|e| Status::internal(format!("failed to record branch: {e}")))?;

        Ok(
            self.with_config_version(Response::new(CreateBranchResponse {
                branch: Some(supabased_proto::supabased::BranchInfo {
                    branch_name: req.branch_name,
                    project_name: req.project_name,
                    status: branch_resp.status.unwrap_or_default(),
                    created_at: branch_resp.created_at.unwrap_or_default(),
                }),
            })),
        )
    }

    async fn list_branches(
        &self,
        request: Request<ListBranchesRequest>,
    ) -> Result<Response<ListBranchesResponse>, Status> {
        let ctx = request
            .extensions()
            .get::<AuthContext>()
            .ok_or_else(|| Status::unauthenticated("authentication required"))?;

        require_permission(ctx, "branches.list")?;

        let req = request.into_inner();

        let projects: Vec<_> = if req.project_name.is_empty() {
            self.config.projects.iter().collect()
        } else {
            let p = self
                .config
                .resolve_project(&req.project_name)
                .ok_or_else(|| {
                    Status::not_found(format!("unknown project: {}", req.project_name))
                })?;
            vec![p]
        };

        let mut branches = Vec::new();
        for project in &projects {
            let api_branches = self
                .supabase_client
                .list_branches(&project.project_ref)
                .await?;

            for b in api_branches {
                // Skip the default branch (the parent project itself)
                if b.is_default == Some(true) {
                    continue;
                }
                branches.push(supabased_proto::supabased::BranchInfo {
                    branch_name: b.name.unwrap_or_default(),
                    project_name: project.name.clone(),
                    status: b.status.unwrap_or_default(),
                    created_at: b.created_at.unwrap_or_default(),
                });
            }
        }

        Ok(self.with_config_version(Response::new(ListBranchesResponse { branches })))
    }

    async fn delete_branch(
        &self,
        request: Request<DeleteBranchRequest>,
    ) -> Result<Response<DeleteBranchResponse>, Status> {
        let ctx = request
            .extensions()
            .get::<AuthContext>()
            .ok_or_else(|| Status::unauthenticated("authentication required"))?
            .clone();

        let req = request.into_inner();

        // Look up branch in SQLite to get creator and branch_ref
        let record = db::get_branch(&self.db, &req.branch_name, &req.project_name)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?
            .ok_or_else(|| {
                Status::not_found(format!(
                    "branch '{}' not found in project '{}'",
                    req.branch_name, req.project_name
                ))
            })?;

        require_permission_or_owner(
            &ctx,
            "branches.delete_own",
            "branches.delete_any",
            &record.creator_identity,
        )?;

        // Delete from Supabase
        self.supabase_client
            .delete_branch(&record.branch_ref)
            .await?;

        // Remove from SQLite
        db::delete_branch(&self.db, &req.branch_name, &req.project_name)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?;

        Ok(self.with_config_version(Response::new(DeleteBranchResponse { deleted: true })))
    }

    async fn get_branch_credentials(
        &self,
        request: Request<GetBranchCredentialsRequest>,
    ) -> Result<Response<BranchCredentials>, Status> {
        let ctx = request
            .extensions()
            .get::<AuthContext>()
            .ok_or_else(|| Status::unauthenticated("authentication required"))?
            .clone();

        let req = request.into_inner();

        // Look up branch in SQLite to get creator and branch_ref
        let record = db::get_branch(&self.db, &req.branch_name, &req.project_name)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?
            .ok_or_else(|| {
                Status::not_found(format!(
                    "branch '{}' not found in project '{}'",
                    req.branch_name, req.project_name
                ))
            })?;

        require_permission_or_owner(
            &ctx,
            "branches.get_credentials_own",
            "branches.get_credentials_any",
            &record.creator_identity,
        )?;

        // Fetch API keys from Supabase
        let keys = self
            .supabase_client
            .get_api_keys(&record.branch_ref)
            .await?;

        let creds = supabase::extract_credentials(&keys, &record.branch_ref)?;

        Ok(self.with_config_version(Response::new(BranchCredentials {
            branch_name: req.branch_name,
            project_name: req.project_name,
            api_url: creds.api_url,
            anon_key: creds.anon_key,
            service_role_key: creds.service_role_key,
        })))
    }
}

#[cfg(test)]
mod oauth_session_tests {
    use super::*;

    #[test]
    fn auth_session_id_has_hex_shape_and_is_unique() {
        let first = generate_auth_session_id();
        let second = generate_auth_session_id();
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn pending_response_clamps_interval() {
        let response = pending_response(0);
        let Some(finish_github_device_auth_response::Result::Pending(pending)) = response.result
        else {
            panic!("expected pending response");
        };
        assert_eq!(pending.interval, 1);
    }

    #[test]
    fn pruning_removes_expired_sessions_only() {
        let sessions = Arc::new(Mutex::new(HashMap::from([
            (
                "expired".to_string(),
                GithubDeviceSession {
                    device_code: "expired-code".to_string(),
                    expires_at: Instant::now() - Duration::from_secs(1),
                    interval: 5,
                },
            ),
            (
                "active".to_string(),
                GithubDeviceSession {
                    device_code: "active-code".to_string(),
                    expires_at: Instant::now() + Duration::from_secs(60),
                    interval: 5,
                },
            ),
        ])));
        let now = Instant::now();
        sessions
            .lock()
            .unwrap()
            .retain(|_, session| session.expires_at > now);
        let sessions = sessions.lock().unwrap();
        assert!(!sessions.contains_key("expired"));
        assert!(sessions.contains_key("active"));
    }
}
