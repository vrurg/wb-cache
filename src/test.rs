#![cfg(any(test, feature = "test"))]
pub mod db;
pub mod progress;
pub mod scriptwriter;
pub mod types;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Error;
use anyhow::Result;
use db::migrations::Migrator;
use fieldx_plus::fx_plus;
use scriptwriter::steps::Step;
use sea_orm::prelude::*;
use sea_orm_migration::MigratorTrait;

pub trait TestProgress {
    fn set_length(&self, length: u64);
    fn set_message(&self, message: String);
    fn inc(&self, inc: u64);
}

pub trait TestApp: Sync + Send + 'static {
    fn tempdir(&self) -> Result<PathBuf>;
}

#[fx_plus(agent(APP, unwrap(map(Error, app_is_gone))), fallible(off, error(Error)), sync)]
pub struct TestCompany<APP: TestApp> {
    #[fieldx(mode(async), lazy, get, fallible)]
    db: Arc<DatabaseConnection>,
}

impl<APP: TestApp> TestCompany<APP> {
    async fn build_db(&self) -> Result<Arc<DatabaseConnection>> {
        let app = self.app()?;
        let tempdir = app.tempdir()?;
        let sqlite_path = tempdir.join("test_company.db");

        let db = Arc::new(sea_orm::Database::connect(format!("sqlite://{}?mode=rwc", sqlite_path.display())).await?);

        Migrator::up(&*db, None).await?;

        Ok(db)
    }

    fn app_is_gone(&self) -> Error {
        Error::msg("Application object is gone")
    }

    pub async fn act(&self, screenplay: Vec<Step>) -> Result<()> {
        let app = self.app()?;
        let db = self.db().await?;

        Ok(())
    }
}
