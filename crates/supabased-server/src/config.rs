use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub projects: Vec<ProjectConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    #[serde(rename = "ref")]
    pub project_ref: String,
}

impl ServerConfig {
    pub fn resolve_project(&self, name: &str) -> Option<&ProjectConfig> {
        self.projects.iter().find(|p| p.name == name)
    }
}

pub fn load_config(path: &str) -> Result<(ServerConfig, String), String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read config file {path}: {e}"))?;

    use sha2::{Sha256, Digest};
    let config_hash = format!("{:x}", Sha256::digest(contents.as_bytes()));

    let config: ServerConfig =
        toml::from_str(&contents).map_err(|e| format!("failed to parse config file {path}: {e}"))?;
    if config.projects.is_empty() {
        return Err(format!("config file {path} must define at least one [[projects]] entry"));
    }
    config
        .projects
        .iter()
        .try_for_each(|p| {
            if p.name.is_empty() {
                Err(format!("project entry in {path} has empty name"))
            } else if p.project_ref.is_empty() {
                Err(format!("project '{}' in {path} has empty ref", p.name))
            } else {
                Ok(())
            }
        })?;
    Ok((config, config_hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_valid_toml() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[[projects]]
name = "staging"
ref = "abcdefghijklmnop"

[[projects]]
name = "production"
ref = "qrstuvwxyz123456"
"#
        )
        .unwrap();

        let (config, hash) = load_config(f.path().to_str().unwrap()).unwrap();
        assert_eq!(config.projects.len(), 2);
        assert_eq!(config.projects[0].name, "staging");
        assert_eq!(config.projects[0].project_ref, "abcdefghijklmnop");
        assert_eq!(config.projects[1].name, "production");
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA-256 hex is 64 chars
    }

    #[test]
    fn rejects_missing_file() {
        let result = load_config("/nonexistent/path.toml");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to read"));
    }

    #[test]
    fn rejects_malformed_toml() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "this is not valid toml {{{{").unwrap();
        let result = load_config(f.path().to_str().unwrap());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to parse"));
    }

    #[test]
    fn rejects_empty_projects() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "projects = []").unwrap();
        let result = load_config(f.path().to_str().unwrap());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("at least one"));
    }

    #[test]
    fn resolve_project_finds_match() {
        let config = ServerConfig {
            projects: vec![
                ProjectConfig { name: "staging".into(), project_ref: "abc".into() },
                ProjectConfig { name: "prod".into(), project_ref: "xyz".into() },
            ],
        };
        let p = config.resolve_project("staging").unwrap();
        assert_eq!(p.project_ref, "abc");
    }

    #[test]
    fn resolve_project_returns_none_for_unknown() {
        let config = ServerConfig {
            projects: vec![
                ProjectConfig { name: "staging".into(), project_ref: "abc".into() },
            ],
        };
        assert!(config.resolve_project("unknown").is_none());
    }

    #[test]
    fn hash_is_deterministic() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[[projects]]
name = "staging"
ref = "abcdefghijklmnop"
"#
        )
        .unwrap();

        let (_, hash1) = load_config(f.path().to_str().unwrap()).unwrap();
        let (_, hash2) = load_config(f.path().to_str().unwrap()).unwrap();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn hash_changes_when_content_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        std::fs::write(
            &path,
            r#"
[[projects]]
name = "staging"
ref = "abc"
"#,
        )
        .unwrap();
        let (_, hash1) = load_config(path.to_str().unwrap()).unwrap();

        std::fs::write(
            &path,
            r#"
[[projects]]
name = "production"
ref = "xyz"
"#,
        )
        .unwrap();
        let (_, hash2) = load_config(path.to_str().unwrap()).unwrap();

        assert_ne!(hash1, hash2);
    }
}
