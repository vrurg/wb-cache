//! SQLite database driver.
use std::borrow::Borrow;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use fieldx::fxstruct;
use sea_orm::ConnectionTrait;
use sea_orm::DatabaseConnection;
use sea_orm_migration::IntoSchemaManagerConnection;
use sea_orm_migration::SchemaManagerConnection;

use crate::test::simulation::types::Result;

use super::DatabaseDriver;

/// SQLite database driver.
#[derive(Debug)]
#[fxstruct(sync, no_new)]
pub struct Sqlite {
    connection: DatabaseConnection,
}

impl Sqlite {
    pub async fn connect(db_dir: &Path, db_name: &str) -> Result<Arc<Self>> {
        let db_path = db_dir.join(db_name);

        let schema = format!("sqlite://{}?mode=rwc", db_path.display());
        let db = sea_orm::Database::connect(&schema)
            .await
            .inspect_err(|e| eprintln!("Error connecting to database {schema}: {e}"))?;

        Ok(Arc::new(Self { connection: db }))
    }
}

#[async_trait]
impl DatabaseDriver for Sqlite {
    fn name(&self) -> &'static str {
        "SQLite"
    }

    fn connection(&self) -> DatabaseConnection {
        self.connection.clone()
    }

    /// Set the following SQLite pragmas for performance:
    ///
    /// - `journal_mode=WAL`
    /// - `cache=64000`
    /// - `synchronous=NORMAL`
    async fn configure(&self) -> Result<()> {
        let db = &self.connection;

        db.execute_unprepared("PRAGMA journal_mode=WAL;").await?;
        db.execute_unprepared("PRAGMA cache=64000;").await?;
        db.execute_unprepared("PRAGMA synchronous=NORMAL;").await?;

        Ok(())
    }

    /// Execute `wal_checkpoint` to checkpoint the WAL (Write-Ahead Logging) journal.
    async fn checkpoint(&self) -> Result<()> {
        self.connection.execute_unprepared("PRAGMA wal_checkpoint;").await?;

        Ok(())
    }
}

impl Deref for Sqlite {
    type Target = DatabaseConnection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl AsRef<DatabaseConnection> for Sqlite {
    fn as_ref(&self) -> &DatabaseConnection {
        &self.connection
    }
}

impl Borrow<DatabaseConnection> for Sqlite {
    fn borrow(&self) -> &DatabaseConnection {
        &self.connection
    }
}

impl<'c> IntoSchemaManagerConnection<'c> for &'c Sqlite {
    fn into_schema_manager_connection(self) -> SchemaManagerConnection<'c> {
        SchemaManagerConnection::Connection(&self.connection)
    }
}
