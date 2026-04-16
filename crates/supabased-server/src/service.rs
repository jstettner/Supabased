use std::time::Duration;

use tonic::{Request, Response, Status};
use tokio_rusqlite::Connection;

use supabased_proto::supabased::{
    supabased_server::Supabased,
    AuthRequest, AuthResponse,
    WhoAmIRequest, WhoAmIResponse,
    auth_request::Method,
    ListProjectsRequest, ListProjectsResponse,
    CreateBranchRequest, CreateBranchResponse,
    ListBranchesRequest, ListBranchesResponse,
    DeleteBranchRequest, DeleteBranchResponse,
    GetBranchCredentialsRequest, BranchCredentials,
};

use crate::auth;
use crate::auth::{AuthContext, require_permission, require_permission_or_owner};
use crate::config::ServerConfig;
use crate::db;
use crate::github;
use crate::rate_limit::RateLimiter;
use crate::supabase;
use crate::supabase::SupabaseClient;

pub struct SupabasedService {
    pub db: Connection,
    pub jwt_secret: Vec<u8>,
    pub github_org: String,
    pub rate_limiter: RateLimiter,
    pub supabase_client: SupabaseClient,
    pub config: ServerConfig,
}

impl SupabasedService {
    pub fn new(
        db: Connection,
        jwt_secret: Vec<u8>,
        github_org: String,
        supabase_client: SupabaseClient,
        config: ServerConfig,
    ) -> Self {
        let rate_limiter = RateLimiter::new(5, Duration::from_secs(60));
        rate_limiter.spawn_cleanup_task();
        Self { db, jwt_secret, github_org, rate_limiter, supabase_client, config }
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
        Ok(Response::new(WhoAmIResponse {
            identity: ctx.identity.clone(),
            permissions: ctx.permissions.clone(),
            accessible_branches: vec![],
        }))
    }

    async fn authenticate(
        &self,
        request: Request<AuthRequest>,
    ) -> Result<Response<AuthResponse>, Status> {
        if let Some(addr) = request.remote_addr() {
            self.rate_limiter.check_rate_limit(addr.ip())?;
        }

        let req = request.into_inner();
        let method = req.method.ok_or_else(|| {
            Status::invalid_argument("auth method required")
        })?;

        let identity = match method {
            Method::GithubToken(token) => {
                let user = github::validate_token(&token).await?;
                github::check_org_membership(&token, &self.github_org, &user.login).await?;
                format!("github:{}", user.login)
            }
            Method::ApiKey(_) => {
                return Err(Status::unimplemented("API key auth not yet supported"));
            }
        };

        let permissions: Vec<String> = auth::DEFAULT_PERMISSIONS
            .iter()
            .map(|s| s.to_string())
            .collect();

        let (token, expires_at) = auth::create_token(
            &self.jwt_secret,
            &identity,
            &permissions,
        )
        .map_err(|e| Status::internal(format!("token creation failed: {e}")))?;

        Ok(Response::new(AuthResponse {
            session_token: token,
            identity,
            permissions,
            expires_at,
        }))
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

        Ok(Response::new(ListProjectsResponse { projects }))
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
            .ok_or_else(|| {
                Status::not_found(format!("unknown project: {}", req.project_name))
            })?;

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

        Ok(Response::new(CreateBranchResponse {
            branch: Some(supabased_proto::supabased::BranchInfo {
                branch_name: req.branch_name,
                project_name: req.project_name,
                status: branch_resp.status.unwrap_or_default(),
                created_at: branch_resp.created_at.unwrap_or_default(),
            }),
        }))
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

        Ok(Response::new(ListBranchesResponse { branches }))
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

        Ok(Response::new(DeleteBranchResponse { deleted: true }))
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

        Ok(Response::new(BranchCredentials {
            branch_name: req.branch_name,
            project_name: req.project_name,
            api_url: creds.api_url,
            anon_key: creds.anon_key,
            service_role_key: creds.service_role_key,
        }))
    }
}
