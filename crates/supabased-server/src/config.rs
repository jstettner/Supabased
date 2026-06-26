use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub projects: Vec<ProjectConfig>,
    pub database: Option<DatabaseConnectionConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConnectionConfig {
    pub host_template: String,
    #[serde(default = "default_database_name")]
    pub name: String,
    #[serde(default = "default_database_user")]
    pub user: String,
    #[serde(default = "default_database_port")]
    pub port: u16,
    pub password_env: String,
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
}

impl ServerConfig {
    pub fn resolve_project(&self, name: &str) -> Option<&ProjectConfig> {
        self.projects.iter().find(|p| p.name == name)
    }

    pub fn database_connection_for_project_ref(
        &self,
        project_ref: &str,
    ) -> Result<ProjectDatabaseConnection, String> {
        let config = self.database.as_ref().ok_or_else(|| {
            "[database] config is required for demo restore operations".to_string()
        })?;
        let password = std::env::var(&config.password_env).map_err(|_| {
            format!(
                "{} environment variable is required for demo restore operations",
                config.password_env
            )
        })?;

        Ok(config.connection_for_project_ref(project_ref, password))
    }
}

impl DatabaseConnectionConfig {
    pub fn connection_for_project_ref(
        &self,
        project_ref: &str,
        password: String,
    ) -> ProjectDatabaseConnection {
        ProjectDatabaseConnection {
            host: self.host_template.replace("{project_ref}", project_ref),
            port: self.port,
            database: self.name.clone(),
            user: self.user.clone(),
            password,
        }
    }
}

fn default_database_name() -> String {
    "postgres".to_string()
}

fn default_database_user() -> String {
    "postgres".to_string()
}

fn default_database_port() -> u16 {
    5432
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
    config.projects.iter().try_for_each(|p| {
        if p.name.is_empty() {
            Err(format!("project entry in {path} has empty name"))
        } else if p.project_ref.is_empty() {
            Err(format!("project '{}' in {path} has empty ref", p.name))
        } else {
            Ok(())
        }
    })?;
    if let Some(database) = &config.database {
        if database.host_template.is_empty() {
            return Err(format!("database.host_template in {path} is empty"));
        }
        if database.name.is_empty() {
            return Err(format!("database.name in {path} is empty"));
        }
        if database.user.is_empty() {
            return Err(format!("database.user in {path} is empty"));
        }
        if database.password_env.is_empty() {
            return Err(format!("database.password_env in {path} is empty"));
        }
    }
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
            database: None,
            projects: vec![
                ProjectConfig {
                    name: "staging".into(),
                    project_ref: "abc".into(),
                },
                ProjectConfig {
                    name: "prod".into(),
                    project_ref: "xyz".into(),
                },
            ],
        };
        let p = config.resolve_project("staging").unwrap();
        assert_eq!(p.project_ref, "abc");
    }

    #[test]
    fn resolve_project_returns_none_for_unknown() {
        let config = ServerConfig {
            database: None,
            projects: vec![ProjectConfig {
                name: "staging".into(),
                project_ref: "abc".into(),
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
    fn renders_database_connection_config() {
        let config = DatabaseConnectionConfig {
            host_template: "db.{project_ref}.supabase.co".into(),
            name: "postgres".into(),
            user: "postgres".into(),
            port: 5432,
            password_env: "SUPABASE_DB_PASSWORD".into(),
        };

        let conn = config.connection_for_project_ref("ref123", "secret".into());
        assert_eq!(conn.host, "db.ref123.supabase.co");
        assert_eq!(conn.port, 5432);
        assert_eq!(conn.database, "postgres");
        assert_eq!(conn.user, "postgres");
        assert_eq!(conn.password, "secret");
    }

    #[test]
    fn database_config_is_required_when_rendering() {
        let config = ServerConfig {
            database: None,
            projects: vec![ProjectConfig {
                name: "staging".into(),
                project_ref: "abc".into(),
            }],
        };

        assert!(
            config
                .database_connection_for_project_ref("ref123")
                .is_err()
        );
    }

    #[test]
    fn parses_database_config_with_defaults() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[database]
host_template = "db.{{project_ref}}.supabase.co"
password_env = "SUPABASE_DB_PASSWORD"

[[projects]]
name = "staging"
ref = "abcdefghijklmnop"
"#
        )
        .unwrap();

        let (config, _) = load_config(f.path().to_str().unwrap()).unwrap();
        let database = config.database.unwrap();
        assert_eq!(database.host_template, "db.{project_ref}.supabase.co");
        assert_eq!(database.name, "postgres");
        assert_eq!(database.user, "postgres");
        assert_eq!(database.port, 5432);
        assert_eq!(database.password_env, "SUPABASE_DB_PASSWORD");
    }
}
