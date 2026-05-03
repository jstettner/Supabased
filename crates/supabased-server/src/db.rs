use rusqlite_migration::{M, Migrations};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tokio_rusqlite::rusqlite;
use tokio_rusqlite::{Connection, Error as TokioRusqliteError, OptionalExtension};

const MIGRATIONS: &[M<'static>] = &[
    M::up(
        "CREATE TABLE jwt_secrets (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            secret BLOB NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    ),
    M::up(
        "CREATE TABLE branches (
            branch_name TEXT NOT NULL,
            project_name TEXT NOT NULL,
            creator_identity TEXT NOT NULL,
            branch_ref TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (branch_name, project_name)
        );",
    ),
];

pub async fn init_db(path: &str) -> Result<Connection, Box<dyn std::error::Error>> {
    harden_db_file_permissions(path)?;
    let conn = Connection::open(path).await?;
    harden_db_family_permissions(path)?;

    conn.call(|conn| -> Result<(), rusqlite::Error> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    })
    .await?;

    conn.call(|conn| -> Result<(), rusqlite::Error> {
        let migrations = Migrations::new(MIGRATIONS.to_vec());
        migrations
            .to_latest(conn)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok(())
    })
    .await?;

    harden_db_family_permissions(path)?;

    Ok(conn)
}

fn harden_db_family_permissions(path: &str) -> Result<(), std::io::Error> {
    if path == ":memory:" {
        return Ok(());
    }

    harden_db_file_permissions(path)?;
    harden_db_file_permissions(&format!("{path}-wal"))?;
    harden_db_file_permissions(&format!("{path}-shm"))?;

    Ok(())
}

fn harden_db_file_permissions(path: &str) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    if std::path::Path::new(path).exists() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

pub async fn ensure_jwt_secret(conn: &Connection) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let secret = conn
        .call(|conn| -> Result<Vec<u8>, rusqlite::Error> {
            let existing: Option<Vec<u8>> = conn
                .query_row("SELECT secret FROM jwt_secrets WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .optional()?;

            if let Some(secret) = existing {
                return Ok(secret);
            }

            let mut secret = vec![0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, secret.as_mut_slice());
            conn.execute(
                "INSERT INTO jwt_secrets (id, secret) VALUES (1, ?1)",
                [&secret],
            )?;

            Ok(secret)
        })
        .await?;

    Ok(secret)
}

#[allow(dead_code)]
pub struct BranchRecord {
    pub branch_name: String,
    pub project_name: String,
    pub creator_identity: String,
    pub branch_ref: String,
    pub created_at: String,
}

pub async fn record_branch(
    conn: &Connection,
    branch_name: &str,
    project_name: &str,
    creator: &str,
    branch_ref: &str,
) -> Result<(), TokioRusqliteError> {
    let branch_name = branch_name.to_string();
    let project_name = project_name.to_string();
    let creator = creator.to_string();
    let branch_ref = branch_ref.to_string();
    conn.call(move |conn| -> Result<(), rusqlite::Error> {
        conn.execute(
            "INSERT INTO branches (branch_name, project_name, creator_identity, branch_ref) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![branch_name, project_name, creator, branch_ref],
        )?;
        Ok(())
    })
    .await
}

