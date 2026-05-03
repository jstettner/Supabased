use serde::Deserialize;
use std::time::Duration;
use tonic::Status;

const USER_AGENT: &str = "supabased-server";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
pub struct GitHubUser {
    pub login: String,
}

#[derive(Debug, Deserialize)]
pub struct DeviceAuthStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub interval: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DeviceAuthPoll {
    Complete {
        access_token: String,
        scope: String,
        token_type: String,
    },
    Pending {
        interval: Option<i64>,
    },
    SlowDown {
        interval: Option<i64>,
    },
}

#[derive(Debug, Deserialize)]
struct DeviceAuthPollSuccess {
    access_token: String,
    scope: String,
    token_type: String,
}

#[derive(Debug, Deserialize)]
struct DeviceAuthError {
    error: String,
    error_description: Option<String>,
    interval: Option<i64>,
}

pub async fn validate_token(token: &str) -> Result<GitHubUser, Status> {
    let client = http_client()?;

    let response = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| Status::unavailable(format!("GitHub API request failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Status::unauthenticated(
            "invalid GitHub OAuth token -- authorization may have been revoked or expired",
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
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| Status::internal(format!("failed to build HTTP client: {e}")))?;

    let url = format!("https://api.github.com/orgs/{org}/members/{username}");

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", USER_AGENT)
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
            "org membership check failed -- ensure your GitHub token has the 'read:org' scope",
        )),
        status => {
            let body = response.text().await.unwrap_or_default();
            Err(Status::internal(format!(
                "GitHub org membership API returned {status}: {body}"
            )))
        }
    }
}

pub async fn start_device_auth(client_id: &str, scope: &str) -> Result<DeviceAuthStart, Status> {
    let params = [("client_id", client_id), ("scope", scope)];
    let response = http_client()?
        .post("https://github.com/login/device/code")
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json")
        .form(&params)
        .send()
        .await
        .map_err(|e| Status::unavailable(format!("GitHub OAuth request failed: {e}")))?;

    parse_device_auth_start_response(response).await
}

pub async fn poll_device_auth(
    client_id: &str,
    device_code: &str,
) -> Result<DeviceAuthPoll, Status> {
    let params = [
        ("client_id", client_id),
        ("device_code", device_code),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ];

    let response = http_client()?
        .post("https://github.com/login/oauth/access_token")
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json")
        .form(&params)
        .send()
        .await
        .map_err(|e| Status::unavailable(format!("GitHub OAuth request failed: {e}")))?;

    parse_device_auth_poll_response(response).await
}

fn http_client() -> Result<reqwest::Client, Status> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| Status::internal(format!("failed to build GitHub HTTP client: {e}")))
}

async fn parse_device_auth_start_response(
    response: reqwest::Response,
) -> Result<DeviceAuthStart, Status> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(Status::internal(format!(
            "GitHub OAuth device-code endpoint returned {status}: {body}"
        )));
    }

    parse_device_auth_start_json(&body)
}

fn parse_device_auth_start_json(body: &str) -> Result<DeviceAuthStart, Status> {
    serde_json::from_str(body)
        .map_err(|e| Status::internal(format!("failed to parse GitHub OAuth response: {e}")))
}

async fn parse_device_auth_poll_response(
    response: reqwest::Response,
) -> Result<DeviceAuthPoll, Status> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(Status::internal(format!(
            "GitHub OAuth token endpoint returned {status}: {body}"
        )));
    }

    parse_device_auth_poll_json(&body)
}

