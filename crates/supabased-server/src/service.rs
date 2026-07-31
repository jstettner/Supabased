use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};

use tokio_rusqlite::Connection;
use tonic::{Request, Response, Status};

use supabased_proto::supabased::{
    AuthResponse, BranchCredentials, BranchOwnership, CreateBranchRequest, CreateBranchResponse,
    DeleteBranchRequest, DeleteBranchResponse, DeleteDemoStateRequest, DeleteDemoStateResponse,
    DemoStateInfo, FinishGithubDeviceAuthRequest, FinishGithubDeviceAuthResponse,
    GetBranchCredentialsRequest, GithubDeviceAuthPending, ListBranchesRequest,
    ListBranchesResponse, ListDemoStatesRequest, ListDemoStatesResponse, ListProjectsRequest,
    ListProjectsResponse, LogoutRequest, LogoutResponse, RefreshSessionRequest,
    RestoreDemoStateRequest, RestoreDemoStateResponse, SaveDemoStateRequest, SaveDemoStateResponse,
    StartGithubDeviceAuthRequest, StartGithubDeviceAuthResponse, WhoAmIRequest, WhoAmIResponse,
    finish_github_device_auth_response, supabased_server::Supabased,
};

use crate::auth::{self, AuthContext, require_permission, require_permission_or_owner};
use crate::config::{ProjectConfig, ServerConfig};
use crate::db;
use crate::github;
use crate::rate_limit::RateLimiter;
use crate::restore;
use crate::supabase;
use crate::supabase::DeleteBranchOutcome;
use crate::supabase::SupabaseClient;

const REFRESH_TOKEN_TTL_SECONDS: i64 = 30 * 24 * 3600;

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
    random_hex(32)
}

fn random_hex(len: usize) -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes[..len.min(bytes.len())]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[allow(deprecated)]
fn branch_credentials_response(
    project_name: String,
    branch_name: String,
    creds: supabase::BranchCredentialSet,
) -> BranchCredentials {
    let publishable_key = creds.publishable_key;
    let secret_key = creds.secret_key;

    BranchCredentials {
        branch_name,
        project_name,
        api_url: creds.api_url,
        anon_key: publishable_key.clone(),
        service_role_key: secret_key.clone(),
        publishable_key,
        secret_key,
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn classify_branch_ownership(
    ctx: &AuthContext,
    record: Option<&db::BranchRecord>,
) -> BranchOwnership {
    match record {
        Some(record) if record.creator_identity == ctx.identity => BranchOwnership::Yours,
        Some(_) => BranchOwnership::Other,
        None => BranchOwnership::Untracked,
    }
}

fn demo_state_info(record: db::DemoStateRecord) -> DemoStateInfo {
    DemoStateInfo {
        project_name: record.project_name,
        name: record.name,
        branch_name: record.branch_name,
        branch_ref: record.branch_ref,
        creator_identity: record.creator_identity,
        created_at: record.created_at,
        last_restored_at: record.last_restored_at.unwrap_or_default(),
    }
}

fn sanitized_demo_branch_name(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    let mut last_was_dash = false;

    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_') {
            sanitized.push(c);
            last_was_dash = false;
        } else if !last_was_dash {
            sanitized.push('-');
            last_was_dash = true;
        }
    }

    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "demo/state".to_string()
    } else {
        format!("demo/{}", sanitized.chars().take(80).collect::<String>())
    }
}

fn require_restore_confirmation(confirm_overwrite_main: bool) -> Result<(), Status> {
    if confirm_overwrite_main {
        Ok(())
    } else {
        Err(Status::failed_precondition(
            "restore requires confirm_overwrite_main=true",
        ))
    }
}

fn resolve_demo_project<'a>(
    config: &'a ServerConfig,
    project_name: &str,
) -> Result<&'a ProjectConfig, Status> {
    let project = resolve_project(config, project_name)?;
    if project.is_demo_project() {
        Ok(project)
    } else {
        Err(Status::failed_precondition(format!(
            "project '{project_name}' is not configured for demo operations"
        )))
    }
}

