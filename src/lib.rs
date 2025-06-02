//! # wb-cache
//!
//! Generic write-behind caching solution for key-indexed record-based storages.
//!
//! Think of it as an L1 cache for backend storage.
//!
//! # What's In It For Me?
//!
//! The table is, perhaps, the best answer:
//!
//! | DB | Plain, sec | Cached, sec | Ratio |
//! | -- | ---------- | ----------- | ----- |
//! | PostgreSQL | 27073.36 | 277.95 | **97.44x** |
//! | SQLite | 137.93 | 14.35 | **9.61x** |
//!
//! The test code is available in the `test` module. It implements a simplified simulation of an e-commerce startup with
//! as realistic a model as possible. Apparently, the outcomes in different conditions would vary. Here is what was
//! used for this two:
//!
//! - PostgreSQL 17
//!   * QNAP TVS-h874X, 12th Gen Intel® Core™ i9-12900E, 64GB RAM, Docker container on Samsung SSD 990 EVO 2TB
//!   * Local network latency: 3ms on average
//! - SQLite
//!   * Apple Mac Studio M2 Ultra, 128GB RAM, SSD AP2048Z
//!
//! # The Basics
//!
//! The `wb-cache` crate is designed for the following use case:
//!
//! - Key-indexed, record-based storage; e.g., database tables, NoSQL databases, and even external in-memory caches.
//! - Unsatisfactory latency in data exchange operations.
//! - Batching write operations to storage is beneficial for performance.
//!
//! The cache operates on the following principles:
//!
//! - It is backend-agnostic.
//! - It is key and value agnostic.
//! - Keys are classified as "primary" and "secondary," and each record can have none, one, or many secondary keys.
//! - Implemented as a controller over the [moka](https://crates.io/crates/moka) cache.
//! - Fully async.
//! - As an "L1" cache, it doesn't support distributed caching.
//!
//! The cache's principal model is as follows:
//!
//! ![](https://raw.githubusercontent.com/vrurg/wb-cache/bbec97b39af581a4b31ad44e2a90ea3e010a5a45/docs/operations.svg)
//!
//! # Data Controller
//!
//! The diagram reflects an important design decision: the primary component of the cache is not the cache itself, but
//! the data controller. This documentation will delve into the technical details of the controller later; for now, it
//! is important to understand that the controller is not only a layer between the backend and the cache but also the
//! type that, through trait-associated types, defines the following types: `Key`, `Value`, `CacheUpdate`, and `Error`.
//!
//! The data controller is also responsible for producing update records that can later be used for efficient backend
//! updates. For example, the simulation included with this crate uses SeaORM's ActiveModel to detect differences
//! between a record's previous and updated content. These differences are merged with the existing update state to
//! create a final update record, which will eventually be submitted to the underlying database table — unless the
//! original record has been deleted from the cache, in which case the update record is simply discarded.
//!
//! There is one more role that the data controller plays: it tells the cache what to do with a new or updated
//! data record received from the user. It can be ignored, inserted into the cache, revoked, or revoked with its
//! corresponding update record dropped as well, which means a full delete. The simulation uses this feature to
//! optimize caching by immediately inserting records when it is known that their content will not change after
//! being sent to the backend. This allows the cache to avoid unnecessary roundtrips to the backend.
//!
//! ![](https://raw.githubusercontent.com/vrurg/wb-cache/bbec97b39af581a4b31ad44e2a90ea3e010a5a45/docs/cache_dc_interact_new.svg)
//! ![](https://raw.githubusercontent.com/vrurg/wb-cache/bbec97b39af581a4b31ad44e2a90ea3e010a5a45/docs/cache_dc_interact_update.svg)

pub mod cache;
pub mod entry;
pub mod entry_selector;
pub mod test;
pub mod traits;
pub mod types;
pub mod update_iterator;
pub(crate) mod update_state;

#[doc(inline)]
pub use cache::Cache;
#[doc(inline)]
pub use traits::DataController;

pub mod prelude {
    pub use crate::cache::Cache;
    pub use crate::entry::Entry;
    pub use crate::traits::DataController;
    pub use crate::types::*;
}

#[macro_export]
macro_rules! wbdc_response {
    ($op:expr, $update:expr) => {
        $crate::types::DataControllerResponse {
            op: $op,
            update: $update,
        }
    };
}
