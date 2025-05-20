use async_trait::async_trait;
use std::fmt::Debug;
use std::fmt::Display;
use std::hash::Hash;
use std::sync::Arc;

use crate::update_iterator::WBUpdateIterator;

// For types that are in charge of reading/writing records.
#[async_trait]
pub trait WBDataController: Sized + Send + Sync + 'static {
    /// The key type to be used with methods like [`WBCache::get()`](crate::WBCache::get) or
    /// [`WBCache::entry()`](crate::WBCache::entry).
    ///
    /// To implement multi-key access you can use an enum as the key type.
    type Key: Debug + Display + Hash + Clone + Eq + Sized + Send + Sync + 'static;
    type Value: Debug + Clone + Send + Sync + 'static;
    type CacheUpdate: Debug + Send + Sync + 'static;
    type Error: Display + Debug + Send + Sync + 'static;

    async fn get_for_key(&self, key: &Self::Key) -> Result<Option<Self::Value>, Self::Error>;
    // In the future the method might return a list of instructions for the cache on how to act upon modified keys.
    async fn write_back(&self, updates: Arc<WBUpdateIterator<Self>>) -> Result<(), Self::Error>;
    async fn on_new(&self, key: &Self::Key, value: &Self::Value) -> Result<Option<Self::CacheUpdate>, Self::Error>;
    async fn on_delete(&self, key: &Self::Key) -> Result<Option<Self::CacheUpdate>, Self::Error>;
    async fn on_change(
        &self,
        key: &Self::Key,
        value: &Self::Value,
        old_value: Self::Value,
        prev_handler: Option<Self::CacheUpdate>,
    ) -> Result<Option<Self::CacheUpdate>, Self::Error>;
    fn primary_key_of(&self, value: &Self::Value) -> Self::Key;

    /// Returns a list of secondary keys for the given value.
    /// Default implementation returns an empty vector.
    fn secondary_keys_of(&self, _value: &Self::Value) -> Vec<Self::Key> {
        Vec::new()
    }

    // The following implementations cover plain, non-enum, key.

    // Take any key and return corresponding primary.
    async fn get_primary_key_for(&self, key: &Self::Key) -> Result<Option<Self::Key>, Self::Error> {
        Ok(Some(key.clone()))
    }

    fn is_primary(&self, _key: &Self::Key) -> bool {
        true
    }

    #[inline(always)]
    async fn on_access(
        &self,
        _key: &Self::Key,
        _value: &Self::Value,
        prev_update: Option<Self::CacheUpdate>,
    ) -> Result<Option<Self::CacheUpdate>, Self::Error> {
        // log::debug!("default DC on_access for '{_key}'");
        Ok(prev_update)
    }
}

#[async_trait]
pub trait WBObserver<DC>: Send + Sync + 'static
where
    DC: WBDataController,
{
    async fn on_flush(&self, _cache_updates: Arc<WBUpdateIterator<DC>>) -> Result<(), DC::Error> {
        Ok(())
    }

    async fn on_flush_one(&self, _updates: &DC::Key, _update: &DC::CacheUpdate) -> Result<(), Arc<DC::Error>> {
        Ok(())
    }

    async fn on_monitor_error(&self, _error: &Arc<DC::Error>) {}

    async fn on_debug(&self, _message: &str) {}
}
