use std::time::Duration;

use async_trait::async_trait;
use fieldx::fxstruct;
use sea_orm::ConnectOptions;
use sea_orm::ConnectionTrait;
use sea_orm::DatabaseConnection;

use crate::test::types::Result;

use super::DatabaseDriver;

#[derive(Debug)]
#[fxstruct(sync, rc, no_new, builder)]
pub struct Pg {
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
    #[fieldx(inner_mut, get(off), set, builder(off))]
    connection: DatabaseConnection,
}

impl Pg {
    pub async fn connect(&self) -> Result<()> {
        let schema = format!(
            "postgres://{}:{}@{}:{}/{}",
            self.user, self.password, self.host, self.port, self.database
        );
        let mut opts = ConnectOptions::new(&schema);
        opts.max_connections(20)
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(20))
            .max_lifetime(Duration::from_secs(60))
            .test_before_acquire(true);

        self.set_connection(
            sea_orm::Database::connect(opts)
                .await
                .inspect_err(|e| eprintln!("Error connecting to database {schema}: {e}"))?,
        );

        Ok(())
    }
}

#[async_trait]
impl DatabaseDriver for Pg {
    fn connection(&self) -> DatabaseConnection {
        self.connection.read().clone()
    }

    async fn configure(&self) -> Result<()> {
        self.connection()
            .execute_unprepared("SET synchronous_commit = off;")
            .await?;

        Ok(())
    }

    async fn checkpoint(&self) -> Result<()> {
        Ok(())
    }
}
