use crate::cache::ValueState;
use crate::cache::WBCache;
use crate::traits::WBDataController;
use fieldx_plus::child_build;
use fieldx_plus::fx_plus;
use moka::Entry;
use std::fmt::Debug;
use std::sync::Arc;
use tracing::instrument;

#[fx_plus(child(WBCache<DC>, rc_strong), sync, default(off))]
pub struct WBEntry<DC>
where
    DC: WBDataController,
{
    key: DC::Key,
    value: DC::Value,
}

impl<DC> WBEntry<DC>
where
    DC: WBDataController,
{
    pub(crate) fn new(parent: &WBCache<DC>, key: DC::Key, value: DC::Value) -> Self {
        child_build!(
            parent,
            WBEntry<DC> {
                key:   key,
                value: value,
            }
        )
        .unwrap()
    }

    // Only valid for primaries
    #[instrument(level = "trace")]
    pub(crate) fn from_primary_entry(
        parent: &WBCache<DC>,
        entry: Entry<DC::Key, ValueState<DC::Key, DC::Value>>,
    ) -> Self {
        child_build!(
            parent,
            WBEntry<DC> {
                key: entry.key().clone(),
                value: entry.into_value().into_value(),
            }
        )
        .unwrap()
    }

    #[instrument(level = "trace")]
    pub async fn value(&self) -> Result<&DC::Value, Arc<DC::Error>> {
        self.parent().on_access(&self.key, Arc::new(self.value.clone())).await?;
        Ok(&self.value)
    }

    pub fn key(&self) -> &DC::Key {
        &self.key
    }

    pub fn into_value(self) -> DC::Value {
        self.value
    }
}

impl<DC> Debug for WBEntry<DC>
where
    DC: WBDataController,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WBEntry")
            .field("key", &self.key)
            .field("value", &self.value)
            .finish()
    }
}
