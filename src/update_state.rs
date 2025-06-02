use crate::prelude::*;
use fieldx_plus::fx_plus;
use fieldx_plus::Child;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::RwLock;

// This struct is here to contain a key updating procedures withing single thread or task and to prevent the entire
// cache object to block on `updates` HashMap locks. The point is that it may take a while for a data controller to
// process on_new, on_access and, perhaps, other requests and get back with a response. This 'while' is the time other
// tasks would be waiting for the cache to become available again.
#[fx_plus(child(Cache<DC>, rc_strong), sync, rc, get(off), default(off))]
pub(crate) struct UpdateState<DC>
where
    DC: DataController,
{
    #[fieldx(builder(off))]
    pub(crate) data: Arc<RwLock<Option<DC::CacheUpdate>>>,
}

// !!! IMPORTANT NOTE TO MYSELF: never try to update parent's cache object here! We only take care of the single update
// and do nothing else.
impl<DC> UpdateState<DC>
where
    DC: DataController + Send + Sync + 'static,
{
    pub(crate) async fn on_new(&self, key: &DC::Key, value: &DC::Value) -> Result<DataControllerOp, DC::Error> {
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
    ) -> Result<DataControllerOp, DC::Error> {
        let mut guard = self.data.write().await;
        let parent = self.parent();
        let mut dc_response = parent
            .data_controller()
            .on_change(key, &*value, old_value, guard.take())
            .await?;
        *guard = dc_response.update.take();
        Ok(dc_response.op)
    }

    pub(crate) async fn on_access(&self, key: &DC::Key, value: Arc<DC::Value>) -> Result<DataControllerOp, DC::Error> {
        let mut guard = self.data.write().await;
        let parent = self.parent();
        let mut dc_response = parent.data_controller().on_access(key, &value, guard.take()).await?;
        *guard = dc_response.update.take();
        Ok(dc_response.op)
    }

    pub(crate) async fn on_delete(&self, key: &DC::Key) -> Result<DataControllerOp, DC::Error> {
        let mut guard = self.data.write().await;
        let parent = self.parent();
        let mut dc_response = parent.data_controller().on_delete(key, guard.as_ref()).await?;
        *guard = dc_response.update.take();
        Ok(dc_response.op)
    }
}

impl<DC> Debug for UpdateState<DC>
where
    DC: DataController,
{
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "UpdateState {{ {:?} }}", self.data)
    }
}
