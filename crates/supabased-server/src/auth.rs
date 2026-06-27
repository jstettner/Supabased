use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use tonic::{Request, Status};

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub role: String,
    pub permissions: Vec<String>,
    pub iat: i64,
    pub exp: i64,
}

pub const DEFAULT_PERMISSIONS: &[&str] = &[
    "branches.create",
    "branches.list",
    "branches.delete_own",
    "branches.get_credentials_own",
    "demo.save",
    "demo.list",
    "demo.delete_own",
    "info.read",
];

pub fn create_token(
    secret: &[u8],
    identity: &str,
    permissions: &[String],
) -> Result<(String, i64), jsonwebtoken::errors::Error> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let exp = now + 8 * 3600; // 8 hours

    let claims = Claims {
        sub: identity.to_string(),
        role: "developer".to_string(),
        permissions: permissions.to_vec(),
        iat: now,
        exp,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )?;

    Ok((token, exp))
}

pub fn verify_token(secret: &[u8], token: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret),
        &Validation::default(),
    )?;

    Ok(token_data.claims)
}

#[derive(Clone, Debug)]
pub struct AuthContext {
    pub identity: String,
    pub permissions: Vec<String>,
}

#[derive(Clone)]
pub struct JwtInterceptor {
    secret: Vec<u8>,
}

impl JwtInterceptor {
    pub fn new(secret: Vec<u8>) -> Self {
        Self { secret }
    }
}

impl tonic::service::Interceptor for JwtInterceptor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        let token = match req.metadata().get("authorization") {
            Some(val) => {
                let val = val
                    .to_str()
                    .map_err(|_| Status::unauthenticated("invalid authorization header"))?;
                val.strip_prefix("Bearer ")
                    .ok_or_else(|| Status::unauthenticated("expected Bearer token"))?
                    .to_string()
            }
            None => {
                // No token — let the request through without AuthContext.
                // Handlers that need auth will check for AuthContext themselves.
                // This allows Authenticate to work without a token.
                return Ok(req);
            }
        };

        let claims = verify_token(&self.secret, &token)
            .map_err(|e| Status::unauthenticated(format!("invalid token: {e}")))?;

        req.extensions_mut().insert(AuthContext {
            identity: claims.sub,
            permissions: claims.permissions,
        });

        Ok(req)
    }
}

pub fn make_interceptor(secret: Vec<u8>) -> JwtInterceptor {
    JwtInterceptor::new(secret)
}

pub fn require_permission(ctx: &AuthContext, perm: &str) -> Result<(), Status> {
    if ctx.permissions.iter().any(|p| p == perm) {
        Ok(())
    } else {
        Err(Status::permission_denied(format!(
            "missing required permission: {perm}"
        )))
    }
}

pub fn require_permission_or_owner(
    ctx: &AuthContext,
    perm_own: &str,
    perm_any: &str,
    owner: &str,
) -> Result<(), Status> {
    if ctx.permissions.iter().any(|p| p == perm_any) {
        return Ok(());
    }
    if ctx.permissions.iter().any(|p| p == perm_own) && ctx.identity == owner {
        return Ok(());
    }
    Err(Status::permission_denied(format!(
        "missing required permission: {perm_any} or {perm_own} (as owner)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(identity: &str, perms: &[&str]) -> AuthContext {
        AuthContext {
            identity: identity.to_string(),
            permissions: perms.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn require_permission_grants_when_present() {
        let c = ctx("github:alice", &["branches.create", "branches.list"]);
        assert!(require_permission(&c, "branches.create").is_ok());
    }

    #[test]
    fn require_permission_denies_when_absent() {
        let c = ctx("github:alice", &["branches.list"]);
        let err = require_permission(&c, "branches.create").unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn require_permission_or_owner_grants_with_any() {
        let c = ctx("github:alice", &["branches.delete_any"]);
        assert!(
            require_permission_or_owner(
                &c,
                "branches.delete_own",
                "branches.delete_any",
                "github:bob", // not the owner, but has _any
            )
            .is_ok()
        );
    }

    #[test]
    fn require_permission_or_owner_grants_own_when_owner() {
        let c = ctx("github:alice", &["branches.delete_own"]);
        assert!(
            require_permission_or_owner(
                &c,
                "branches.delete_own",
                "branches.delete_any",
                "github:alice", // is the owner
            )
            .is_ok()
        );
    }

    #[test]
    fn require_permission_or_owner_denies_own_when_not_owner() {
        let c = ctx("github:alice", &["branches.delete_own"]);
        let err = require_permission_or_owner(
            &c,
            "branches.delete_own",
            "branches.delete_any",
            "github:bob", // not the owner
        )
        .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn require_permission_or_owner_denies_with_neither() {
        let c = ctx("github:alice", &["branches.list"]);
        let err = require_permission_or_owner(
            &c,
            "branches.delete_own",
            "branches.delete_any",
            "github:alice",
        )
        .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }
}
