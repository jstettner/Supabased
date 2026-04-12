use tokio_rusqlite::{Connection, OptionalExtension};
use tokio_rusqlite::rusqlite;
use rusqlite_migration::{Migrations, M};

const MIGRATIONS: &[M<'static>] = &[M::up(
    "CREATE TABLE jwt_secrets (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            secret BLOB NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
)];

pub async fn init_db(path: &str) -> Result<Connection, Box<dyn std::error::Error>> {
    let conn = Connection::open(path).await?;

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

    Ok(conn)
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
            rand::Rng::fill(&mut rand::thread_rng(), secret.as_mut_slice());
            conn.execute(
                "INSERT INTO jwt_secrets (id, secret) VALUES (1, ?1)",
                [&secret],
            )?;

            Ok(secret)
        })
        .await?;

    Ok(secret)
}
