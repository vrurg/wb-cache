use crate::prelude::*;
use fieldx_plus::fx_plus;
use fieldx_plus::Child;
use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;
use tracing::instrument;

#[fx_plus(child(Cache<DC>, rc_strong), default(off), sync, builder(vis(pub(crate))))]
pub struct EntryKeySelector<DC>
where
    DC: DataController,
{
    /// Contains the primary key for existing entries.
    #[fieldx(optional, get)]
    primary_key: DC::Key,

    /// The key for which this selector is created.
    #[fieldx(get)]
    key: DC::Key,

    /// True if this is a primary key selector, false if it is a secondary key selector.
    #[fieldx(get(copy))]
    primary: bool,
}

impl<DC> EntryKeySelector<DC>
where
    DC: DataController,
{
    /// Performs a compute operation on the entry identified by the key. If the entry is not cached yet the controller
    /// will try to pull it from the backend via its data controller. If the entry is not found, the callback will be
    /// called with `None`.
    ///
    /// The callback should return a `Result<Op<DC::Value>, DC::Error>`, where [`Op`] will instruct the controller how
    /// to use the returned value.
    ///
    /// Returns a [result](`CompResult`) of the operation if it succeeds or an error.
    #[instrument(level = "trace", skip(callback))]
    pub async fn and_try_compute_with<F, Fut>(self, callback: F) -> Result<CompResult<DC>, Arc<DC::Error>>
    where
        F: FnOnce(Option<Entry<DC>>) -> Fut,
        Fut: Future<Output = Result<Op<DC::Value>, DC::Error>>,
    {
        let parent = self.parent();
        Ok(if self.primary {
            parent.get_and_try_compute_with_primary(self.key(), callback).await?
        }
        else {
            parent.get_and_try_compute_with_secondary(self.key(), callback).await?
        })
    }

    /// Returns an [`Entry`] for the requested key if it's either cached or can be fetched from the backend. Otherwise
    /// uses the give `init` future to create a new entry.
    #[instrument(level = "trace", skip(init))]
    pub async fn or_try_insert_with<F>(self, init: F) -> Result<Entry<DC>, Arc<DC::Error>>
    where
        F: Future<Output = Result<DC::Value, DC::Error>>,
    {
        let parent = self.parent();
        if self.primary {
            parent.get_or_try_insert_with_primary(self.key(), init).await
        }
        else {
            parent.get_or_try_insert_with_secondary(self.key(), init).await
        }
    }
}

impl<DC> Debug for EntryKeySelector<DC>
where
    DC: DataController,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryKeySelector")
            .field("primary_key", &self.primary_key)
            .field("key", &self.key)
            .field("is_primary", &self.primary)
            .finish()
    }
}
