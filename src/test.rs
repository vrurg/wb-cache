#[cfg(not(any(feature = "pg", feature = "sqlite")))]
compile_error!("At least one of the features `pg` or `sqlite` must be enabled to use simulation module.");
#[cfg(feature = "simulation")]
pub mod simulation;