pub async fn get_branch(
    conn: &Connection,
    branch_name: &str,
    project_name: &str,
) -> Result<Option<BranchRecord>, TokioRusqliteError> {
    let branch_name = branch_name.to_string();
    let project_name = project_name.to_string();
    conn.call(move |conn| -> Result<Option<BranchRecord>, rusqlite::Error> {
        let result = conn
            .query_row(
                "SELECT branch_name, project_name, creator_identity, branch_ref, created_at FROM branches WHERE branch_name = ?1 AND project_name = ?2",
                rusqlite::params![branch_name, project_name],
                |row| {
                    Ok(BranchRecord {
                        branch_name: row.get(0)?,
                        project_name: row.get(1)?,
                        creator_identity: row.get(2)?,
                        branch_ref: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(result)
    })
    .await
}

#[allow(dead_code)]
pub async fn list_branches_by_project(
    conn: &Connection,
    project_name: &str,
) -> Result<Vec<BranchRecord>, TokioRusqliteError> {
    let project_name = project_name.to_string();
    conn.call(move |conn| -> Result<Vec<BranchRecord>, rusqlite::Error> {
        let mut stmt = conn.prepare(
            "SELECT branch_name, project_name, creator_identity, branch_ref, created_at FROM branches WHERE project_name = ?1 ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![project_name], |row| {
                Ok(BranchRecord {
                    branch_name: row.get(0)?,
                    project_name: row.get(1)?,
                    creator_identity: row.get(2)?,
                    branch_ref: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

#[allow(dead_code)]
pub async fn list_all_branches(conn: &Connection) -> Result<Vec<BranchRecord>, TokioRusqliteError> {
    conn.call(|conn| -> Result<Vec<BranchRecord>, rusqlite::Error> {
        let mut stmt = conn.prepare(
            "SELECT branch_name, project_name, creator_identity, branch_ref, created_at FROM branches ORDER BY project_name, created_at",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(BranchRecord {
                    branch_name: row.get(0)?,
                    project_name: row.get(1)?,
                    creator_identity: row.get(2)?,
                    branch_ref: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

pub async fn delete_branch(
    conn: &Connection,
    branch_name: &str,
    project_name: &str,
) -> Result<bool, TokioRusqliteError> {
    let branch_name = branch_name.to_string();
    let project_name = project_name.to_string();
    conn.call(move |conn| -> Result<bool, rusqlite::Error> {
        let rows = conn.execute(
            "DELETE FROM branches WHERE branch_name = ?1 AND project_name = ?2",
            rusqlite::params![branch_name, project_name],
        )?;
        Ok(rows > 0)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_conn() -> Connection {
        init_db(":memory:").await.unwrap()
    }

    #[tokio::test]
    async fn record_and_get_branch() {
        let conn = test_conn().await;
        record_branch(&conn, "my-branch", "staging", "github:alice", "ref-abc")
            .await
            .unwrap();
        let branch = get_branch(&conn, "my-branch", "staging")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(branch.branch_name, "my-branch");
        assert_eq!(branch.project_name, "staging");
        assert_eq!(branch.creator_identity, "github:alice");
        assert_eq!(branch.branch_ref, "ref-abc");
    }

    #[tokio::test]
    async fn get_branch_returns_none_for_unknown() {
        let conn = test_conn().await;
        let result = get_branch(&conn, "nonexistent", "staging").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn list_branches_returns_correct_subset() {
        let conn = test_conn().await;
        record_branch(&conn, "b1", "staging", "github:alice", "ref-1")
            .await
            .unwrap();
        record_branch(&conn, "b2", "staging", "github:bob", "ref-2")
            .await
            .unwrap();
        record_branch(&conn, "b3", "production", "github:alice", "ref-3")
            .await
            .unwrap();

        let staging = list_branches_by_project(&conn, "staging").await.unwrap();
        assert_eq!(staging.len(), 2);

        let prod = list_branches_by_project(&conn, "production").await.unwrap();
        assert_eq!(prod.len(), 1);
        assert_eq!(prod[0].branch_name, "b3");
    }

    #[tokio::test]
    async fn delete_branch_removes_row() {
        let conn = test_conn().await;
        record_branch(&conn, "b1", "staging", "github:alice", "ref-1")
            .await
            .unwrap();
        let deleted = delete_branch(&conn, "b1", "staging").await.unwrap();
        assert!(deleted);
        let result = get_branch(&conn, "b1", "staging").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn delete_branch_returns_false_for_nonexistent() {
        let conn = test_conn().await;
        let deleted = delete_branch(&conn, "nonexistent", "staging")
            .await
            .unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn same_branch_name_different_projects() {
        let conn = test_conn().await;
        record_branch(&conn, "feature", "staging", "github:alice", "ref-1")
            .await
            .unwrap();
        record_branch(&conn, "feature", "production", "github:alice", "ref-2")
            .await
            .unwrap();

        let s = get_branch(&conn, "feature", "staging")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.branch_ref, "ref-1");
        let p = get_branch(&conn, "feature", "production")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(p.branch_ref, "ref-2");
    }

    #[tokio::test]
    async fn duplicate_branch_same_project_fails() {
        let conn = test_conn().await;
        record_branch(&conn, "feature", "staging", "github:alice", "ref-1")
            .await
            .unwrap();
        let result = record_branch(&conn, "feature", "staging", "github:bob", "ref-2").await;
        assert!(result.is_err());
    }
}
