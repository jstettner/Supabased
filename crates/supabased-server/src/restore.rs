use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tonic::Status;

use crate::config::ProjectDatabaseConnection;

const TRUNCATE_PUBLIC_SQL: &str = r#"
DO $$
DECLARE
    r record;
BEGIN
    FOR r IN
        SELECT quote_ident(schemaname) || '.' || quote_ident(tablename) AS full_name
        FROM pg_tables
        WHERE schemaname = 'public'
    LOOP
        EXECUTE 'TRUNCATE TABLE ' || r.full_name || ' RESTART IDENTITY CASCADE';
    END LOOP;
END $$;
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

impl CommandSpec {
    fn new(
        program: impl Into<OsString>,
        args: Vec<OsString>,
        env: Vec<(OsString, OsString)>,
    ) -> Self {
        Self {
            program: program.into(),
            args,
            env,
        }
    }
}

pub trait ProcessRunner {
    fn run(&self, command: &CommandSpec) -> Result<(), Status>;
}

pub struct SystemProcessRunner;

impl ProcessRunner for SystemProcessRunner {
    fn run(&self, command: &CommandSpec) -> Result<(), Status> {
        let output = Command::new(&command.program)
            .args(&command.args)
            .envs(command.env.iter().cloned())
            .output()
            .map_err(|e| {
                Status::internal(format!(
                    "failed to run {}: {e}",
                    command.program.to_string_lossy()
                ))
            })?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            format!("exit status {}", output.status)
        } else {
            format!("exit status {}: {stderr}", output.status)
        };
        Err(Status::internal(format!(
            "{} failed with {detail}",
            command.program.to_string_lossy()
        )))
    }
}

pub fn restore_public_schema(
    source: &ProjectDatabaseConnection,
    target: &ProjectDatabaseConnection,
) -> Result<(), Status> {
    restore_public_schema_with_runner(source, target, &SystemProcessRunner)
}

pub fn restore_public_schema_with_runner(
    source: &ProjectDatabaseConnection,
    target: &ProjectDatabaseConnection,
    runner: &impl ProcessRunner,
) -> Result<(), Status> {
    let tempdir = tempfile::tempdir()
        .map_err(|e| Status::internal(format!("failed to create restore workspace: {e}")))?;
    let dump_path = tempdir.path().join("public.dump");
    let restore_sql_path = tempdir.path().join("public-data.sql");
    let truncate_sql_path = tempdir.path().join("truncate-public.sql");
    let source_passfile_path = tempdir.path().join("source.pgpass");
    let target_passfile_path = tempdir.path().join("target.pgpass");

    std::fs::write(&truncate_sql_path, TRUNCATE_PUBLIC_SQL)
        .map_err(|e| Status::internal(format!("failed to write restore SQL: {e}")))?;
    write_pgpass_file(&source_passfile_path, source)
        .map_err(|e| Status::internal(format!("failed to write source passfile: {e}")))?;
    write_pgpass_file(&target_passfile_path, target)
        .map_err(|e| Status::internal(format!("failed to write target passfile: {e}")))?;

    let commands = restore_commands(
        source,
        target,
        &source_passfile_path,
        &target_passfile_path,
        &dump_path,
        &restore_sql_path,
        &truncate_sql_path,
    );
    for command in commands {
        runner.run(&command)?;
    }

    Ok(())
}

pub fn restore_commands(
    source: &ProjectDatabaseConnection,
    target: &ProjectDatabaseConnection,
    source_passfile_path: &Path,
    target_passfile_path: &Path,
    dump_path: &Path,
    restore_sql_path: &Path,
    truncate_sql_path: &Path,
) -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "pg_dump",
            vec![
                "--format=custom".into(),
                "--data-only".into(),
                "--schema=public".into(),
                "--no-owner".into(),
                "--no-privileges".into(),
                "--file".into(),
                dump_path.as_os_str().to_os_string(),
            ],
            connection_env(source, source_passfile_path),
        ),
        CommandSpec::new(
            "pg_restore",
            vec![
                "--data-only".into(),
                "--schema=public".into(),
                "--no-owner".into(),
                "--no-privileges".into(),
                "--file".into(),
                restore_sql_path.as_os_str().to_os_string(),
                dump_path.as_os_str().to_os_string(),
            ],
            Vec::new(),
        ),
        CommandSpec::new(
            "psql",
            vec![
                "-v".into(),
                "ON_ERROR_STOP=1".into(),
                "--single-transaction".into(),
                "-f".into(),
                truncate_sql_path.as_os_str().to_os_string(),
                "-f".into(),
                restore_sql_path.as_os_str().to_os_string(),
            ],
            connection_env(target, target_passfile_path),
        ),
    ]
}

