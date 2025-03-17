//! # wb-cache
//!
//! `wb-cache` stands for a simple write-back cache, designed to perform the following tasks in addition
//! to those typical of any other cache implementation:
//!
//! - Provide transparent L1 caching for applications
//! - Offer a generic interface to integrate with any data source (DB, REST, in-memory caching service, etc.)
//! - Accumulate data updates before flushing them back to the data source
//!
//! The crate is based on the [`moka`](https://crates.io/crates/moka) crate and is designed for use in asynchronous contexts.
//!
//! ## Vocabulary
//!
//! - **Data Controller (DC)**: The interface between the cache and the data source. A data controller implements the
//!   [`WBDataController`](crate::traits::WBDataController) trait.
//!
//! ## Basic Principles
//!
//! `wb-cache` is not an ORM in any way. It operates on a record as a whole, whether it represents the complete record
//! from the backend store, a portion of it, or a combination of multiple records; depending on how a data controller is
//! implemented.  Records are identified by their unique keys, which can be considered primary or secondary, and are
//! automatically fetched from the backend upon request by the application.
//!
//! Flushed records are submitted back to the data controller as a single batch. It is up to the controller to optimize
//! the writing process to minimize latencies. The cache does not guarantee the order of the updates.
//!
//! In a complex scenario, an application may not even know where the data comes from. For example, the controller may
//! sit on top of a memory caching service and a database, effectively providing both L1 and L2 caches between the
//! application and the database.
//!
//! ## Reasoning
//!
//! The crate was initiated as part of a local project where its web interface was based on the HTMX framework. This
//! made the project very sensitive to latencies. Even with the database located on a local network, the delays in UI
//! reaction were rather noticeable. Introducing a memory caching service wouldn't help much because it had to be run on
//! a different server as well. The situation was worsened by the fact that the database tables were sparsely related to
//! each other, making it necessary to implement update logic for each table separately.
//!
//! Hence, the idea of a common write-back interface made perfect sense to be implemented.
//!
//! ## Architecture
//!
//! The central part of the cache is the [`WBCache`](crate::cache::WBCache) object. It serves as a front end to the
//! actual `moka` instance.
//!
//! The cache object holds an instance of a data controller that must be manually created, configured by the user,
//! and submitted to the cache builder. Typically, the controller is tailored to the specific kind of record.
//!
//! The cache object also maintains a limited pool of update records. Both the pool size and the record age are restricted.
//! Once these limits are exceeded, the cache begins flushing the updates back to the data controller.
//! If the same record is changed in the cache multiple times between two flushes, the pool will still contain only
//! a single update for that record.
//!
//! The actual content of the update is determined by the data controller, so the cache object remains agnostic about it.
//! This allows the data controller to minimize the amount of data written back to the data source by sending only
//! the actual record changes. In the most extreme cases, creating a new record and immediately deleting it would not
//! even generate any network traffic.
//!
//! ## Caveats
//!
//! Due to the nascent stage of the crate, it is rather inflexible regarding the parameters and features of the
//! underlying `moka` instance. This is likely to change in the future, depending on the demand for the crate.

pub mod cache;
pub mod entry;
pub mod entry_selector;
pub mod traits;
pub mod types;
pub(crate) mod update_state;

pub use cache::WBCache;

pub mod prelude {
    pub use crate::{cache::WBCache, entry::WBEntry, traits::WBDataController, types::*};
}