fn parse_device_auth_poll_json(body: &str) -> Result<DeviceAuthPoll, Status> {
    if let Ok(success) = serde_json::from_str::<DeviceAuthPollSuccess>(body) {
        return Ok(DeviceAuthPoll::Complete {
            access_token: success.access_token,
            scope: success.scope,
            token_type: success.token_type,
        });
    }

    let error: DeviceAuthError = serde_json::from_str(body)
        .map_err(|e| Status::internal(format!("failed to parse GitHub OAuth response: {e}")))?;

    match error.error.as_str() {
        "authorization_pending" => Ok(DeviceAuthPoll::Pending {
            interval: error.interval,
        }),
        "slow_down" => Ok(DeviceAuthPoll::SlowDown {
            interval: error.interval,
        }),
        "expired_token" => Err(Status::deadline_exceeded(github_oauth_error_message(
            "GitHub OAuth device code expired",
            &error,
        ))),
        "access_denied" => Err(Status::permission_denied(github_oauth_error_message(
            "GitHub OAuth authorization was denied",
            &error,
        ))),
        "device_flow_disabled" => Err(Status::failed_precondition(github_oauth_error_message(
            "GitHub OAuth device flow is disabled for this app",
            &error,
        ))),
        other => Err(Status::internal(github_oauth_error_message(
            &format!("GitHub OAuth returned error '{other}'"),
            &error,
        ))),
    }
}

fn github_oauth_error_message(prefix: &str, error: &DeviceAuthError) -> String {
    match error.error_description.as_deref() {
        Some(description) if !description.is_empty() => format!("{prefix}: {description}"),
        _ => prefix.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    #[test]
    fn maps_device_auth_start_success() {
        let start = parse_device_auth_start_json(
            r#"{"device_code":"device-123","user_code":"ABCD-EFGH","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#,
        )
        .unwrap();

        assert_eq!(start.device_code, "device-123");
        assert_eq!(start.user_code, "ABCD-EFGH");
        assert_eq!(start.verification_uri, "https://github.com/login/device");
        assert_eq!(start.expires_in, 900);
        assert_eq!(start.interval, 5);
    }

    #[test]
    fn maps_access_token_success_to_complete() {
        let poll = parse_device_auth_poll_json(
            r#"{"access_token":"gho_token","scope":"read:org","token_type":"bearer"}"#,
        )
        .unwrap();

        assert_eq!(
            poll,
            DeviceAuthPoll::Complete {
                access_token: "gho_token".to_string(),
                scope: "read:org".to_string(),
                token_type: "bearer".to_string(),
            }
        );
    }

    #[test]
    fn maps_authorization_pending_to_pending() {
        let poll = parse_device_auth_poll_json(r#"{"error":"authorization_pending"}"#).unwrap();

        assert_eq!(poll, DeviceAuthPoll::Pending { interval: None });
    }

    #[test]
    fn maps_authorization_pending_interval_to_pending() {
        let poll =
            parse_device_auth_poll_json(r#"{"error":"authorization_pending","interval":10}"#)
                .unwrap();

        assert_eq!(poll, DeviceAuthPoll::Pending { interval: Some(10) });
    }

    #[test]
    fn maps_slow_down_to_slow_down() {
        let poll = parse_device_auth_poll_json(r#"{"error":"slow_down","interval":15}"#).unwrap();

        assert_eq!(poll, DeviceAuthPoll::SlowDown { interval: Some(15) });
    }

    #[test]
    fn maps_expired_token_to_deadline_exceeded() {
        let err = parse_device_auth_poll_json(
            r#"{"error":"expired_token","error_description":"The device code has expired."}"#,
        )
        .unwrap_err();

        assert_eq!(err.code(), Code::DeadlineExceeded);
        assert!(err.message().contains("expired"));
    }

    #[test]
    fn maps_access_denied_to_permission_denied() {
        let err = parse_device_auth_poll_json(
            r#"{"error":"access_denied","error_description":"The user denied your request."}"#,
        )
        .unwrap_err();

        assert_eq!(err.code(), Code::PermissionDenied);
        assert!(err.message().contains("denied"));
    }

    #[test]
    fn maps_device_flow_disabled_to_failed_precondition() {
        let err = parse_device_auth_poll_json(
            r#"{"error":"device_flow_disabled","error_description":"Device flow is disabled."}"#,
        )
        .unwrap_err();

        assert_eq!(err.code(), Code::FailedPrecondition);
        assert!(err.message().contains("disabled"));
    }
}
