use crate::prelude::*;
use fieldx_plus::fx_plus;
use fieldx_plus::Child;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::RwLock;

// TODO! The data controller should be able to report back the status of the processed update. Currently, the
// anticipated statuses are:
//
// - 'final' where the controller returns DC::CacheUpdate and the final DC::Value which can be sent back to the cache
// - 'pending' which is the current behavior where the cache is not updated and only the .update field is refreshed
//
// The update object must be marked accordingly to the outcome of calling the data controller method. Later on,
// maybe_flush would check the status of the update and decide whether to flush it or not. Eventually, this would allow
// prevent any writes to the backend in cases where the final operation on the value is `delete`.

// This struct is here to contain a key updating procedures withing single thread or task and to prevent the entire
// cache object to block on `updates` HashMap locks. The point is that it may take a while for a data controller to
// process on_new, on_access and, perhaps, other requests and get back with a response. This 'while' is the time other
// tasks would be waiting for the cache to become available again.
#[fx_plus(child(WBCache<DC>, rc_strong), sync, rc, get(off), default(off))]
pub(crate) struct WBUpdateState<DC>
where
    DC: WBDataController,
{
    #[fieldx(builder(off))]
    pub(crate) data: Arc<RwLock<Option<DC::CacheUpdate>>>,
}

// !!! IMPORTANT NOTE TO MYSELF: never try to update parent's cache object here! We only take care of the single update
// and do nothing else.
impl<DC> WBUpdateState<DC>
where
    DC: WBDataController + Send + Sync + 'static,
{
    pub(crate) async fn on_new(&self, key: &DC::Key, value: &DC::Value) -> Result<WBDataControllerOp, DC::Error> {
        let mut guard = self.data.write().await;
        let parent = self.parent();
        let mut dc_response = parent.data_controller().on_new(key, value).await?;

        *guard = dc_response.update.take();

        Ok(dc_response.op)
    }

    pub(crate) async fn on_change(
        &self,
        key: &DC::Key,
        value: Arc<DC::Value>,
        old_value: DC::Value,
    ) -> Result<WBDataControllerOp, DC::Error> {
        let mut guard = self.data.write().await;
        let parent = self.parent();
        let mut dc_response = parent
            .data_controller()
            .on_change(key, &*value, old_value, guard.take())
            .await?;
        *guard = dc_response.update.take();
        Ok(dc_response.op)
    }

    pub(crate) async fn on_access(
        &self,
        key: &DC::Key,
        value: Arc<DC::Value>,
    ) -> Result<WBDataControllerOp, DC::Error> {
        let mut guard = self.data.write().await;
        let parent = self.parent();
        let mut dc_response = parent.data_controller().on_access(key, &value, guard.take()).await?;
        *guard = dc_response.update.take();
        Ok(dc_response.op)
    }

    pub(crate) async fn on_delete(&self, key: &DC::Key) -> Result<WBDataControllerOp, DC::Error> {
        let mut guard = self.data.write().await;
        let parent = self.parent();
        let mut dc_response = parent.data_controller().on_delete(key, guard.as_ref()).await?;
        *guard = dc_response.update.take();
        Ok(dc_response.op)
    }
}

impl<DC> Debug for WBUpdateState<DC>
where
    DC: WBDataController,
{
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "WBUpdateState {{ {:?} }}", self.data)
    }
}
