use tonic::{Request, Response, Status};
use tokio_rusqlite::Connection;

use supabased_proto::supabased::{
    supabased_server::Supabased,
    AuthRequest, AuthResponse,
    WhoAmIRequest, WhoAmIResponse,
    auth_request::Method,
};

use crate::auth;
use crate::auth::AuthContext;
use crate::github;

pub struct SupabasedService {
    pub db: Connection,
    pub jwt_secret: Vec<u8>,
}

impl SupabasedService {
    pub fn new(db: Connection, jwt_secret: Vec<u8>) -> Self {
        Self { db, jwt_secret }
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
        let req = request.into_inner();
        let method = req.method.ok_or_else(|| {
            Status::invalid_argument("auth method required")
        })?;

        let identity = match method {
            Method::GithubToken(token) => {
                let user = github::validate_token(&token).await?;
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
}
