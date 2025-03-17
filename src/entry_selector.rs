use crate::prelude::*;
use fieldx_plus::{fx_plus, Child};
use moka::ops::compute::Op;
use std::{future::Future, sync::Arc};

#[fx_plus(child(WBCache<DC>, rc_strong), default(off), sync)]
pub struct WBEntryKeySelector<DC>
where
    DC: WBDataController,
{
    #[fieldx(predicate, get)]
    primary_key: DC::Key,

    #[fieldx(get)]
    key: DC::Key,

    #[fieldx(get(copy))]
    is_primary: bool,
}

impl<DC> WBEntryKeySelector<DC>
where
    DC: WBDataController,
{
    pub async fn and_try_compute_with<F, Fut>(self, f: F) -> Result<WBCompResult<DC>, DC::Error>
    where
        F: FnOnce(Option<WBEntry<DC>>) -> Fut,
        Fut: Future<Output = Result<Op<DC::Value>, DC::Error>>,
    {
        let parent = self.parent();
        Ok(if self.is_primary {
            parent.get_and_try_compute_with_primary(self.key(), f).await?
        }
        else {
            parent.get_and_try_compute_with_secondary(self.key(), f).await?
        })
    }

    pub async fn or_try_insert_with<F>(self, init: F) -> Result<WBEntry<DC>, Arc<DC::Error>>
    where
        F: Future<Output = Result<DC::Value, DC::Error>>,
    {
        let parent = self.parent();
        if self.is_primary {
            parent.get_or_try_insert_with_primary(self.key(), init).await
        }
        else {
            parent.get_or_try_insert_with_secondary(self.key(), init).await
        }
    }
}
