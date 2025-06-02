use async_trait::async_trait;
use std::fmt::Debug;
use std::fmt::Display;
use std::hash::Hash;
use std::sync::Arc;

use crate::types::DataControllerOp;
use crate::types::DataControllerResponse;
use crate::update_iterator::UpdateIterator;
use crate::wbdc_response;

/// The [data controller](crate#data-controller) implementation.
///
/// Use of the [`Cache`](crate::Cache) must start with creating a type that implements this trait.
///
/// This crate includes example implementations of data controllers in the simulation code. These can be found in the
/// [`test::simulation::db::entity`](crate::test::simulation::db::entity) module as the `Manager` types.  Since they all
/// use the same backend (SeaORM framework), they implement the trait
/// [`DCCommon`](crate::test::simulation::db::cache::DCCommon) which provides a set of common methods for simulation
/// data controllers.
#[async_trait]
pub trait DataController: Sized + Send + Sync + 'static {
    /// The key type to be used with methods like [`Cache::get()`](crate::Cache::get) or
    /// [`Cache::entry()`](crate::Cache::entry).
    ///
    /// To implement multi-key access you can use an enum as the key type. See, for example,
    /// [`CustomerBy`](crate::test::simulation::db::entity::customer::CustomerBy) key type in the supplied simulation
    /// code.
    type Key: Debug + Display + Hash + Clone + Eq + Sized + Send + Sync + 'static;
    /// The record type to be stored in the cache and maintained by the data controller.
    type Value: Debug + Clone + Send + Sync + 'static;
    /// The type of the update pool records that are produced by the data controller.
    ///
    /// The simulation code uses the [`CacheUpdates`](crate::test::simulation::db::cache::CacheUpdates) enum for this purpose.
    /// Then, every every model's data controller uses this enum with respective active model type. See the implemenation
    /// for [the customer model](crate::test::simulation::db::entity::customer::Manager::CacheUpdate) for example.
    type CacheUpdate: Debug + Send + Sync + 'static;
    /// The error type that the data controller can return. Note that this is the only error type the the cache
    /// controller is using!
    type Error: Display + Debug + Send + Sync + 'static;

    /// Must return the value for the given key, if it exists, `None` otherwise.
    ///
    /// Example: [`customer model`](crate::test::simulation::db::entity::customer::Manager::get_for_key).
    async fn get_for_key(&self, key: &Self::Key) -> Result<Option<Self::Value>, Self::Error>;
    /// Given a list of update records, must apply these updates to the underlying backend.
    ///
    /// Example: [`DCCommon::wbdc_write_back()`](crate::test::simulation::db::cache::DCCommon::wbdc_write_back).
    async fn write_back(&self, updates: Arc<UpdateIterator<Self>>) -> Result<(), Self::Error>;
    /// Called when a new record is added to the cache. On success, it must return a
    /// [`DataControllerResponse`](crate::types::DataControllerResponse) with the operation type set to what the
    /// data controller considers appropriate for the new record. For example, if it is known that the new record will
    /// have exactly the same field values after being written to the backend as it had when submitted to this method,
    /// then the operation can be set to [`DataControllerOp::Insert`].
    ///
    /// Example: [`DCCommon::wbdbc_on_new()`](crate::test::simulation::db::cache::DCCommon::wbdbc_on_new). The trait
    /// classifies DB models into two categories: immutable and mutable. Immutable models do not modify written records
    /// (e.g., when there is no primary key autoincrement) and their records are immediately cached without a roundtrip
    /// to the database.
    async fn on_new(&self, key: &Self::Key, value: &Self::Value) -> Result<DataControllerResponse<Self>, Self::Error>;
    /// Called when there is a `delete` request for the given key has been received. Don't mix it up with the `invalidate`
    /// request which is only clearing a cache entry.
    ///
    /// The most typical response operation for this method is [`DataControllerOp::Revoke`].
    ///
    /// Example: [`DCCommon::wbdc_on_delete()`](crate::test::simulation::db::cache::DCCommon::wbdc_on_delete).
    async fn on_delete(
        &self,
        key: &Self::Key,
        update: Option<&Self::CacheUpdate>,
    ) -> Result<DataControllerResponse<Self>, Self::Error>;
    /// Called when a record is modified by the user.
    ///
    /// Example: [`DCCommon::wbdc_on_change()`](crate::test::simulation::db::cache::DCCommon::wbdc_on_change).
    async fn on_change(
        &self,
        key: &Self::Key,
        value: &Self::Value,
        old_value: Self::Value,
        prev_handler: Option<Self::CacheUpdate>,
    ) -> Result<DataControllerResponse<Self>, Self::Error>;
    /// Returns the primary key for the given value.
    ///
    /// Example: [`customer model`](crate::test::simulation::db::entity::customer::Manager::primary_key_of).
    fn primary_key_of(&self, value: &Self::Value) -> Self::Key;

    /// Returns a list of secondary keys for the given value. Default implementation returns an empty vector.
    ///
    /// Example: [`customer model`](crate::test::simulation::db::entity::customer::Manager::secondary_keys_of).
    fn secondary_keys_of(&self, _value: &Self::Value) -> Vec<Self::Key> {
        Vec::new()
    }

    /// Take a key and return its corresponding primary key.  This operation may require a backend request.
    ///
    /// The default implementation covers the simple case where there are no secondary keys, simply returning the key
    /// itself.
    ///
    /// Example: [`customer model`](crate::test::simulation::db::entity::customer::Manager::get_primary_key_for).
    async fn get_primary_key_for(&self, key: &Self::Key) -> Result<Option<Self::Key>, Self::Error> {
        Ok(Some(key.clone()))
    }

    /// Returns `true` if the given key is considered the primary key.
    ///
    /// The default implementation always returns `true`, which is appropriate when no secondary keys exist.
    ///
    /// Example: [`customer model`](crate::test::simulation::db::entity::customer::Manager::is_primary).
    fn is_primary(&self, _key: &Self::Key) -> bool {
        true
    }

    /// This method is called on every access to the given key in the cache. It can be used to implement various related
    /// functionality like updateing the access time column in the backend, or calculating related metrics or
    /// statistics.
    ///
    /// The default implementation does nothing and returns a no-op response.
    #[inline(always)]
    async fn on_access(
        &self,
        _key: &Self::Key,
        _value: &Self::Value,
        prev_update: Option<Self::CacheUpdate>,
    ) -> Result<DataControllerResponse<Self>, Self::Error> {
        // log::debug!("default DC on_access for '{_key}'");
        Ok(wbdc_response!(DataControllerOp::Nop, prev_update))
    }
}

/// The observer trait is used by the cache controller to notify user code about various events that happen in the
/// cache controller.
///
/// _Note_: This functionality is currently rather limited and is considered experimental.
#[async_trait]
pub trait Observer<DC>: Send + Sync + 'static
where
    DC: DataController,
{
    async fn on_flush(&self, _cache_updates: Arc<UpdateIterator<DC>>) -> Result<(), DC::Error> {
        Ok(())
    }
    async fn on_flush_one(&self, _updates: &DC::Key, _update: &DC::CacheUpdate) -> Result<(), Arc<DC::Error>> {
        Ok(())
    }
    async fn on_monitor_error(&self, _error: &Arc<DC::Error>) {}
    async fn on_error(&self, _error: Arc<DC::Error>) {}
    async fn on_warning(&self, _message: &str) {}
    async fn on_debug(&self, _message: &str) {}
}
