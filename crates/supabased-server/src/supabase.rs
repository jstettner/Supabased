use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use tonic::Status;

const BASE_URL: &str = "https://api.supabase.com";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

fn create_branch_body(branch_name: &str) -> Value {
    serde_json::json!({
        "branch_name": branch_name,
        "is_default": false,
        "with_data": true,
    })
}

pub struct SupabaseClient {
    token: String,
}

/// Full branch response from `POST /v1/projects/{ref}/branches`
/// and `GET /v1/projects/{ref}/branches` (list item).
/// Models the complete API shape — we selectively map to proto types.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct BranchResponse {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub project_ref: Option<String>,
    #[serde(default)]
    pub parent_project_ref: Option<String>,
    #[serde(default)]
    pub is_default: Option<bool>,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// API key response from `GET /v1/projects/{ref}/api-keys`.
/// Handles both legacy (`name` field) and new opaque (`type` field) key formats.
#[derive(Debug, Deserialize)]
pub struct ApiKeyResponse {
    pub api_key: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type", default)]
    pub key_type: Option<String>,
}

/// Extracted credential set ready for proto conversion.
pub struct BranchCredentialSet {
    pub api_url: String,
    pub anon_key: String,
    pub service_role_key: String,
}

impl SupabaseClient {
    pub fn new(token: String) -> Self {
        Self { token }
    }

    fn http_client() -> Result<reqwest::Client, Status> {
        reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| Status::internal(format!("failed to build Supabase HTTP client: {e}")))
    }

    /// Create a branch with data from a parent project.
    /// `POST /v1/projects/{project_ref}/branches`
    pub async fn create_branch(
        &self,
        project_ref: &str,
        branch_name: &str,
    ) -> Result<BranchResponse, Status> {
        let client = Self::http_client()?;
        let url = format!("{BASE_URL}/v1/projects/{project_ref}/branches");

        let body = create_branch_body(branch_name);

        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", "supabased-server")
            .json(&body)
            .send()
            .await
            .map_err(|e| Status::unavailable(format!("Supabase API request failed: {e}")))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Status::unauthenticated(
                "Supabase access token is invalid or expired",
            ));
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Status::internal(format!(
                "Supabase API returned {status}: {body}"
            )));
        }

        response
            .json::<BranchResponse>()
            .await
            .map_err(|e| Status::internal(format!("failed to parse Supabase response: {e}")))
    }

    /// List all branches for a project.
    /// `GET /v1/projects/{project_ref}/branches`
    pub async fn list_branches(&self, project_ref: &str) -> Result<Vec<BranchResponse>, Status> {
        let client = Self::http_client()?;
        let url = format!("{BASE_URL}/v1/projects/{project_ref}/branches");

        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", "supabased-server")
            .send()
            .await
            .map_err(|e| Status::unavailable(format!("Supabase API request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Status::internal(format!(
                "Supabase API returned {status}: {body}"
            )));
        }

        response
            .json::<Vec<BranchResponse>>()
            .await
            .map_err(|e| Status::internal(format!("failed to parse Supabase response: {e}")))
    }

    /// Delete a single branch.
    /// `DELETE /v1/branches/{branch_ref}`
    /// NOTE: This is NOT `/v1/projects/{ref}/branches` which disables branching entirely.
    pub async fn delete_branch(&self, branch_ref: &str) -> Result<(), Status> {
        let client = Self::http_client()?;
        let url = format!("{BASE_URL}/v1/branches/{branch_ref}");

        let response = client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", "supabased-server")
            .send()
            .await
            .map_err(|e| Status::unavailable(format!("Supabase API request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Status::internal(format!(
                "Supabase API returned {status}: {body}"
            )));
        }

        Ok(())
    }

    /// Get API keys for a branch (each branch is its own project).
    /// `GET /v1/projects/{branch_ref}/api-keys`
    pub async fn get_api_keys(&self, branch_ref: &str) -> Result<Vec<ApiKeyResponse>, Status> {
        let client = Self::http_client()?;
        let url = format!("{BASE_URL}/v1/projects/{branch_ref}/api-keys");

        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", "supabased-server")
            .send()
            .await
            .map_err(|e| Status::unavailable(format!("Supabase API request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Status::internal(format!(
                "Supabase API returned {status}: {body}"
            )));
        }

        response
            .json::<Vec<ApiKeyResponse>>()
            .await
            .map_err(|e| Status::internal(format!("failed to parse Supabase response: {e}")))
    }
}

