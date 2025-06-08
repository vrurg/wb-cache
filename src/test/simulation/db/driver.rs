//! Database drivers.
#[cfg(feature = "pg")]
pub mod pg;
#[cfg(feature = "sqlite")]
pub mod sqlite;

use std::fmt::Debug;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;

use crate::test::simulation::types::Result;

/// Trait for database [drivers](super::driver#modules) in the simulation environment.
#[async_trait]
pub trait DatabaseDriver: Debug + Sync + Send + 'static {
    /// Return driver name.
    fn name(&self) -> &'static str;
    /// Returns the database connection for the driver.
    fn connection(&self) -> DatabaseConnection;
    /// Configure the database connection parameters. See corresponding driver implementation for details.
    async fn configure(&self) -> Result<()>;
    /// Checkpointing allows the driver to perform any necessary operations at designated points determined by the
    /// calling code.
    async fn checkpoint(&self) -> Result<()>;
}
