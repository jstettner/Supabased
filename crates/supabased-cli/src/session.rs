use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Session {
    pub session_token: String,
    pub identity: String,
    pub expires_at: i64,
}

pub fn session_path() -> PathBuf {
    let config_dir = dirs::config_dir()
        .expect("could not determine config directory")
        .join("supabased");
    config_dir.join("session.json")
}

pub fn load_session() -> Result<Session, String> {
    let path = session_path();
    let contents = fs::read_to_string(&path).map_err(|_| {
        format!(
            "not logged in — run `supabased login` first\n  (no session file at {})",
            path.display()
        )
    })?;

    let session: Session = serde_json::from_str(&contents).map_err(|e| {
        format!("corrupt session file at {}: {e}", path.display())
    })?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    if session.expires_at <= now {
        return Err("session expired — run `supabased login` again".to_string());
    }

    Ok(session)
}

pub fn save_session(session: &Session) -> Result<(), Box<dyn std::error::Error>> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(session)?;
    fs::write(&path, json)?;
    Ok(())
}