fn connection_env(
    connection: &ProjectDatabaseConnection,
    passfile_path: &Path,
) -> Vec<(OsString, OsString)> {
    vec![
        ("PGHOST".into(), connection.host.clone().into()),
        ("PGPORT".into(), connection.port.to_string().into()),
        ("PGDATABASE".into(), connection.database.clone().into()),
        ("PGUSER".into(), connection.user.clone().into()),
        (
            "PGPASSFILE".into(),
            passfile_path.as_os_str().to_os_string(),
        ),
    ]
}

fn write_pgpass_file(
    path: &Path,
    connection: &ProjectDatabaseConnection,
) -> Result<(), std::io::Error> {
    let contents = format!(
        "{}:{}:{}:{}:{}\n",
        escape_pgpass_field(&connection.host),
        connection.port,
        escape_pgpass_field(&connection.database),
        escape_pgpass_field(&connection.user),
        escape_pgpass_field(&connection.password)
    );
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn escape_pgpass_field(value: &str) -> String {
    value.replace('\\', "\\\\").replace(':', "\\:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockRunner {
        commands: Mutex<Vec<CommandSpec>>,
        fail_at: Option<usize>,
    }

    impl ProcessRunner for MockRunner {
        fn run(&self, command: &CommandSpec) -> Result<(), Status> {
            let mut commands = self.commands.lock().unwrap();
            if self.fail_at == Some(commands.len()) {
                return Err(Status::internal("mock process failed"));
            }
            commands.push(command.clone());
            Ok(())
        }
    }

    fn conn(host: &str, password: &str) -> ProjectDatabaseConnection {
        ProjectDatabaseConnection {
            host: host.into(),
            port: 5432,
            database: "postgres".into(),
            user: "postgres".into(),
            password: password.into(),
        }
    }

    fn strings(command: &CommandSpec) -> (String, Vec<String>, Vec<(String, String)>) {
        (
            command.program.to_string_lossy().into_owned(),
            command
                .args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            command
                .env
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn restore_command_construction_uses_public_schema_only() {
        let commands = restore_commands(
            &conn("db.source.supabase.co", "source-secret"),
            &conn("db.target.supabase.co", "target-secret"),
            Path::new("/tmp/source.pgpass"),
            Path::new("/tmp/target.pgpass"),
            Path::new("/tmp/public.dump"),
            Path::new("/tmp/public-data.sql"),
            Path::new("/tmp/truncate-public.sql"),
        );

        assert_eq!(commands.len(), 3);
        let (program, args, env) = strings(&commands[0]);
        assert_eq!(program, "pg_dump");
        assert!(args.contains(&"--format=custom".to_string()));
        assert!(args.contains(&"--data-only".to_string()));
        assert!(args.contains(&"--schema=public".to_string()));
        assert!(!args.iter().any(|arg| arg.contains("source-secret")));
        assert!(env.contains(&("PGHOST".to_string(), "db.source.supabase.co".to_string())));
        assert!(env.contains(&("PGPASSFILE".to_string(), "/tmp/source.pgpass".to_string())));
        assert!(!env.iter().any(|(_, value)| value.contains("source-secret")));

        let (program, args, env) = strings(&commands[1]);
        assert_eq!(program, "pg_restore");
        assert!(args.contains(&"--schema=public".to_string()));
        assert!(args.contains(&"/tmp/public.dump".to_string()));
        assert!(env.is_empty());

        let (program, args, env) = strings(&commands[2]);
        assert_eq!(program, "psql");
        assert!(!args.iter().any(|arg| arg.contains("target-secret")));
        assert!(args.contains(&"--single-transaction".to_string()));
        assert!(args.contains(&"ON_ERROR_STOP=1".to_string()));
        assert!(env.contains(&("PGHOST".to_string(), "db.target.supabase.co".to_string())));
        assert!(env.contains(&("PGPASSFILE".to_string(), "/tmp/target.pgpass".to_string())));
        assert!(!env.iter().any(|(_, value)| value.contains("target-secret")));
    }

    #[test]
    fn restore_runner_stops_after_process_failure() {
        let runner = MockRunner {
            commands: Mutex::new(Vec::new()),
            fail_at: Some(1),
        };

        let result = restore_public_schema_with_runner(
            &conn("db.source.supabase.co", "source-secret"),
            &conn("db.target.supabase.co", "target-secret"),
            &runner,
        );
        assert!(result.is_err());
        assert_eq!(runner.commands.lock().unwrap().len(), 1);
    }

    #[test]
    fn pgpass_fields_are_escaped() {
        assert_eq!(escape_pgpass_field(r"a:b\c"), r"a\:b\\c");
    }
}
