use serde::Deserialize;
use tonic::Status;

#[derive(Debug, Deserialize)]
pub struct GitHubUser {
    pub login: String,
}

pub async fn validate_token(token: &str) -> Result<GitHubUser, Status> {
    let client = reqwest::Client::new();

    let response = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", "supabased-server")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| Status::unavailable(format!("GitHub API request failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Status::unauthenticated(
            "invalid GitHub token — check that your PAT is valid and not expired",
        ));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Status::internal(format!(
            "GitHub API returned {status}: {body}"
        )));
    }

    let user: GitHubUser = response
        .json()
        .await
        .map_err(|e| Status::internal(format!("failed to parse GitHub response: {e}")))?;

    Ok(user)
}
