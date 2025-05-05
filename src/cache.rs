use crate::entry_selector::WBEntryKeySelector;
use crate::prelude::*;
use crate::traits::WBObserver;
use crate::update_state::WBUpdateState;

use fieldx_plus::child_build;
use fieldx_plus::fx_plus;
use moka::future::Cache;
use moka::ops::compute::CompResult;
use moka::policy::EvictionPolicy;
use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Display;
use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;
use tokio_stream::{self as stream};

pub use moka::ops::compute::Op;

macro_rules! on_event {
    ($self:ident, $method:ident($($args:tt)*)) => {
        let observers = $self.observers().await;
        if !observers.is_empty() {
            for observer in observers.iter() {
                observer.$method($($args)*).await?;
            }
        }
    };
}

// The key `K` for a secondary entry must always match the primary key,
// as it uniquely identifies the corresponding primary value in the cache.
#[derive(Clone, Debug)]
pub(crate) enum ValueState<K, V>
where
    K: Debug + Hash + Clone + Eq + Sized + Send + Sync + 'static,
    V: Debug + Clone + Send + Sync + 'static,
{
    Primary(V),
    Secondary(K),
}

impl<K, V> ValueState<K, V>
where
    K: Debug + Display + Hash + Clone + Eq + Sized + Send + Sync + 'static,
    V: Debug + Clone + Send + Sync + 'static,
{
    pub(crate) fn into_value(self) -> V {
        match self {
            Self::Primary(v) => v,
            Self::Secondary(_) => panic!("secondary doesn't have a value"),
        }
    }
}

type WBArcCache<DC> = Arc<
    Cache<<DC as WBDataController>::Key, ValueState<<DC as WBDataController>::Key, <DC as WBDataController>::Value>>,
>;
type WBUpdatesHash<DC> = HashMap<<DC as WBDataController>::Key, Arc<WBUpdateState<DC>>>;
type WBUpdaterTask<DC> = JoinHandle<Result<(), Arc<<DC as WBDataController>::Error>>>;

/// This is where all the magic happens!
///
/// ```ignore
/// let controller = MyDataController::new(host, port);
/// let cache = WBCache::builder()
///     .data_controller(controller)
///     .max_updates(1000)
///     .max_capacity(100_000)
///    .flush_interval(Duration::from_secs(10))
///    .build();
///
/// // The key type is defined by the data controller implementation of WBDataController.
/// cache.entry(key).await?
///     .and_try_compute_with(|entry| async {
///        let record = if let Some(entry) = entry {
///            modify(entry.into_value()).await?
///        }
///        else {
///           create_new().await?
///        };
///
///        Ok(Op::Put(record))
///     })
///     .await?;
/// ```
#[fx_plus(
    parent,
    new(off),
    // Need explicit `default(off)` because the field defaults are for the builder type only.
    default(off),
    sync,
    builder(
        doc("Builder object of [`WBCache`].", "", "See [`WBCache::builder()`] method."),
        method_doc("Implement builder pattern for [`WBCache`]."),
    )
)]
pub struct WBCache<DC>
where
    DC: WBDataController,
    DC::Key: Send + Sync + 'static,
    DC::Error: Send + Sync + 'static,
{
    #[fieldx(vis(pub(crate)), builder(vis(pub), required, into), get(clone))]
    data_controller: Arc<DC>,

    /// Cache name. Most useful for debugging and logging.
    #[fieldx(lock, optional, clearer, get(off))]
    name: &'static str,

    #[fieldx(get(copy), default(100))]
    max_updates: u64,

    #[fieldx(get(copy), default(10_000))]
    max_capacity: u64,

    /// The delay between two consecutive flushes. If a flush was manually requested then the timer is reset.
    #[fieldx(get(copy), set, default(Duration::from_secs(10)))]
    flush_interval: Duration,

    #[fieldx(get(copy), set, default(false))]
    delete_immediately: bool,

    #[fieldx(vis(pub(crate)), lazy, clearer(private), get(clone), builder(off))]
    cache: WBArcCache<DC>,

    // Here we keep the ensured update records, i.e. those ready to be submitted back to the data controller for processing.
    #[fieldx(private, lazy, clearer, get_mut, builder(off))]
    updates: WBUpdatesHash<DC>,

    #[fieldx(private, clearer, lock, get, set, builder(off))]
    updater_task: WBUpdaterTask<DC>,

    #[fieldx(lock, private, get(copy), set, builder(off), default(Instant::now()))]
    last_flush: Instant,

    #[fieldx(mode(async), private, lock, get, builder("_observers", private), default)]
    observers: Vec<Box<dyn WBObserver<DC>>>,
}

