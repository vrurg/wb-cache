// PostgreSQL
// [INFO]       plain |      cached
// [INFO]   26312.69s |     297.98s
// [INFO]      88.30x |       1.00x

//! # wb-cache
//!
//! Generic write-behind caching solution for key-indexed record-based storages.

pub mod cache;
pub mod entry;
pub mod entry_selector;
#[cfg(any(test, feature = "test"))]
pub mod test;
pub mod traits;
pub mod types;
pub(crate) mod update_iterator;
pub(crate) mod update_state;

#[doc(inline)]
pub use cache::WBCache;
#[doc(inline)]
pub use traits::WBDataController;

pub mod prelude {
    pub use crate::cache::WBCache;
    pub use crate::entry::WBEntry;
    pub use crate::traits::WBDataController;
    pub use crate::types::*;
}

#[macro_export]
macro_rules! wbdc_response {
    ($op:expr, $update:expr) => {
        $crate::types::WBDataControllerResponse {
            op: $op,
            update: $update,
        }
    };
}
