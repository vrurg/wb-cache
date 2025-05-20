#[cfg(feature = "pg")]
pub mod pg;
#[cfg(feature = "sqlite")]
pub mod sqlite;

use std::fmt::Debug;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;

use crate::test::types::Result;

#[async_trait]
pub trait DatabaseDriver: Debug + Sync + Send + 'static {
    fn connection(&self) -> DatabaseConnection;
    async fn configure(&self) -> Result<()>;
    async fn checkpoint(&self) -> Result<()>;
}