impl<DC> WBCache<DC>
where
    DC: WBDataController,
{
    fn build_cache(&self) -> WBArcCache<DC> {
        Arc::new(
            Cache::builder()
                .max_capacity(self.max_capacity())
                .name(self.clear_name().unwrap_or_else(|| std::any::type_name::<DC::Value>()))
                .eviction_policy(EvictionPolicy::tiny_lfu())
                .build(),
        )
    }

    fn build_updates(&self) -> HashMap<DC::Key, Arc<WBUpdateState<DC>>> {
        HashMap::new()
    }

    async fn get_primary_key_from(&self, key: &DC::Key) -> Result<Option<DC::Key>, DC::Error> {
        Ok(if let Some(v) = self.cache().get(key).await {
            Some(match v {
                ValueState::Primary(_) => key.clone(),
                ValueState::Secondary(ref k) => k.clone(),
            })
        }
        else {
            self.data_controller().get_primary_key_for(key).await?
        })
    }

    // This method is for primaries only
    pub(crate) async fn get_and_try_compute_with_primary<F, Fut>(
        &self,
        key: &DC::Key,
        f: F,
    ) -> Result<WBCompResult<DC>, DC::Error>
    where
        F: FnOnce(Option<WBEntry<DC>>) -> Fut,
        Fut: Future<Output = Result<Op<DC::Value>, DC::Error>>,
    {
        let myself = self.myself().unwrap();

        self.maybe_flush_deleted(key).await?;

        let result = self
            .cache()
            .entry(key.clone())
            .and_try_compute_with(|entry| async {
                let (wb_entry, old_v, fetched) = if let Some(entry) = entry {
                    let old_v = entry.into_value().into_value();
                    (
                        Some(WBEntry::new(&myself, key.clone(), old_v.clone())),
                        Some(old_v),
                        false,
                    )
                }
                else {
                    let old_v = myself.data_controller().get_for_key(key).await?;
                    old_v.map_or((None, None, false), |v| {
                        (Some(WBEntry::new(&myself, key.clone(), v.clone())), Some(v), true)
                    })
                };

                let op = f(wb_entry).await?;

                let op = match op {
                    Op::Remove => {
                        if let Some(ref old_v) = old_v {
                            let secondaries = myself.data_controller().secondary_keys_of(old_v);
                            // If primary gets removed all related secondaries are to be dropped out of the cache.
                            for skey in secondaries {
                                myself.cache().invalidate(&skey).await;
                            }
                        }

                        myself.on_delete(key, old_v).await?;
                        Op::Remove
                    }
                    Op::Put(v) => {
                        let v = v;
                        if let Some(old_v) = old_v {
                            myself.on_change(key, &v, old_v).await?;
                            Op::Put(ValueState::Primary(v))
                        }
                        else {
                            myself.on_new(key, v.clone()).await?;
                            Op::Nop
                        }
                    }
                    Op::Nop => {
                        // Even if user wants to do nothing we still need to put the newly fetched value into cache.
                        if fetched {
                            Op::Put(ValueState::Primary(old_v.unwrap()))
                        }
                        else {
                            Op::Nop
                        }
                    }
                };

                Result::<Op<ValueState<DC::Key, DC::Value>>, DC::Error>::Ok(op)
            })
            .await?;

        Ok(match result {
            CompResult::Inserted(e) => WBCompResult::Inserted(WBEntry::from_primary_entry(self, e)),
            CompResult::Removed(e) => WBCompResult::Removed(WBEntry::from_primary_entry(self, e)),
            CompResult::ReplacedWith(e) => WBCompResult::ReplacedWith(WBEntry::from_primary_entry(self, e)),
            CompResult::Unchanged(e) => WBCompResult::Unchanged(WBEntry::from_primary_entry(self, e)),
            CompResult::StillNone(k) => WBCompResult::StillNone(k),
        })
    }

    // This method is for secondaries only. If WBCompResult wraps a WBEntry then the entry would have secondary key and
    // the primary's value in it because this is what'd be expected by user.
    pub(crate) async fn get_and_try_compute_with_secondary<F, Fut>(
        &self,
        key: &DC::Key,
        f: F,
    ) -> Result<WBCompResult<DC>, DC::Error>
    where
        F: FnOnce(Option<WBEntry<DC>>) -> Fut,
        Fut: Future<Output = Result<Op<DC::Value>, DC::Error>>,
    {
        let myself = self.myself().unwrap();
        let mut primary_value = None;

        let result = self
            .cache()
            .entry(key.clone())
            .and_try_compute_with(|entry| async {
                let mut primary_key = if let Some(entry) = entry {
                    Some(match entry.into_value() {
                        ValueState::Secondary(k) => k,
                        _ => panic!("Non-secondary cache entry of key '{key}'"),
                    })
                }
                else {
                    self.get_primary_key_from(key).await?
                };

                let secondary_key = key.clone();

                let result = if let Some(ref pk) = primary_key {
                    self.get_and_try_compute_with_primary(pk, |entry| async move {
                        let secondary_entry = if let Some(entry) = entry {
                            Some(WBEntry::new(&myself, secondary_key, entry.value().await?.clone()))
                        }
                        else {
                            None
                        };
                        f(secondary_entry).await
                    })
                    .await?
                }
                else {
                    match f(None).await? {
                        Op::Nop | Op::Remove => WBCompResult::StillNone(Arc::new(secondary_key)),
                        Op::Put(new_value) => {
                            let pkey = myself.data_controller().primary_key_of(&new_value);
                            myself.on_new(&pkey, new_value.clone()).await?;
                            primary_key = Some(pkey);
                            WBCompResult::Inserted(WBEntry::new(&myself, secondary_key, new_value))
                        }
                    }
                };

                let op = match result {
                    WBCompResult::Inserted(v) | WBCompResult::ReplacedWith(v) | WBCompResult::Unchanged(v) => {
                        primary_value = Some(v.into_value());
                        Op::Put(ValueState::Secondary(primary_key.unwrap()))
                    }
                    WBCompResult::Removed(e) => {
                        primary_value = Some(e.into_value());
                        Op::Remove
                    }
                    WBCompResult::StillNone(_) => Op::Nop,
                };

                Result::<Op<ValueState<DC::Key, DC::Value>>, DC::Error>::Ok(op)
            })
            .await?;

        Ok(match result {
            CompResult::Inserted(_) => WBCompResult::Inserted(WBEntry::new(self, key.clone(), primary_value.unwrap())),
            CompResult::ReplacedWith(_) => {
                WBCompResult::ReplacedWith(WBEntry::new(self, key.clone(), primary_value.unwrap()))
            }
            CompResult::Unchanged(_) => {
                WBCompResult::Unchanged(WBEntry::new(self, key.clone(), primary_value.unwrap()))
            }
            CompResult::Removed(_) => WBCompResult::Removed(WBEntry::new(self, key.clone(), primary_value.unwrap())),
            CompResult::StillNone(k) => WBCompResult::StillNone(k),
        })
    }

    pub(crate) async fn get_or_try_insert_with_primary(
        &self,
        key: &DC::Key,
        init: impl Future<Output = Result<DC::Value, DC::Error>>,
    ) -> Result<WBEntry<DC>, Arc<DC::Error>> {
        self.maybe_flush_deleted(key).await?;

        let cache_entry = self
            .cache()
            .entry(key.clone())
            .or_try_insert_with(async {
                Ok(ValueState::Primary(
                    if let Some(v) = self.data_controller().get_for_key(key).await? {
                        v
                    }
                    else {
                        let new_value = init.await?;
                        self.on_new(key, new_value.clone()).await?;
                        new_value
                    },
                ))
            })
            .await?;
        Ok(WBEntry::new(self, key.clone(), cache_entry.into_value().into_value()))
    }

    pub(crate) async fn get_or_try_insert_with_secondary(
        &self,
        key: &DC::Key,
        init: impl Future<Output = Result<DC::Value, DC::Error>>,
    ) -> Result<WBEntry<DC>, Arc<DC::Error>> {
        let myself = self.myself().unwrap();
        let result = self
            .cache()
            .entry(key.clone())
            .and_try_compute_with(|entry| async {
                let primary_key = if let Some(entry) = entry {
                    let ValueState::Secondary(pkey) = entry.value()
                    else {
                        panic!("Not a secondary key: '{key}'")
                    };
                    self.get_or_try_insert_with_primary(pkey, init).await?;
                    pkey.clone()
                }
                else if let Some(pkey) = myself.data_controller().get_primary_key_for(key).await? {
                    self.get_or_try_insert_with_primary(&pkey, init).await?;
                    pkey
                }
                else {
                    let new_value = init.await?;
                    let pkey = self.data_controller().primary_key_of(&new_value);
                    self.on_new(&pkey, new_value.clone()).await?;
                    self.cache().insert(pkey.clone(), ValueState::Primary(new_value)).await;
                    pkey
                };

                Result::<_, Arc<DC::Error>>::Ok(Op::Put(ValueState::Secondary(primary_key)))
            })
            .await?;

        Ok(match result {
            CompResult::Inserted(e) | CompResult::Unchanged(e) | CompResult::ReplacedWith(e) => {
                WBEntry::new(self, key.clone(), e.into_value().into_value())
            }
            _ => panic!("Unexpected outcome of get_or_try_insert_with_secondary: {result:?}"),
        })
    }

    // This method ensures that each thread locks the entire cache only for the duration required to create a new hash
    // entry.
    pub(crate) async fn get_update_state(
        &self,
        key: &DC::Key,
        value: Option<&DC::Value>,
    ) -> Result<Arc<WBUpdateState<DC>>, DC::Error> {
        if self.updates().len() as u64 >= self.max_updates() {
            self.flush().await?;
        }
        else {
            self.check_updater_task().await;
        }

        let update_state;
        {
            let mut updates = self.updates_mut();

            let update_key = value
                .map(|v| self.data_controller().primary_key_of(v))
                .unwrap_or(key.clone());

            if !updates.contains_key(&update_key) {
                update_state = child_build!(self, WBUpdateState<DC>).unwrap();
                updates.insert(key.clone(), Arc::clone(&update_state));
            }
            else {
                update_state = updates.get(&update_key).unwrap().clone();
            }
        };

        Ok(update_state)
    }

    #[allow(dead_code)]
    #[inline]
    pub fn name(&self) -> String {
        self.cache().name().unwrap_or("<anon>").to_string()
    }

    #[inline]
    pub async fn entry(&self, key: DC::Key) -> Result<WBEntryKeySelector<DC>, DC::Error> {
        Ok(if let Some(pkey) = self.get_primary_key_from(&key).await? {
            child_build!(
                self,
                WBEntryKeySelector<DC> {
                    is_primary: pkey == key,
                    key: key,
                    primary_key: pkey,
                }
            )
        }
        else {
            child_build!(
                self,
                WBEntryKeySelector<DC> {
                    is_primary: self.data_controller().is_primary(&key),
                    key: key,
                }
            )
        }
        .unwrap())
    }

    pub async fn get(&self, key: &DC::Key) -> Result<Option<DC::Value>, DC::Error> {
        let outcome = self
            .entry(key.clone())
            .await?
            .and_try_compute_with(|_| async { Ok(Op::Nop) })
            .await?;
        Ok(match outcome {
            WBCompResult::Inserted(entry) | WBCompResult::Unchanged(entry) | WBCompResult::ReplacedWith(entry) => {
                let value = entry.into_value();
                self.on_access(key, &value).await?;
                Some(value)
            }
            WBCompResult::StillNone(_) => None,
            _ => None,
        })
    }

    #[inline]
    pub async fn insert(&self, value: DC::Value) -> Result<(), Arc<DC::Error>> {
        let key = self.data_controller().primary_key_of(&value);
        Ok(self.on_new(&key, value).await?)
    }

    #[inline]
    pub async fn delete(&self, key: &DC::Key) -> Result<(), Arc<DC::Error>> {
        // let value = self.cache().remove(key).await;
        let result = self
            .entry(key.clone())
            .await?
            .and_try_compute_with(|entry| async move { Ok(if entry.is_some() { Op::Remove } else { Op::Nop }) })
            .await?;

        match result {
            WBCompResult::Removed(entry) => self.on_delete(&entry.key().clone(), Some(entry.into_value())).await?,
            WBCompResult::StillNone(_) => (),
            _ => panic!("Impossible result of removal operation: {result:?}"),
        }

        Ok(())
    }

    #[inline]
    pub async fn invalidate(&self, key: &DC::Key) {
        let myself = self.myself().unwrap();
        self.cache()
            .entry(key.clone())
            .and_compute_with(|entry| async {
                if let Some(entry) = entry {
                    if let ValueState::Primary(value) = entry.value() {
                        for secondary in myself.data_controller().secondary_keys_of(value) {
                            myself.cache().invalidate(&secondary).await;
                        }
                    }
                }
                Op::Remove
            })
            .await;
    }

    pub async fn flush(&self) -> Result<usize, DC::Error> {
        let Some(updates) = self.clear_updates()
        else {
            return Ok(0);
        };

        let updates_count = updates.len();

        if updates_count > 0 {
            on_event!(self, on_flush());

            // Create a stream of updates to be flushed. Skip the entries where no update is present.
            let stream = stream::iter(updates.into_iter())
                .then(|(k, v)| async move { (k, v.update.write().await.take()) })
                .filter_map(|(k, u)| u.map(|u| (k, u)));

            self.data_controller().write_back(stream).await?;
            self.set_last_flush(Instant::now());
        }

        Ok(updates_count)
    }

    pub async fn flush_one(&self, key: &DC::Key) -> Result<usize, DC::Error> {
        let mut updates = self.updates_mut();
        if let Some(update) = updates.remove(key) {
            if update.has_update().await {
                let update = update.clear_update().await.unwrap();

                on_event!(self, on_flush_one(&update));

                let stream = stream::iter([(key.clone(), update)]);
                self.data_controller().write_back(stream).await?;
                return Ok(1);
            }
        }
        Ok(0)
    }

    async fn monitor_updates(&self) -> Result<(), Arc<DC::Error>> {
        loop {
            if self.updates().is_empty() {
                // Don't consume resources if no updates have been produced during the last flush interval.
                break;
            }

            let interval = self.flush_interval();
            let remaining = interval.saturating_sub(self.last_flush().elapsed());
            if remaining == Duration::ZERO {
                // When last flush took place earlier than the flush interval ago...
                self.flush().await?;
            }

            tokio::time::sleep(interval - remaining).await;
        }
        Ok(())
    }

    async fn check_updater_task(&self) {
        if self.flush_interval() != Duration::ZERO && self.updater_task().as_ref().map_or(true, |t| t.is_finished()) {
            let async_self = self.myself().unwrap();
            self.set_updater_task(tokio::spawn(async move { async_self.monitor_updates().await }));
        }
    }

    pub(crate) async fn on_new(&self, key: &DC::Key, value: DC::Value) -> Result<(), DC::Error> {
        self.get_update_state(key, Some(&value))
            .await?
            .on_new(key, value)
            .await?;
        Ok(())
    }

    pub(crate) async fn on_change(
        &self,
        key: &DC::Key,
        value: &DC::Value,
        old_val: DC::Value,
    ) -> Result<(), DC::Error> {
        self.get_update_state(key, Some(value))
            .await?
            .on_change(key, value, old_val)
            .await?;
        Ok(())
    }

    pub(crate) async fn on_access<'a>(&self, key: &DC::Key, value: &'a DC::Value) -> Result<&'a DC::Value, DC::Error> {
        self.get_update_state(key, Some(value))
            .await?
            .on_access(key, value)
            .await?;
        Ok(value)
    }

    pub(crate) async fn on_delete(&self, key: &DC::Key, value: Option<DC::Value>) -> Result<(), DC::Error> {
        self.get_update_state(key, value.as_ref()).await?.on_delete(key).await?;
        if self.delete_immediately() {
            self.maybe_flush_deleted(key).await?;
        }
        Ok(())
    }

    // If a key has been deleted but not flushed yet do it now to prevent re-reading from the backend.
    pub(crate) async fn maybe_flush_deleted(&self, key: &DC::Key) -> Result<usize, DC::Error> {
        if self.cache().contains_key(key) {
            return Ok(0);
        }
        return self.flush_one(key).await;
    }

    pub async fn close(&self) {
        let _ = self.flush().await;
        if let Some(updater) = self.clear_updater_task() {
            updater.abort();
            let _ = updater.await;
        }
        self.clear_cache();
    }
}

impl<DC> WBCacheBuilder<DC>
where
    DC: WBDataController,
{
    pub fn observer(mut self, observer: impl WBObserver<DC>) -> Self {
        if let Some(ref mut observers) = self.observers {
            observers.push(Box::new(observer));
            self
        }
        else {
            self._observers(vec![Box::new(observer)])
        }
    }
}
