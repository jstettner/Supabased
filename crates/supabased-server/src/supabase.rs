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

fn api_keys_url(branch_ref: &str) -> String {
    format!("{BASE_URL}/v1/projects/{branch_ref}/api-keys?reveal=true")
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
    pub publishable_key: String,
    pub secret_key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteBranchOutcome {
    Deleted,
    Missing,
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
    pub async fn delete_branch(&self, branch_ref: &str) -> Result<DeleteBranchOutcome, Status> {
        let client = Self::http_client()?;
        let url = format!("{BASE_URL}/v1/branches/{branch_ref}");

        let response = client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", "supabased-server")
            .send()
            .await
            .map_err(|e| Status::unavailable(format!("Supabase API request failed: {e}")))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(DeleteBranchOutcome::Missing);
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Status::internal(format!(
                "Supabase API returned {status}: {body}"
            )));
        }

        Ok(DeleteBranchOutcome::Deleted)
    }

    /// Get API keys for a branch (each branch is its own project).
    /// `GET /v1/projects/{branch_ref}/api-keys`
    pub async fn get_api_keys(&self, branch_ref: &str) -> Result<Vec<ApiKeyResponse>, Status> {
        let client = Self::http_client()?;
        let url = api_keys_url(branch_ref);

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

fn select_modern_key<'a>(
    keys: &'a [ApiKeyResponse],
    key_type: &str,
    expected_prefix: &str,
) -> Result<&'a str, Status> {
    let candidates: Vec<_> = keys
        .iter()
        .filter(|key| key.key_type.as_deref() == Some(key_type))
        .collect();

    let selected = match candidates.as_slice() {
        [] => {
            return Err(Status::internal(format!(
                "no modern {key_type} API key found; create one in the Supabase API Keys settings"
            )));
        }
        [only] => *only,
        many => {
            let defaults: Vec<_> = many
                .iter()
                .filter(|key| key.name.as_deref() == Some("default"))
                .collect();
            match defaults.as_slice() {
                [default] => **default,
                _ => {
                    return Err(Status::internal(format!(
                        "multiple {key_type} API keys found without a unique key named 'default'"
                    )));
                }
            }
        }
    };

    if !selected.api_key.starts_with(expected_prefix) {
        return Err(Status::internal(format!(
            "selected {key_type} API key does not have the expected {expected_prefix} prefix"
        )));
    }

    let suffix = &selected.api_key[expected_prefix.len()..];
    if suffix.is_empty()
        || suffix.contains('*')
        || suffix.contains("...")
        || suffix.contains('…')
        || suffix.contains('•')
    {
        return Err(Status::internal(format!(
            "selected {key_type} API key is masked or incomplete; ensure the Management API response reveals key values"
        )));
    }

    Ok(&selected.api_key)
}

