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
//! The benchmarking is implemented by the [`test::simulation`] module. It simulates an e-commerce startup using a
//! semi-realistic model. The outcomes are expected to vary under different conditions. Here is what was used for these
//! two:
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
//! For simplicity, the cache controller is often referred to simply as "cache," unless it is necessary
//! to distinguish it from the inner cache object.
//!
//! # Data Records And Keys
//!
//! The cache controller operates on data records identified by keys. There must be one primary key and zero or more
//! secondary keys. The inner cache object may contain multiple entries for the same data record: one entry is the
//! primary key entry, which holds the record itself, while the secondary entries serve as references to the primary key
//! entry.
//!
//! The above fact means that cache maximum capacity doesn't necessarily mean the maximum number of records that can be
//! held in the cache.
//!
//! # Cache Controller Components
//!
//! ![](https://raw.githubusercontent.com/vrurg/wb-cache/8164e6cbc9da18d95f20b07c1fe1e5e147eecaf4/docs/cache_controller.svg)
//!
//! The diagram above illustrates the main components of the cache controller. Here is about the purpose of each of them in few words:
//!
//! - Data Controller – see the section below for more details.
//! - Updates Pool – the data controller produces update records that are stored in the pool until they are flushed to the backend.
//! - Cache – the actual [moka](https://crates.io/crates/moka) cache object.
//! - Monitoring Task – a background task that monitors the cache and auto-flushes updates to the backend.
//! - Observers – a list of user-registered objects that are listening to cache events.
//!
//! ## The Cache Object
//!
//! There is not much to say about the cache object itself. The only two parameters of the cache controller that are
//! used to configure the inner cache is the maximum capacity and cache name. The object is set to use Tiny LFU eviction
//! policy.
//!
//! ## The Updates Pool
//!
//! The cache controller is totally agnostic about the content of the updates pool as its purpose is to store records
//! produced by the data controller. The pool is always indexed by the primary key of the record meaning that the
//! maximum pool capacity is the actual maximum number of record updates that can be stored in the cache.
//!
//! The pool helps to minimize the number of individual writes to the backend by batching them together.
//!
//! ## The Monitor Task
//!
//! The monitor task runs in the background and monitors the cache state. If the maximum update pool capacity is
//! reached, if the flush timeout expires, or if the cache is being shut down, the task initiates a batch flush
//! operation.
//!
//! The task is defined by two timing parameters: `monitor_tick_duration` and `flush_interval`. The first specifies the
//! time interval between consecutive checks of the cache state. The second specifies the time after which the task will
//! initiate a flush operation if there is at least one update in the pool. Setting `flush_interval` to `Duration::MAX`
//! effectively disables the timed flush operation, meaning that it will only be triggered by either exceeding the maximum
//! update pool capacity or by the cache shutdown.
//!
//! Note that a manually initiated batch flush resets the flush timeout.
//!
//! ## Observers
//!
//! User-registered objects that implement [`Observer`] trait and listening to cache events.
//!
//! # Data Controller
//!
//! The diagram reflects an important design decision: the primary component of the cache is not the cache itself, but
//! the data controller. This documentation will delve into the technical details of the controller later; for now, it
//! is important to understand that the controller is not only a layer between the backend and the cache but also the
//! type that, through [`DataController`] trait-associated types, defines the following types: `Key`, `Value`,
//! `CacheUpdate`, and `Error`.
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
//! being sent to the backend. This allows the cache to avoid unnecessary roundtrips to the backend.[^all_in_mem]
//!
//! [^all_in_mem]: In the most extreme case, there is a chance that a newly inserted record may never be written to the
//! backend. This can occur when the record is inserted, then possibly updated, and subsequently deleted quickly enough
//! to avoid auto-flushing.
//!
//! Here is two diagrams illustrating the data flow between the cache and the data controller. The first one is for the
//! most simple case of a new record being inserted into the cache:
//!
//! <a id="on_new"></a>
//! ![](https://raw.githubusercontent.com/vrurg/wb-cache/bbec97b39af581a4b31ad44e2a90ea3e010a5a45/docs/cache_dc_interact_new.svg)
//!
//! Below is the second diagram illustrating a more complex case of processing an update. Note that the diagram assumes that
//! the record requested by the user is already present in the cache. If it is not, the cache would ask the data
//! controller to fetch it from the backend, which is omitted here for simplicity. The scenario where the record is not
//! found in the backend essentially reduces to the case of a new record, so it is not shown here.
//!
//! ![](https://raw.githubusercontent.com/vrurg/wb-cache/bbec97b39af581a4b31ad44e2a90ea3e010a5a45/docs/cache_dc_interact_update.svg)
//!
//! # How Do I Use It?
//!
//! Implement a data controller that conforms to the [`DataController`] trait. Refer to the trait's documentation for
//! complete details on the expected behavior.
//!
//! _The [`test::simulation`] module is provided as an example of using caching in a semi-realistic environment. In
//! particular, the [`test::simulation::db::entity`] module includes SeaORM-based examples that can guide your
//! implementation._
//!
//! Finally, pass an instance of your controller to the [`Cache`
//! builder](cache::CacheBuilder). For instance, if your controller type is `MyDataController`, you can integrate it
//! like this:
//!
//! ```ignore
//! use wb_cache::{Cache, prelude::*};
//! use my_crate::MyDataController;
//!
//! let cache = Cache::builder()
//!     .data_controller(MyDataController)
//!     // ... more parameters if needed ...
//!     .build();
//! ```
//!
//! Sometimes one may consider using the (observers)[#observers] in their application code.  This is primarily a
//! front-end rather than a back-end feature, although that is not a strict rule.
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
#[doc(inline)]
pub use traits::Observer;

pub mod prelude {
    pub use crate::cache::Cache;
    pub use crate::entry::Entry;
    pub use crate::traits::DataController;
    pub use crate::types::*;
    pub use moka::ops::compute::Op;
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
