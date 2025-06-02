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
    #[fieldx(optional, get)]
    primary_key: DC::Key,

    #[fieldx(get)]
    key: DC::Key,

    #[fieldx(get(copy))]
    primary: bool,
}

impl<DC> EntryKeySelector<DC>
where
    DC: DataController,
{
    pub async fn and_try_compute_with<F, Fut>(self, callback: F) -> Result<CompResult<DC>, Arc<DC::Error>>
    where
        F: FnOnce(Option<Entry<DC>>) -> Fut,
        Fut: Future<Output = Result<Op<DC::Value>, DC::Error>>,
    {
        let parent = self.parent();
        Ok(if self.primary {
            parent.get_and_try_compute_with_primary(self.key(), callback).await?
        } else {
            parent.get_and_try_compute_with_secondary(self.key(), callback).await?
        })
    }

    #[instrument(level = "trace", skip(init))]
    pub async fn or_try_insert_with<F>(self, init: F) -> Result<Entry<DC>, Arc<DC::Error>>
    where
        F: Future<Output = Result<DC::Value, DC::Error>>,
    {
        let parent = self.parent();
        if self.primary {
            parent.get_or_try_insert_with_primary(self.key(), init).await
        } else {
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
