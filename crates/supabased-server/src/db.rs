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
    M::up(
        "CREATE TABLE refresh_sessions (
            selector TEXT PRIMARY KEY,
            token_hash BLOB NOT NULL,
            identity TEXT NOT NULL,
            permissions_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            last_used_at INTEGER,
            revoked_at INTEGER
        );",
    ),
    M::up(
        "CREATE TABLE demo_states (
            project_name TEXT NOT NULL,
            name TEXT NOT NULL,
            branch_name TEXT NOT NULL,
            branch_ref TEXT NOT NULL,
            creator_identity TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_restored_at TEXT,
            PRIMARY KEY (project_name, name)
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

#[allow(dead_code)]
#[derive(Debug)]
pub struct DemoStateRecord {
    pub project_name: String,
    pub name: String,
    pub branch_name: String,
    pub branch_ref: String,
    pub creator_identity: String,
    pub created_at: String,
    pub last_restored_at: Option<String>,
}

pub async fn record_demo_state(
    conn: &Connection,
    project_name: &str,
    name: &str,
    branch_name: &str,
    branch_ref: &str,
    creator: &str,
) -> Result<(), TokioRusqliteError> {
    let project_name = project_name.to_string();
    let name = name.to_string();
    let branch_name = branch_name.to_string();
    let branch_ref = branch_ref.to_string();
    let creator = creator.to_string();
    conn.call(move |conn| -> Result<(), rusqlite::Error> {
        conn.execute(
            "INSERT INTO demo_states
                (project_name, name, branch_name, branch_ref, creator_identity)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![project_name, name, branch_name, branch_ref, creator],
        )?;
        Ok(())
    })
    .await
}

pub async fn get_demo_state(
    conn: &Connection,
    project_name: &str,
    name: &str,
) -> Result<Option<DemoStateRecord>, TokioRusqliteError> {
    let project_name = project_name.to_string();
    let name = name.to_string();
    conn.call(
        move |conn| -> Result<Option<DemoStateRecord>, rusqlite::Error> {
            conn.query_row(
                "SELECT project_name, name, branch_name, branch_ref, creator_identity, created_at, last_restored_at
                 FROM demo_states
                 WHERE project_name = ?1 AND name = ?2",
                rusqlite::params![project_name, name],
                demo_state_from_row,
            )
            .optional()
        },
    )
    .await
}

pub async fn list_demo_states_by_project(
    conn: &Connection,
    project_name: &str,
) -> Result<Vec<DemoStateRecord>, TokioRusqliteError> {
    let project_name = project_name.to_string();
    conn.call(move |conn| -> Result<Vec<DemoStateRecord>, rusqlite::Error> {
        let mut stmt = conn.prepare(
            "SELECT project_name, name, branch_name, branch_ref, creator_identity, created_at, last_restored_at
             FROM demo_states
             WHERE project_name = ?1
             ORDER BY created_at, name",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![project_name], demo_state_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

pub async fn delete_demo_state(
    conn: &Connection,
    project_name: &str,
    name: &str,
) -> Result<bool, TokioRusqliteError> {
    let project_name = project_name.to_string();
    let name = name.to_string();
    conn.call(move |conn| -> Result<bool, rusqlite::Error> {
        let rows = conn.execute(
            "DELETE FROM demo_states WHERE project_name = ?1 AND name = ?2",
            rusqlite::params![project_name, name],
        )?;
        Ok(rows > 0)
    })
    .await
}

pub async fn mark_demo_state_restored(
    conn: &Connection,
    project_name: &str,
    name: &str,
) -> Result<Option<DemoStateRecord>, TokioRusqliteError> {
    let project_name = project_name.to_string();
    let name = name.to_string();
    conn.call(
        move |conn| -> Result<Option<DemoStateRecord>, rusqlite::Error> {
            let rows = conn.execute(
                "UPDATE demo_states
                 SET last_restored_at = datetime('now')
                 WHERE project_name = ?1 AND name = ?2",
                rusqlite::params![project_name, name],
            )?;
            if rows == 0 {
                return Ok(None);
            }
            conn.query_row(
                "SELECT project_name, name, branch_name, branch_ref, creator_identity, created_at, last_restored_at
                 FROM demo_states
                 WHERE project_name = ?1 AND name = ?2",
                rusqlite::params![project_name, name],
                demo_state_from_row,
            )
            .optional()
        },
    )
    .await
}

fn demo_state_from_row(row: &rusqlite::Row<'_>) -> Result<DemoStateRecord, rusqlite::Error> {
    Ok(DemoStateRecord {
        project_name: row.get(0)?,
        name: row.get(1)?,
        branch_name: row.get(2)?,
        branch_ref: row.get(3)?,
        creator_identity: row.get(4)?,
        created_at: row.get(5)?,
        last_restored_at: row.get(6)?,
    })
}

#[derive(Debug)]
pub struct RefreshSessionRecord {
    pub selector: String,
    pub token_hash: Vec<u8>,
    pub identity: String,
    pub permissions_json: String,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
}

pub async fn insert_refresh_session(
    conn: &Connection,
    selector: &str,
    token_hash: &[u8],
    identity: &str,
    permissions: &[String],
    now: i64,
    expires_at: i64,
) -> Result<(), TokioRusqliteError> {
    let selector = selector.to_string();
    let token_hash = token_hash.to_vec();
    let identity = identity.to_string();
    let permissions_json =
        serde_json::to_string(permissions).expect("serializing refresh permissions cannot fail");

    conn.call(move |conn| -> Result<(), rusqlite::Error> {
        conn.execute(
            "INSERT INTO refresh_sessions
                (selector, token_hash, identity, permissions_json, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                selector,
                token_hash,
                identity,
                permissions_json,
                now,
                expires_at
            ],
        )?;
        Ok(())
    })
    .await
}

pub async fn get_refresh_session(
    conn: &Connection,
    selector: &str,
) -> Result<Option<RefreshSessionRecord>, TokioRusqliteError> {
    let selector = selector.to_string();
    conn.call(
        move |conn| -> Result<Option<RefreshSessionRecord>, rusqlite::Error> {
            conn.query_row(
                "SELECT selector, token_hash, identity, permissions_json, expires_at, revoked_at
                 FROM refresh_sessions WHERE selector = ?1",
                rusqlite::params![selector],
                |row| {
                    Ok(RefreshSessionRecord {
                        selector: row.get(0)?,
                        token_hash: row.get(1)?,
                        identity: row.get(2)?,
                        permissions_json: row.get(3)?,
                        expires_at: row.get(4)?,
                        revoked_at: row.get(5)?,
                    })
                },
            )
            .optional()
        },
    )
    .await
}

pub async fn rotate_refresh_session(
    conn: &Connection,
    old_selector: &str,
    new_selector: &str,
    new_token_hash: &[u8],
    identity: &str,
    permissions: &[String],
    now: i64,
    expires_at: i64,
) -> Result<(), TokioRusqliteError> {
    let old_selector = old_selector.to_string();
    let new_selector = new_selector.to_string();
    let new_token_hash = new_token_hash.to_vec();
    let identity = identity.to_string();
    let permissions_json =
        serde_json::to_string(permissions).expect("serializing refresh permissions cannot fail");

    conn.call(move |conn| -> Result<(), rusqlite::Error> {
        let tx = conn.transaction()?;
        let rows = tx.execute(
            "UPDATE refresh_sessions
             SET revoked_at = ?1, last_used_at = ?1
             WHERE selector = ?2 AND revoked_at IS NULL",
            rusqlite::params![now, old_selector],
        )?;
        if rows == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "INSERT INTO refresh_sessions
                (selector, token_hash, identity, permissions_json, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                new_selector,
                new_token_hash,
                identity,
                permissions_json,
                now,
                expires_at
            ],
        )?;
        tx.commit()?;
        Ok(())
    })
    .await
}

pub async fn revoke_refresh_session(
    conn: &Connection,
    selector: &str,
    now: i64,
) -> Result<bool, TokioRusqliteError> {
    let selector = selector.to_string();
    conn.call(move |conn| -> Result<bool, rusqlite::Error> {
        let rows = conn.execute(
            "UPDATE refresh_sessions
             SET revoked_at = ?1
             WHERE selector = ?2 AND revoked_at IS NULL",
            rusqlite::params![now, selector],
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

    #[tokio::test]
    async fn record_get_and_list_demo_states() {
        let conn = test_conn().await;
        record_demo_state(
            &conn,
            "staging",
            "happy path",
            "demo/happy-path",
            "branch-ref",
            "github:alice",
        )
        .await
        .unwrap();

        let state = get_demo_state(&conn, "staging", "happy path")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.project_name, "staging");
        assert_eq!(state.name, "happy path");
        assert_eq!(state.branch_name, "demo/happy-path");
        assert_eq!(state.branch_ref, "branch-ref");
        assert_eq!(state.creator_identity, "github:alice");
        assert_eq!(state.last_restored_at, None);

        let states = list_demo_states_by_project(&conn, "staging").await.unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].name, "happy path");
    }

    #[tokio::test]
    async fn duplicate_demo_state_same_project_fails() {
        let conn = test_conn().await;
        record_demo_state(
            &conn,
            "staging",
            "happy path",
            "demo/happy-path",
            "branch-ref",
            "github:alice",
        )
        .await
        .unwrap();

        let result = record_demo_state(
            &conn,
            "staging",
            "happy path",
            "demo/happy-path-2",
            "branch-ref-2",
            "github:bob",
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_demo_state_removes_row() {
        let conn = test_conn().await;
        record_demo_state(
            &conn,
            "staging",
            "happy path",
            "demo/happy-path",
            "branch-ref",
            "github:alice",
        )
        .await
        .unwrap();

        let deleted = delete_demo_state(&conn, "staging", "happy path")
            .await
            .unwrap();
        assert!(deleted);

        let result = get_demo_state(&conn, "staging", "happy path")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn delete_demo_state_returns_false_for_unknown() {
        let conn = test_conn().await;
        let deleted = delete_demo_state(&conn, "staging", "missing")
            .await
            .unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn mark_demo_state_restored_updates_timestamp() {
        let conn = test_conn().await;
        record_demo_state(
            &conn,
            "staging",
            "happy path",
            "demo/happy-path",
            "branch-ref",
            "github:alice",
        )
        .await
        .unwrap();

        let state = mark_demo_state_restored(&conn, "staging", "happy path")
            .await
            .unwrap()
            .unwrap();
        assert!(state.last_restored_at.is_some());

        let missing = mark_demo_state_restored(&conn, "staging", "missing")
            .await
            .unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn refresh_session_stores_hash_only() {
        let conn = test_conn().await;
        let permissions = vec!["branches.list".to_string()];
        insert_refresh_session(
            &conn,
            "selector",
            b"hashed-secret",
            "github:alice",
            &permissions,
            10,
            20,
        )
        .await
        .unwrap();

        let session = get_refresh_session(&conn, "selector")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.selector, "selector");
        assert_eq!(session.token_hash, b"hashed-secret");
        assert_eq!(session.identity, "github:alice");
        assert_eq!(session.permissions_json, r#"["branches.list"]"#);
        assert_eq!(session.expires_at, 20);
        assert_eq!(session.revoked_at, None);
    }

    #[tokio::test]
    async fn rotate_refresh_session_revokes_old_and_inserts_new() {
        let conn = test_conn().await;
        let permissions = vec!["branches.list".to_string()];
        insert_refresh_session(
            &conn,
            "old",
            b"old-hash",
            "github:alice",
            &permissions,
            10,
            20,
        )
        .await
        .unwrap();

        rotate_refresh_session(
            &conn,
            "old",
            "new",
            b"new-hash",
            "github:alice",
            &permissions,
            15,
            45,
        )
        .await
        .unwrap();

        let old = get_refresh_session(&conn, "old").await.unwrap().unwrap();
        let new = get_refresh_session(&conn, "new").await.unwrap().unwrap();
        assert_eq!(old.revoked_at, Some(15));
        assert_eq!(new.revoked_at, None);
        assert_eq!(new.token_hash, b"new-hash");
        assert_eq!(new.expires_at, 45);
    }

    #[tokio::test]
    async fn revoke_refresh_session_marks_active_session() {
        let conn = test_conn().await;
        let permissions = vec!["branches.list".to_string()];
        insert_refresh_session(
            &conn,
            "selector",
            b"hash",
            "github:alice",
            &permissions,
            10,
            20,
        )
        .await
        .unwrap();

        assert!(revoke_refresh_session(&conn, "selector", 18).await.unwrap());
        assert!(!revoke_refresh_session(&conn, "selector", 19).await.unwrap());

        let session = get_refresh_session(&conn, "selector")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.revoked_at, Some(18));
    }
}
