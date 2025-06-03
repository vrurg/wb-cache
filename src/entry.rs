use crate::cache::Cache;
use crate::cache::ValueState;
use crate::traits::DataController;
use fieldx_plus::child_build;
use fieldx_plus::fx_plus;
use moka::Entry as MokaEntry;
use std::fmt::Debug;
use std::sync::Arc;
use tracing::instrument;

/// This is a snapshot of a single entry in the cache. Note that as a snapshot, it doesn't refer to the value in the
/// cache, but holds a copy of it.
#[fx_plus(child(Cache<DC>, rc_strong), sync, default(off))]
pub struct Entry<DC>
where
    DC: DataController,
{
    key:   DC::Key,
    value: DC::Value,
}

impl<DC> Entry<DC>
where
    DC: DataController,
{
    pub(crate) fn new(parent: &Cache<DC>, key: DC::Key, value: DC::Value) -> Self {
        child_build!(
            parent,
            Entry<DC> {
                key:   key,
                value: value,
            }
        )
        .unwrap()
    }

    // Only valid for primaries
    #[instrument(level = "trace")]
    pub(crate) fn from_primary_entry(
        parent: &Cache<DC>,
        entry: MokaEntry<DC::Key, ValueState<DC::Key, DC::Value>>,
    ) -> Self {
        child_build!(
            parent,
            Entry<DC> {
                key: entry.key().clone(),
                value: entry.into_value().into_value(),
            }
        )
        .unwrap()
    }

    /// The snapshot value.
    #[instrument(level = "trace")]
    pub async fn value(&self) -> Result<&DC::Value, Arc<DC::Error>> {
        self.parent().on_access(&self.key, Arc::new(self.value.clone())).await?;
        Ok(&self.value)
    }

    /// The key of the entry.
    pub fn key(&self) -> &DC::Key {
        &self.key
    }

    /// Consumes the entry and returns its stored value.
    pub fn into_value(self) -> DC::Value {
        self.value
    }
}

impl<DC> Debug for Entry<DC>
where
    DC: DataController,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("key", &self.key)
            .field("value", &self.value)
            .finish()
    }
}
