use crate::prelude::*;
use fieldx_plus::{fx_plus, Child};
use std::{fmt::Debug, sync::Arc};

// This struct is here to contain a key updating procedures withing single thread or task and to prevent the entire
// cache object to block on `updates` HashMap locks. The point is that it may take a while for a data controller to
// process on_new, on_access and, perhaps, other requests and get back with a response. This 'while' is the time other
// tasks would be waiting for the cache to become available again.
#[fx_plus(child(WBCache<DC>, rc_strong), sync, rc, get(off), default(off))]
pub(crate) struct WBUpdateState<DC>
where
    DC: WBDataController,
{
    #[fieldx(vis(pub(crate)), clearer, predicate, writer, builder(off))]
    update: DC::CacheUpdate,

    #[fieldx(lock, get(vis(pub(crate)), copy), set(private), default(false), builder(off))]
    is_delete: bool,
}

// !!! IMPORTANT NOTE TO MYSELF: never try to update parent's cache object here! We only take care of the single update
// and do nothing else.
impl<DC> WBUpdateState<DC>
where
    DC: WBDataController + Send + Sync + 'static,
{
    pub(crate) async fn on_new(&self, key: &DC::Key, value: DC::Value) -> Result<(), DC::Error> {
        let mut guard = self.write_update();
        let parent = self.parent();
        *guard = parent.data_controller().on_new(key, &value).await?;
        self.set_is_delete(false);
        Ok(())
    }

    pub(crate) async fn on_change(
        &self,
        key: &DC::Key,
        value: &DC::Value,
        old_value: DC::Value,
    ) -> Result<(), DC::Error> {
        let mut guard = self.write_update();
        let parent = self.parent();
        log::debug!("[{}] passing on_change({key}) event to DC", parent.name());
        *guard = parent
            .data_controller()
            .on_change(key, value, old_value, guard.take())
            .await?;
        self.set_is_delete(false);
        Ok(())
    }

    pub(crate) async fn on_access(&self, key: &DC::Key, value: &DC::Value) -> Result<(), DC::Error> {
        // log::debug!("UPDATE STATE ACCESS({key})");
        let mut guard = self.write_update();
        let parent = self.parent();
        // log::debug!("updating '{key}' with DC on_access");
        *guard = parent.data_controller().on_access(key, value, guard.take()).await?;
        // log::debug!("updated '{key}' with DC on_access");
        self.set_is_delete(false);
        Ok(())
    }

    pub(crate) async fn on_delete(&self, key: &DC::Key) -> Result<(), DC::Error> {
        let mut guard = self.write_update();
        let parent = self.parent();
        *guard = parent.data_controller().on_delete(key).await?;
        self.set_is_delete(true);
        Ok(())
    }
}

impl<DC> Debug for WBUpdateState<DC>
where
    DC: WBDataController,
{
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.write_update();
        write!(fmt, "WBUpdateState {{ {:?} }}", *guard)
    }
}
