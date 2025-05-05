#![cfg(any(test, feature = "test"))]
pub mod actor;
pub mod company_cached;
pub mod company_plain;
pub mod db;
pub mod progress;
pub mod scriptwriter;
pub mod sim_app;
pub mod types;

use indicatif::ProgressBar;
use progress::POrder;
use progress::PStyle;
use std::path::PathBuf;
use types::simerr;
use types::SimErrorAny;

pub trait TestApp: Sync + Send + 'static {
    fn db_dir(&self) -> Result<PathBuf, SimErrorAny>;
    fn acquire_progress<'a>(
        &'a self,
        style: PStyle,
        order: Option<POrder<'a>>,
    ) -> Result<Option<ProgressBar>, SimErrorAny>;

    fn app_is_gone() -> SimErrorAny {
        simerr!("App is gone")
    }
}