/// Extract modern publishable and secret keys from the API keys response.
/// Legacy JWT-based `anon` and `service_role` entries are deliberately ignored.
pub fn extract_credentials(
    keys: &[ApiKeyResponse],
    branch_ref: &str,
) -> Result<BranchCredentialSet, Status> {
    let publishable_key = select_modern_key(keys, "publishable", "sb_publishable_")?;
    let secret_key = select_modern_key(keys, "secret", "sb_secret_")?;

    Ok(BranchCredentialSet {
        api_url: format!("https://{branch_ref}.supabase.co"),
        publishable_key: publishable_key.to_owned(),
        secret_key: secret_key.to_owned(),
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

    #[test]
    fn api_keys_url_requests_revealed_values() {
        assert_eq!(
            api_keys_url("branch-ref"),
            "https://api.supabase.com/v1/projects/branch-ref/api-keys?reveal=true"
        );
    }

    fn key(api_key: &str, name: Option<&str>, key_type: Option<&str>) -> ApiKeyResponse {
        ApiKeyResponse {
            api_key: api_key.into(),
            name: name.map(str::to_owned),
            key_type: key_type.map(str::to_owned),
        }
    }

    fn make_legacy_keys() -> Vec<ApiKeyResponse> {
        vec![
            key("legacy-anon", Some("anon"), Some("legacy")),
            key("legacy-service-role", Some("service_role"), Some("legacy")),
        ]
    }

    fn make_modern_keys() -> Vec<ApiKeyResponse> {
        vec![
            key(
                "sb_publishable_publishable-key-value",
                Some("default"),
                Some("publishable"),
            ),
            key(
                "sb_secret_secret-key-value",
                Some("default"),
                Some("secret"),
            ),
        ]
    }

    #[test]
    fn extract_credentials_rejects_legacy_only_keys() {
        let keys = make_legacy_keys();
        let error = extract_credentials(&keys, "branch-ref-abc")
            .err()
            .expect("legacy keys should be rejected");
        assert!(error.message().contains("no modern publishable API key"));
    }

    #[test]
    fn extract_credentials_modern_keys() {
        let keys = make_modern_keys();
        let creds = extract_credentials(&keys, "branch-ref-xyz").unwrap();
        assert_eq!(creds.api_url, "https://branch-ref-xyz.supabase.co");
        assert_eq!(
            creds.publishable_key,
            "sb_publishable_publishable-key-value"
        );
        assert_eq!(creds.secret_key, "sb_secret_secret-key-value");
    }

    #[test]
    fn extract_credentials_ignores_legacy_keys_that_come_first() {
        let mut keys = make_legacy_keys();
        keys.extend(make_modern_keys());
        let creds = extract_credentials(&keys, "ref").unwrap();
        assert!(creds.publishable_key.starts_with("sb_publishable_"));
        assert!(creds.secret_key.starts_with("sb_secret_"));
    }

    #[test]
    fn extract_credentials_requires_both_modern_key_types() {
        let only_publishable = vec![key(
            "sb_publishable_value",
            Some("default"),
            Some("publishable"),
        )];
        let error = extract_credentials(&only_publishable, "ref")
            .err()
            .expect("a secret key should be required");
        assert!(error.message().contains("no modern secret API key"));
    }

    #[test]
    fn extract_credentials_empty_keys() {
        let error = extract_credentials(&[], "ref")
            .err()
            .expect("an empty key list should be rejected");
        assert!(error.message().contains("no modern publishable API key"));
    }

    #[test]
    fn extract_credentials_prefers_default_among_multiple_candidates() {
        let keys = vec![
            key("sb_publishable_other", Some("other"), Some("publishable")),
            key(
                "sb_publishable_default",
                Some("default"),
                Some("publishable"),
            ),
            key("sb_secret_other", Some("other"), Some("secret")),
            key("sb_secret_default", Some("default"), Some("secret")),
        ];
        let creds = extract_credentials(&keys, "ref").unwrap();
        assert_eq!(creds.publishable_key, "sb_publishable_default");
        assert_eq!(creds.secret_key, "sb_secret_default");
    }

    #[test]
    fn extract_credentials_accepts_sole_non_default_candidates() {
        let keys = vec![
            key("sb_publishable_custom", Some("custom"), Some("publishable")),
            key("sb_secret_custom", Some("custom"), Some("secret")),
        ];
        let creds = extract_credentials(&keys, "ref").unwrap();
        assert_eq!(creds.publishable_key, "sb_publishable_custom");
        assert_eq!(creds.secret_key, "sb_secret_custom");
    }

    #[test]
    fn extract_credentials_rejects_ambiguous_candidates() {
        let mut keys = make_modern_keys();
        keys[0].name = Some("one".into());
        keys.push(key("sb_publishable_two", Some("two"), Some("publishable")));
        let error = extract_credentials(&keys, "ref")
            .err()
            .expect("ambiguous keys should be rejected");
        assert!(
            error
                .message()
                .contains("without a unique key named 'default'")
        );
    }

    #[test]
    fn extract_credentials_rejects_malformed_values_without_leaking_them() {
        let malformed = "wrong_publishable_sensitive-value";
        let keys = vec![
            key(malformed, Some("default"), Some("publishable")),
            key("sb_secret_valid", Some("default"), Some("secret")),
        ];
        let error = extract_credentials(&keys, "ref")
            .err()
            .expect("malformed keys should be rejected");
        assert!(error.message().contains("expected sb_publishable_ prefix"));
        assert!(!error.message().contains(malformed));
    }

    #[test]
    fn extract_credentials_rejects_masked_values_without_leaking_them() {
        let masked = "sb_secret_********";
        let keys = vec![
            key("sb_publishable_valid", Some("default"), Some("publishable")),
            key(masked, Some("default"), Some("secret")),
        ];
        let error = extract_credentials(&keys, "ref")
            .err()
            .expect("masked keys should be rejected");
        assert!(error.message().contains("masked or incomplete"));
        assert!(!error.message().contains(masked));
    }
}
