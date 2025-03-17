use async_trait::async_trait;
use std::{
    fmt::{Debug, Display},
    hash::Hash,
};

// For types that are in charge of reading/writing records.
#[async_trait]
pub trait WBDataController: Sized + Send + Sync + 'static {
    type Key: Debug + Display + Hash + Clone + Eq + Sized + Send + Sync + 'static;
    type Value: Debug + Clone + Send + Sync + 'static;
    type CacheUpdate: Debug + Send + Sync + 'static;
    type Error: Display + Send + Sync + 'static;

    async fn get_for_key(&self, key: &Self::Key) -> Result<Option<Self::Value>, Self::Error>;
    // In the future the method might return a list of instructions for the cache on how to act upon modified keys.
    async fn write_back(
        &self,
        updates: impl Iterator<Item = (Self::Key, Self::CacheUpdate)> + Send,
    ) -> Result<(), Self::Error>;
    async fn on_new(&self, key: &Self::Key, value: &Self::Value) -> Result<Option<Self::CacheUpdate>, Self::Error>;
    async fn on_delete(&self, key: &Self::Key) -> Result<Option<Self::CacheUpdate>, Self::Error>;
    async fn on_change(
        &self,
        key: &Self::Key,
        value: &Self::Value,
        old_value: Self::Value,
        prev_handler: Option<Self::CacheUpdate>,
    ) -> Result<Option<Self::CacheUpdate>, Self::Error>;

    // The following implementations cover plain, non-enum, key.

    // Take any key and return corresponding primary.
    async fn get_primary_key_for(&self, key: &Self::Key) -> Result<Option<Self::Key>, Self::Error> {
        Ok(Some(key.clone()))
    }

    fn primary_key_of(&self, key: &Self::Key, _value: &Self::Value) -> Self::Key {
        key.clone()
    }

    fn secondary_keys_of(&self, _key: &Self::Key, _value: &Self::Value) -> Vec<Self::Key> {
        vec![]
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
