use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub projects: Vec<ProjectConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDatabaseConnection {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    #[serde(rename = "ref")]
    pub project_ref: String,
    #[serde(default)]
    pub demo: bool,
    pub database_password_env: Option<String>,
}

impl ServerConfig {
    pub fn resolve_project(&self, name: &str) -> Option<&ProjectConfig> {
        self.projects.iter().find(|p| p.name == name)
    }
}

impl ProjectConfig {
    pub fn is_demo_project(&self) -> bool {
        self.demo
    }

    pub fn database_connection_for_ref(
        &self,
        project_ref: &str,
    ) -> Result<ProjectDatabaseConnection, String> {
        let password_env = self
            .database_password_env
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!(
                    "project '{}' is missing database_password_env for demo restore operations",
                    self.name
                )
            })?;
        let password = std::env::var(password_env).map_err(|_| {
            format!("{password_env} environment variable is required for demo restore operations")
        })?;

        Ok(ProjectDatabaseConnection {
            host: format!("db.{project_ref}.supabase.co"),
            port: 5432,
            database: "postgres".to_string(),
            user: "postgres".to_string(),
            password,
        })
    }

    fn validate(&self, path: &str) -> Result<(), String> {
        if self.name.is_empty() {
            Err(format!("project entry in {path} has empty name"))
        } else if self.project_ref.is_empty() {
            Err(format!("project '{}' in {path} has empty ref", self.name))
        } else if self.demo
            && self
                .database_password_env
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            Err(format!(
                "project '{}' in {path} has demo=true but missing database_password_env",
                self.name
            ))
        } else {
            Ok(())
        }
    }
}

pub fn load_config(path: &str) -> Result<(ServerConfig, String), String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read config file {path}: {e}"))?;

    use sha2::{Digest, Sha256};
    let config_hash = format!("{:x}", Sha256::digest(contents.as_bytes()));

    let config: ServerConfig = toml::from_str(&contents)
        .map_err(|e| format!("failed to parse config file {path}: {e}"))?;
    if config.projects.is_empty() {
        return Err(format!(
            "config file {path} must define at least one [[projects]] entry"
        ));
    }
    config.projects.iter().try_for_each(|p| p.validate(path))?;
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
                ProjectConfig {
                    name: "staging".into(),
                    project_ref: "abc".into(),
                    demo: false,
                    database_password_env: None,
                },
                ProjectConfig {
                    name: "prod".into(),
                    project_ref: "xyz".into(),
                    demo: false,
                    database_password_env: None,
                },
            ],
        };
        let p = config.resolve_project("staging").unwrap();
        assert_eq!(p.project_ref, "abc");
    }

    #[test]
    fn resolve_project_returns_none_for_unknown() {
        let config = ServerConfig {
            projects: vec![ProjectConfig {
                name: "staging".into(),
                project_ref: "abc".into(),
                demo: false,
                database_password_env: None,
            }],
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

    #[test]
    fn omitted_demo_defaults_to_false() {
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

        let (config, _) = load_config(f.path().to_str().unwrap()).unwrap();
        assert!(!config.projects[0].demo);
        assert!(config.projects[0].database_password_env.is_none());
    }

    #[test]
    fn demo_project_requires_database_password_env() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[[projects]]
name = "staging"
ref = "abcdefghijklmnop"
demo = true
"#
        )
        .unwrap();

        let err = load_config(f.path().to_str().unwrap()).unwrap_err();
        assert!(err.contains("demo=true"));
        assert!(err.contains("database_password_env"));
    }

    #[test]
    fn non_demo_project_does_not_require_database_password_env() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[[projects]]
name = "staging"
ref = "abcdefghijklmnop"
demo = false
"#
        )
        .unwrap();

        let (config, _) = load_config(f.path().to_str().unwrap()).unwrap();
        assert!(!config.projects[0].is_demo_project());
        assert!(config.projects[0].database_password_env.is_none());
    }

    #[test]
    fn parses_demo_project_database_password_env() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[[projects]]
name = "staging"
ref = "abcdefghijklmnop"
demo = true
database_password_env = "STAGING_DB_PASSWORD"
"#
        )
        .unwrap();

        let (config, _) = load_config(f.path().to_str().unwrap()).unwrap();
        assert!(config.projects[0].is_demo_project());
        assert_eq!(
            config.projects[0].database_password_env.as_deref(),
            Some("STAGING_DB_PASSWORD")
        );
    }

    #[test]
    fn project_database_connection_uses_supabase_conventions() {
        let env_name = "SUPABASED_TEST_DB_PASSWORD_CONNECTION";
        unsafe {
            std::env::set_var(env_name, "secret");
        }
        let config = ProjectConfig {
            name: "staging".into(),
            project_ref: "ref123".into(),
            demo: true,
            database_password_env: Some(env_name.into()),
        };

        let conn = config.database_connection_for_ref("branch456").unwrap();
        unsafe {
            std::env::remove_var(env_name);
        }
        assert_eq!(conn.host, "db.branch456.supabase.co");
        assert_eq!(conn.port, 5432);
        assert_eq!(conn.database, "postgres");
        assert_eq!(conn.user, "postgres");
        assert_eq!(conn.password, "secret");
    }

    #[test]
    fn project_ref_maps_to_supabase_database_host() {
        let env_name = "SUPABASED_TEST_DB_PASSWORD_PROJECT_REF";
        unsafe {
            std::env::set_var(env_name, "secret");
        }
        let config = ProjectConfig {
            name: "staging".into(),
            project_ref: "ref123".into(),
            demo: true,
            database_password_env: Some(env_name.into()),
        };

        let conn = config
            .database_connection_for_ref(&config.project_ref)
            .unwrap();
        unsafe {
            std::env::remove_var(env_name);
        }
        assert_eq!(conn.host, "db.ref123.supabase.co");
    }

    #[test]
    fn database_password_env_is_required_when_rendering() {
        let config = ProjectConfig {
            name: "staging".into(),
            project_ref: "abc".into(),
            demo: true,
            database_password_env: Some("SUPABASED_TEST_DB_PASSWORD_MISSING".into()),
        };

        let err = config.database_connection_for_ref("ref123").unwrap_err();
        assert!(err.contains("SUPABASED_TEST_DB_PASSWORD_MISSING"));
    }

    #[test]
    fn database_password_env_config_is_required_when_rendering() {
        let config = ProjectConfig {
            name: "staging".into(),
            project_ref: "abc".into(),
            demo: true,
            database_password_env: None,
        };

        let err = config.database_connection_for_ref("ref123").unwrap_err();
        assert!(err.contains("database_password_env"));
    }
}