/// Extract anon and service_role keys from the API keys response.
/// Handles both legacy keys (matched by `name` field: "anon" / "service_role")
/// and new opaque keys (matched by `type` field: "publishable" / "secret").
pub fn extract_credentials(
    keys: &[ApiKeyResponse],
    branch_ref: &str,
) -> Result<BranchCredentialSet, Status> {
    let anon_key = keys
        .iter()
        .find(|k| k.name.as_deref() == Some("anon") || k.key_type.as_deref() == Some("publishable"))
        .ok_or_else(|| Status::internal("no anon/publishable key found in API keys response"))?;

    let service_role_key = keys
        .iter()
        .find(|k| {
            k.name.as_deref() == Some("service_role") || k.key_type.as_deref() == Some("secret")
        })
        .ok_or_else(|| Status::internal("no service_role/secret key found in API keys response"))?;

    Ok(BranchCredentialSet {
        api_url: format!("https://{branch_ref}.supabase.co"),
        anon_key: anon_key.api_key.clone(),
        service_role_key: service_role_key.api_key.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_branch_body_does_not_set_git_branch_metadata() {
        let body = create_branch_body("demo/farmer");

        assert_eq!(body["branch_name"], "demo/farmer");
        assert_eq!(body["is_default"], false);
        assert_eq!(body["with_data"], true);
        assert!(body.get("git_branch").is_none());
    }

    fn make_legacy_keys() -> Vec<ApiKeyResponse> {
        vec![
            ApiKeyResponse {
                api_key: "anon-key-value".into(),
                name: Some("anon".into()),
                key_type: None,
            },
            ApiKeyResponse {
                api_key: "service-role-key-value".into(),
                name: Some("service_role".into()),
                key_type: None,
            },
        ]
    }

    fn make_opaque_keys() -> Vec<ApiKeyResponse> {
        vec![
            ApiKeyResponse {
                api_key: "publishable-key-value".into(),
                name: None,
                key_type: Some("publishable".into()),
            },
            ApiKeyResponse {
                api_key: "secret-key-value".into(),
                name: None,
                key_type: Some("secret".into()),
            },
        ]
    }

    #[test]
    fn extract_credentials_legacy_keys() {
        let keys = make_legacy_keys();
        let creds = extract_credentials(&keys, "branch-ref-abc").unwrap();
        assert_eq!(creds.api_url, "https://branch-ref-abc.supabase.co");
        assert_eq!(creds.anon_key, "anon-key-value");
        assert_eq!(creds.service_role_key, "service-role-key-value");
    }

    #[test]
    fn extract_credentials_opaque_keys() {
        let keys = make_opaque_keys();
        let creds = extract_credentials(&keys, "branch-ref-xyz").unwrap();
        assert_eq!(creds.api_url, "https://branch-ref-xyz.supabase.co");
        assert_eq!(creds.anon_key, "publishable-key-value");
        assert_eq!(creds.service_role_key, "secret-key-value");
    }

    #[test]
    fn extract_credentials_missing_anon_key() {
        let keys = vec![ApiKeyResponse {
            api_key: "service-role-key-value".into(),
            name: Some("service_role".into()),
            key_type: None,
        }];
        let result = extract_credentials(&keys, "ref");
        assert!(result.is_err());
    }

    #[test]
    fn extract_credentials_missing_service_role_key() {
        let keys = vec![ApiKeyResponse {
            api_key: "anon-key-value".into(),
            name: Some("anon".into()),
            key_type: None,
        }];
        let result = extract_credentials(&keys, "ref");
        assert!(result.is_err());
    }

    #[test]
    fn extract_credentials_empty_keys() {
        let result = extract_credentials(&[], "ref");
        assert!(result.is_err());
    }
}