fn resolve_project<'a>(
    config: &'a ServerConfig,
    project_name: &str,
) -> Result<&'a ProjectConfig, Status> {
    config
        .resolve_project(project_name)
        .ok_or_else(|| Status::not_found(format!("unknown project: {project_name}")))
}

fn validated_branch_ref(
    branch_resp: &supabase::BranchResponse,
    parent_project_ref: &str,
) -> Result<String, Status> {
    let branch_ref = branch_resp
        .project_ref
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Status::internal("Supabase branch response did not include project_ref"))?;

    if branch_ref == parent_project_ref {
        return Err(Status::failed_precondition(
            "Supabase returned the parent project as the created branch",
        ));
    }

    if branch_resp.parent_project_ref.as_deref() != Some(parent_project_ref) {
        return Err(Status::failed_precondition(
            "Supabase branch response parent did not match the configured project",
        ));
    }

    if branch_resp.is_default != Some(false) {
        return Err(Status::failed_precondition(
            "Supabase branch response was not a non-default child branch",
        ));
    }

    Ok(branch_ref.to_string())
}

fn hash_refresh_secret(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

fn generate_refresh_token() -> (String, String, Vec<u8>) {
    let selector = random_hex(16);
    let secret = random_hex(32);
    let token_hash = hash_refresh_secret(&secret);
    (format!("{selector}.{secret}"), selector, token_hash)
}

fn parse_refresh_token(token: &str) -> Result<(&str, &str), Status> {
    token
        .split_once('.')
        .filter(|(selector, secret)| !selector.is_empty() && !secret.is_empty())
        .ok_or_else(|| Status::unauthenticated("invalid refresh token"))
}

async fn issue_auth_response(
    db: &Connection,
    jwt_secret: &[u8],
    identity: String,
    permissions: Vec<String>,
) -> Result<AuthResponse, Status> {
    let (session_token, expires_at) = auth::create_token(jwt_secret, &identity, &permissions)
        .map_err(|e| Status::internal(format!("token creation failed: {e}")))?;
    let (refresh_token, selector, token_hash) = generate_refresh_token();
    let now = now_unix();
    let refresh_expires_at = now + REFRESH_TOKEN_TTL_SECONDS;

    db::insert_refresh_session(
        db,
        &selector,
        &token_hash,
        &identity,
        &permissions,
        now,
        refresh_expires_at,
    )
    .await
    .map_err(|e| Status::internal(format!("failed to create refresh session: {e}")))?;

    Ok(AuthResponse {
        session_token,
        identity,
        permissions,
        expires_at,
        refresh_token,
        refresh_expires_at,
    })
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
        if let Some(addr) = request.remote_addr() {
            self.rate_limiter.check_rate_limit(addr.ip())?;
        }

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
                let auth =
                    issue_auth_response(&self.db, &self.jwt_secret, identity, permissions).await?;
                self.remove_device_session(&auth_session_id);
                Ok(Response::new(FinishGithubDeviceAuthResponse {
                    result: Some(finish_github_device_auth_response::Result::Auth(auth)),
                }))
            }
        }
    }

    async fn refresh_session(
        &self,
        request: Request<RefreshSessionRequest>,
    ) -> Result<Response<AuthResponse>, Status> {
        let refresh_token = request.into_inner().refresh_token;
        let (selector, secret) = parse_refresh_token(&refresh_token)?;
        let record = db::get_refresh_session(&self.db, selector)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?
            .ok_or_else(|| Status::unauthenticated("invalid refresh token"))?;

        let now = now_unix();
        if record.revoked_at.is_some() || record.expires_at <= now {
            return Err(Status::unauthenticated(
                "refresh session expired — run `supabased login` again",
            ));
        }

        if record.token_hash != hash_refresh_secret(secret) {
            return Err(Status::unauthenticated("invalid refresh token"));
        }

        let permissions: Vec<String> = serde_json::from_str(&record.permissions_json)
            .map_err(|e| Status::internal(format!("stored permissions are invalid: {e}")))?;
        let (session_token, expires_at) =
            auth::create_token(&self.jwt_secret, &record.identity, &permissions)
                .map_err(|e| Status::internal(format!("token creation failed: {e}")))?;
        let (new_refresh_token, new_selector, new_token_hash) = generate_refresh_token();
        let refresh_expires_at = now + REFRESH_TOKEN_TTL_SECONDS;

        db::rotate_refresh_session(
            &self.db,
            &record.selector,
            &new_selector,
            &new_token_hash,
            &record.identity,
            &permissions,
            now,
            refresh_expires_at,
        )
        .await
        .map_err(|e| Status::internal(format!("failed to rotate refresh session: {e}")))?;

        Ok(Response::new(AuthResponse {
            session_token,
            identity: record.identity,
            permissions,
            expires_at,
            refresh_token: new_refresh_token,
            refresh_expires_at,
        }))
    }

    async fn logout(
        &self,
        request: Request<LogoutRequest>,
    ) -> Result<Response<LogoutResponse>, Status> {
        let refresh_token = request.into_inner().refresh_token;
        let (selector, _) = parse_refresh_token(&refresh_token)?;
        let revoked = db::revoke_refresh_session(&self.db, selector, now_unix())
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?;
        Ok(Response::new(LogoutResponse { revoked }))
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
        let project = resolve_project(&self.config, &req.project_name)?;

        let branch_resp = self
            .supabase_client
            .create_branch(&project.project_ref, &req.branch_name)
            .await?;

        let branch_ref = validated_branch_ref(&branch_resp, &project.project_ref)?;

        db::record_branch(
            &self.db,
            &req.branch_name,
            &req.project_name,
            &ctx.identity,
            &branch_ref,
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
                    ownership: BranchOwnership::Yours as i32,
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
            .ok_or_else(|| Status::unauthenticated("authentication required"))?
            .clone();

        require_permission(&ctx, "branches.list")?;

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

        let tracked_records = if req.project_name.is_empty() {
            db::list_all_branches(&self.db)
                .await
                .map_err(|e| Status::internal(format!("database error: {e}")))?
        } else {
            db::list_branches_by_project(&self.db, &req.project_name)
                .await
                .map_err(|e| Status::internal(format!("database error: {e}")))?
        };
        let tracked_by_branch: HashMap<(String, String), db::BranchRecord> = tracked_records
            .into_iter()
            .map(|record| {
                (
                    (record.project_name.clone(), record.branch_name.clone()),
                    record,
                )
            })
            .collect();

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
                let branch_name = b.name.unwrap_or_default();
                let ownership = classify_branch_ownership(
                    &ctx,
                    tracked_by_branch.get(&(project.name.clone(), branch_name.clone())),
                );
                branches.push(supabased_proto::supabased::BranchInfo {
                    branch_name,
                    project_name: project.name.clone(),
                    status: b.status.unwrap_or_default(),
                    created_at: b.created_at.unwrap_or_default(),
                    ownership: ownership as i32,
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
        let outcome = self
            .supabase_client
            .delete_branch(&record.branch_ref)
            .await?;
        if outcome == DeleteBranchOutcome::Missing {
            return Err(Status::not_found(format!(
                "Supabase branch '{}' was not found",
                record.branch_ref
            )));
        }

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
        let response = branch_credentials_response(req.project_name, req.branch_name, creds);

        Ok(self.with_config_version(Response::new(response)))
    }

    async fn save_demo_state(
        &self,
        request: Request<SaveDemoStateRequest>,
    ) -> Result<Response<SaveDemoStateResponse>, Status> {
        let ctx = request
            .extensions()
            .get::<AuthContext>()
            .ok_or_else(|| Status::unauthenticated("authentication required"))?
            .clone();

        require_permission(&ctx, "demo.save")?;

        let req = request.into_inner();
        if req.name.trim().is_empty() {
            return Err(Status::invalid_argument("demo state name required"));
        }
        let project = self
            .config
            .resolve_project(&req.project_name)
            .ok_or_else(|| Status::not_found(format!("unknown project: {}", req.project_name)))?;

        if db::get_demo_state(&self.db, &req.project_name, &req.name)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?
            .is_some()
        {
            return Err(Status::already_exists(format!(
                "demo state '{}' already exists in project '{}'",
                req.name, req.project_name
            )));
        }

        let branch_name = sanitized_demo_branch_name(&req.name);
        let branch_resp = self
            .supabase_client
            .create_branch(&project.project_ref, &branch_name)
            .await?;
        let branch_ref = validated_branch_ref(&branch_resp, &project.project_ref)?;

        db::record_demo_state(
            &self.db,
            &req.project_name,
            &req.name,
            &branch_name,
            &branch_ref,
            &ctx.identity,
        )
        .await
        .map_err(|e| Status::internal(format!("failed to record demo state: {e}")))?;

        let record = db::get_demo_state(&self.db, &req.project_name, &req.name)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?
            .ok_or_else(|| Status::internal("saved demo state record was not found"))?;

        Ok(
            self.with_config_version(Response::new(SaveDemoStateResponse {
                state: Some(demo_state_info(record)),
            })),
        )
    }

    async fn list_demo_states(
        &self,
        request: Request<ListDemoStatesRequest>,
    ) -> Result<Response<ListDemoStatesResponse>, Status> {
        let ctx = request
            .extensions()
            .get::<AuthContext>()
            .ok_or_else(|| Status::unauthenticated("authentication required"))?
            .clone();

        require_permission(&ctx, "demo.list")?;

        let req = request.into_inner();
        resolve_demo_project(&self.config, &req.project_name)?;

        let states = db::list_demo_states_by_project(&self.db, &req.project_name)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?
            .into_iter()
            .map(demo_state_info)
            .collect();

        Ok(self.with_config_version(Response::new(ListDemoStatesResponse { states })))
    }

    async fn delete_demo_state(
        &self,
        request: Request<DeleteDemoStateRequest>,
    ) -> Result<Response<DeleteDemoStateResponse>, Status> {
        let ctx = request
            .extensions()
            .get::<AuthContext>()
            .ok_or_else(|| Status::unauthenticated("authentication required"))?
            .clone();

        let req = request.into_inner();
        resolve_demo_project(&self.config, &req.project_name)?;

        let record = db::get_demo_state(&self.db, &req.project_name, &req.name)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?
            .ok_or_else(|| {
                Status::not_found(format!(
                    "demo state '{}' not found in project '{}'",
                    req.name, req.project_name
                ))
            })?;

        require_permission_or_owner(
            &ctx,
            "demo.delete_own",
            "demo.delete_any",
            &record.creator_identity,
        )?;

        let outcome = self
            .supabase_client
            .delete_branch(&record.branch_ref)
            .await?;
        db::delete_demo_state(&self.db, &req.project_name, &req.name)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?;

        Ok(
            self.with_config_version(Response::new(DeleteDemoStateResponse {
                deleted: true,
                project_name: record.project_name,
                name: record.name,
                branch_name: record.branch_name,
                branch_ref: record.branch_ref,
                remote_branch_deleted: outcome == DeleteBranchOutcome::Deleted,
                remote_branch_missing: outcome == DeleteBranchOutcome::Missing,
            })),
        )
    }

    async fn restore_demo_state(
        &self,
        request: Request<RestoreDemoStateRequest>,
    ) -> Result<Response<RestoreDemoStateResponse>, Status> {
        let ctx = request
            .extensions()
            .get::<AuthContext>()
            .ok_or_else(|| Status::unauthenticated("authentication required"))?
            .clone();

        let req = request.into_inner();
        require_restore_confirmation(req.confirm_overwrite_main)?;

        require_permission(&ctx, "demo.restore_main")?;

        let project = resolve_demo_project(&self.config, &req.project_name)?;
        let record = db::get_demo_state(&self.db, &req.project_name, &req.name)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?
            .ok_or_else(|| {
                Status::not_found(format!(
                    "demo state '{}' not found in project '{}'",
                    req.name, req.project_name
                ))
            })?;

        let source_connection = project
            .database_connection_for_ref(&record.branch_ref)
            .map_err(Status::failed_precondition)?;
        let target_connection = project
            .database_connection_for_ref(&project.project_ref)
            .map_err(Status::failed_precondition)?;

        tokio::task::spawn_blocking(move || {
            restore::restore_public_schema(&source_connection, &target_connection)
        })
        .await
        .map_err(|e| Status::internal(format!("restore task failed: {e}")))??;

        let updated = db::mark_demo_state_restored(&self.db, &req.project_name, &req.name)
            .await
            .map_err(|e| Status::internal(format!("database error: {e}")))?
            .ok_or_else(|| Status::internal("restored demo state record was not found"))?;

        Ok(
            self.with_config_version(Response::new(RestoreDemoStateResponse {
                state: Some(demo_state_info(updated)),
            })),
        )
    }
}

#[cfg(test)]
mod oauth_session_tests {
    use super::*;

    fn ctx(identity: &str) -> AuthContext {
        AuthContext {
            identity: identity.to_string(),
            permissions: vec![],
        }
    }

    fn branch_record(creator_identity: &str) -> db::BranchRecord {
        db::BranchRecord {
            branch_name: "feature".to_string(),
            project_name: "staging".to_string(),
            creator_identity: creator_identity.to_string(),
            branch_ref: "branch-ref".to_string(),
            created_at: "2026-05-03T00:00:00Z".to_string(),
        }
    }

    fn project_config(name: &str, demo: bool) -> ProjectConfig {
        ProjectConfig {
            name: name.to_string(),
            project_ref: format!("{name}-ref"),
            demo,
            database_password_env: demo.then(|| format!("{}_DB_PASSWORD", name.to_uppercase())),
        }
    }

    fn created_branch_response(
        project_ref: &str,
        parent_project_ref: &str,
        is_default: bool,
    ) -> supabase::BranchResponse {
        supabase::BranchResponse {
            id: "branch-id".to_string(),
            name: Some("demo/farmer".to_string()),
            project_ref: Some(project_ref.to_string()),
            parent_project_ref: Some(parent_project_ref.to_string()),
            is_default: Some(is_default),
            git_branch: Some("demo/farmer".to_string()),
            status: Some("CREATING_PROJECT".to_string()),
            created_at: Some("2026-06-26T16:52:22Z".to_string()),
            updated_at: Some("2026-06-26T16:52:22Z".to_string()),
        }
    }

    #[test]
    fn classify_branch_ownership_marks_current_creator_as_yours() {
        let ctx = ctx("github:alice");
        let record = branch_record("github:alice");

        assert_eq!(
            classify_branch_ownership(&ctx, Some(&record)),
            BranchOwnership::Yours
        );
    }

    #[test]
    fn classify_branch_ownership_marks_other_creator_as_other() {
        let ctx = ctx("github:alice");
        let record = branch_record("github:bob");

        assert_eq!(
            classify_branch_ownership(&ctx, Some(&record)),
            BranchOwnership::Other
        );
    }

    #[test]
    fn classify_branch_ownership_marks_missing_record_as_untracked() {
        let ctx = ctx("github:alice");

        assert_eq!(
            classify_branch_ownership(&ctx, None),
            BranchOwnership::Untracked
        );
    }

    #[test]
    fn auth_session_id_has_hex_shape_and_is_unique() {
        let first = generate_auth_session_id();
        let second = generate_auth_session_id();
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    #[allow(deprecated)]
    fn branch_credentials_populate_modern_and_compatibility_fields() {
        let response = branch_credentials_response(
            "staging".into(),
            "feature".into(),
            supabase::BranchCredentialSet {
                api_url: "https://branch-ref.supabase.co".into(),
                publishable_key: "sb_publishable_value".into(),
                secret_key: "sb_secret_value".into(),
            },
        );

        assert_eq!(response.publishable_key, "sb_publishable_value");
        assert_eq!(response.anon_key, response.publishable_key);
        assert_eq!(response.secret_key, "sb_secret_value");
        assert_eq!(response.service_role_key, response.secret_key);
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
    fn refresh_token_has_selector_and_secret() {
        let (token, selector, token_hash) = generate_refresh_token();
        let (parsed_selector, parsed_secret) = parse_refresh_token(&token).unwrap();
        assert_eq!(parsed_selector, selector);
        assert_eq!(token_hash, hash_refresh_secret(parsed_secret));
        assert_ne!(selector, parsed_secret);
    }

    #[test]
    fn demo_branch_name_is_sanitized() {
        assert_eq!(
            sanitized_demo_branch_name("Sales Demo 2026!"),
            "demo/sales-demo-2026"
        );
        assert_eq!(sanitized_demo_branch_name("___"), "demo/___");
        assert_eq!(sanitized_demo_branch_name("!!!"), "demo/state");
    }

    #[test]
    fn restore_confirmation_is_required() {
        let err = require_restore_confirmation(false).unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert!(require_restore_confirmation(true).is_ok());
    }

    #[test]
    fn resolve_project_allows_non_demo_projects() {
        let config = ServerConfig {
            projects: vec![project_config("staging", false)],
        };

        let project = resolve_project(&config, "staging").unwrap();
        assert_eq!(project.project_ref, "staging-ref");
    }

    #[test]
    fn resolve_project_rejects_unknown_projects() {
        let config = ServerConfig {
            projects: vec![project_config("staging", false)],
        };

        let err = resolve_project(&config, "unknown").unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[test]
    fn resolve_demo_project_rejects_non_demo_projects() {
        let config = ServerConfig {
            projects: vec![project_config("staging", false)],
        };

        let err = resolve_demo_project(&config, "staging").unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[test]
    fn resolve_demo_project_allows_demo_projects() {
        let config = ServerConfig {
            projects: vec![project_config("staging", true)],
        };

        let project = resolve_demo_project(&config, "staging").unwrap();
        assert_eq!(project.project_ref, "staging-ref");
    }

    #[test]
    fn resolve_demo_project_rejects_unknown_projects() {
        let config = ServerConfig {
            projects: vec![project_config("staging", true)],
        };

        let err = resolve_demo_project(&config, "unknown").unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[test]
    fn validates_created_branch_response_as_child_branch() {
        let response = created_branch_response("child-ref", "parent-ref", false);
        assert_eq!(
            validated_branch_ref(&response, "parent-ref").unwrap(),
            "child-ref"
        );
    }

    #[test]
    fn rejects_created_branch_response_that_points_at_parent_project() {
        let response = created_branch_response("parent-ref", "parent-ref", false);
        let err = validated_branch_ref(&response, "parent-ref").unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[test]
    fn rejects_created_branch_response_with_wrong_parent() {
        let response = created_branch_response("child-ref", "other-parent-ref", false);
        let err = validated_branch_ref(&response, "parent-ref").unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[test]
    fn rejects_created_branch_response_for_default_branch() {
        let response = created_branch_response("child-ref", "parent-ref", true);
        let err = validated_branch_ref(&response, "parent-ref").unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[test]
    fn restore_permission_uses_elevated_demo_permission() {
        let ctx = AuthContext {
            identity: "github:alice".to_string(),
            permissions: vec!["demo.restore_main".to_string()],
        };
        assert!(require_permission(&ctx, "demo.restore_main").is_ok());

        let ctx = AuthContext {
            identity: "github:alice".to_string(),
            permissions: vec!["demo.save".to_string()],
        };
        let err = require_permission(&ctx, "demo.restore_main").unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn malformed_refresh_token_is_rejected() {
        assert!(parse_refresh_token("").is_err());
        assert!(parse_refresh_token("selector-only").is_err());
        assert!(parse_refresh_token(".secret").is_err());
        assert!(parse_refresh_token("selector.").is_err());
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
