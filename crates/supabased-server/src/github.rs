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

pub async fn check_org_membership(token: &str, org: &str, username: &str) -> Result<(), Status> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Status::internal(format!("failed to build HTTP client: {e}")))?;

    let url = format!("https://api.github.com/orgs/{org}/members/{username}");

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", "supabased-server")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| Status::unavailable(format!("GitHub API request failed: {e}")))?;

    match response.status() {
        reqwest::StatusCode::NO_CONTENT => Ok(()),
        reqwest::StatusCode::NOT_FOUND => Err(Status::permission_denied(format!(
            "user '{username}' is not a member of the '{org}' GitHub organization"
        ))),
        reqwest::StatusCode::FOUND => Err(Status::permission_denied(
            "org membership check failed — ensure your GitHub token has the 'read:org' scope",
        )),
        status => {
            let body = response.text().await.unwrap_or_default();
            Err(Status::internal(format!(
                "GitHub org membership API returned {status}: {body}"
            )))
        }
    }
}
