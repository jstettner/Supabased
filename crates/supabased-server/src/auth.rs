use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
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

pub fn verify_token(
    secret: &[u8],
    token: &str,
) -> Result<Claims, jsonwebtoken::errors::Error> {
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
                let val = val.to_str().map_err(|_| {
                    Status::unauthenticated("invalid authorization header")
                })?;
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

        let claims = verify_token(&self.secret, &token).map_err(|e| {
            Status::unauthenticated(format!("invalid token: {e}"))
        })?;

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
