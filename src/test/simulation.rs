pub mod actor;
pub mod company_cached;
pub mod company_plain;
pub mod db;
pub mod progress;
pub mod scriptwriter;
pub mod sim_app;
pub mod types;

use std::fmt::Debug;

use indicatif::ProgressBar;
use progress::POrder;
use progress::PStyle;
use types::simerr;
use types::SimErrorAny;

pub trait SimulationApp: Debug + Sync + Send + 'static {
    fn acquire_progress<'a>(
        &'a self,
        style: PStyle,
        order: Option<POrder<'a>>,
    ) -> Result<Option<ProgressBar>, SimErrorAny>;

    fn app_is_gone() -> SimErrorAny {
        simerr!("App is gone")
    }
    fn set_cached_per_sec(&self, step: f64);
    fn set_plain_per_sec(&self, step: f64);

    fn report_debug<S: ToString>(&self, msg: S);
    fn report_info<S: ToString>(&self, msg: S);
    fn report_warn<S: ToString>(&self, msg: S);
    fn report_error<S: ToString>(&self, msg: S);
}
