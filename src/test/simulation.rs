//! This module serves a few purposes:
//!
//! 1. Test to verify various aspects of the `wb-cache` crate functionality.
//! 2. Benchmark to run exactly the same scenario with and without caching.
//! 3. Serve as a sample implementation of [data controllers](crate#data-controller) and user
//!    code using caching in a semi-realistic setting.
//!
//! Since, one way or another, everything revolves around the simulation, this term is used most frequently in all
//! references to this module.
//!
//! # Introduction
//!
//! The big challenge of benchmarking is to produce results that closely resemble real-life applications. This
//! simulation attempts to solve that problem by imitating an e-commerce startup.
//!
//! Here is the business plan:
//!
//! 1. Offer a number of products for sale.
//! 2. For each product, have a supplier with parameters such as reliability and shipping time.
//! 3. Start with some or no customers.
//! 4. Allow the number of customers to grow each day, up to a market capacity that cannot be exceeded.
//! 5. Initially, the customer base grows slowly; until an inflection point is reached when growth increases; after the
//!    point, the growth slows down gradually.
//! 6. Each customer purchasing habits may vary with some being more regular and others more erratic.
//! 7. The number of units sold per day for a product is a function of its popularity and price.
//! 8. For simplicity, each order consists of a single product.
//! 9. Due to irregularities in shipments, it is possible to run out of stock. In that case, orders for that product
//!    will remain pending until the inventory is replenished (backordering).
//! 10. Each simulation step corresponds to a single day.
//!
//! To achieve near-realistic parameters, various processes are simulated using random distributions that best capture
//! the intended characteristics.
//!
//! # Components
//!
//! The simulation consists of a few major components:
//!
//! - The [application](sim_app::EcommerceApp) responsible for setting up the environment and binding the other
//!   components together.
//! - [Backend database support](db).
//! - A scriptwriter that creates a step-by-step scenario describing the events that affect the database content.
//! - Actors that execute the screenplay. Each simulation run involves two actors performing simultaneously: one
//!   implements the non-cached (plain) mechanics, and the other the cached version.
//!
//! # How It Works
//!
//! To achieve stable results, the simulation is implemented as follows:
//!
//! First, a scenario is generated. It is either created randomly by the [scriptwriter](scriptwriter::ScriptWriter)
//! or loaded from a file where it was saved after a previous run. Saving and loading scenarios can be used to ensure
//! that exactly the same benchmark is run each time.
//!
//! The scenario is then executed by the [actors](actor::TestActor), as mentioned above. To mitigate the influence
//! of various performance-affecting factors, the actors are run in parallel. However, it must be kept in mind that
//! the caching actor normally finishes much earlier than the plain actor.
//!
//! If the [`sim_app::EcommerceApp`] receives the `--test` command line argument, it will compare
//! the results of the two actors by matching the database content on a record-by-record basis. If discrepancies are
//! found, the simulation will fail with an error message indicating the first mismatch.
//!
//! # Running the Simulation
//!
//! To run the simulation, execute one of the following commands:
//!
//! ```shell
//! cargo run --profile release --feature sqlite --example company -- --sqlite --test
//! ```
//!
//! or
//!
//! ```shell
//! cargo run --profile release --features pg --example company -- --pg --test
//! ```
//!
//! _**Note:** The release profile is optional but speeds up the caching code, yielding better benchmarking results._
//!
//! Running with the PostgreSQL driver requires connection parameters to be provided. This can be done either via the
//! command line or environment variables. To see the available options, run:
//!
//! ```shell
//! cargo run --features pg --example company -- --help
//! ```
//!
//! or, to get full help:
//!
//! ```shell
//! cargo run --all-features --example company -- --help
//! ```
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

use crate::test::simulation::sim_app::ActorStatus;

pub trait SimulationApp: Debug + Sync + Send + 'static {
    fn acquire_progress<'a>(
        &'a self,
        style: PStyle,
        order: Option<POrder<'a>>,
    ) -> Result<Option<ProgressBar>, SimErrorAny>;

    fn app_is_gone() -> SimErrorAny {
        simerr!("App is gone")
    }
    fn set_cached_status(&self, status: ActorStatus);
    fn set_plain_status(&self, status: ActorStatus);

    fn report_debug<S: ToString>(&self, msg: S);
    fn report_info<S: ToString>(&self, msg: S);
    #[allow(unused)]
    fn report_warn<S: ToString>(&self, msg: S);
    fn report_error<S: ToString>(&self, msg: S);
}
