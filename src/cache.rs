use crate::entry_selector::WBEntryKeySelector;
use crate::prelude::*;
use crate::traits::WBObserver;
use crate::update_iterator::WBUpdateIterator;
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
use tokio::time::interval;
use tracing::debug;
use tracing::instrument;

pub use moka::ops::compute::Op;

macro_rules! wbc_event {
    ($self:ident, $method:ident($($args:tt)*) $( $post:tt )* ) => {
        {
        let observers = $self.observers().await;
        if !observers.is_empty() {
            for observer in observers.iter() {
                observer.$method($($args)*).await $( $post )*;
            }
        }
        }
    };
}

macro_rules! check_error {
    ($self:expr) => {
        if let Some(err) = $self.clear_error() {
            return Err(err);
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
    #[fieldx(lock, clearer, get(clone), set, builder(off))]
    error: Arc<DC::Error>,

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

    #[fieldx(vis(pub(crate)), lazy, clearer(private), get(clone), builder(off))]
    cache: WBArcCache<DC>,

    // Here we keep the ensured update records, i.e. those ready to be submitted back to the data controller for processing.
    #[fieldx(vis(pub(crate)), lazy, predicate, clearer, get_mut, builder(off))]
    updates: WBUpdatesHash<DC>,

    #[fieldx(private, clearer, lock, writer, get, set, builder(off))]
    monitor_task: JoinHandle<()>,

    #[fieldx(get(copy), default(10))]
    monitor_tick_duration: u64,

    #[fieldx(private, lazy, get(clone))]
    monitor_notifier: Arc<tokio::sync::Notify>,

    #[fieldx(lock, private, get(copy), set, builder(off), default(Instant::now()))]
    last_flush: Instant,

    #[fieldx(mode(async), private, lock, get, builder("_observers", private), default)]
    observers: Vec<Box<dyn WBObserver<DC>>>,

    #[fieldx(private, lock, set, get(copy), builder(off), default)]
    shutdown: bool,
}

impl<DC> WBCache<DC>
where
    DC: WBDataController,
{
    fn build_cache(&self) -> WBArcCache<DC> {
        let myself = self.myself().unwrap();
        Arc::new(
            Cache::builder()
                .max_capacity(self.max_capacity())
                .name(self.clear_name().unwrap_or_else(|| std::any::type_name::<DC::Value>()))
                .eviction_policy(EvictionPolicy::tiny_lfu())
                .eviction_listener(move |key, _value, removal_cause| {
                    debug!("{} cache eviction: {key:?}, ({removal_cause:?})", myself.name());
                })
                .build(),
        )
    }

    fn build_updates(&self) -> HashMap<DC::Key, Arc<WBUpdateState<DC>>> {
        HashMap::with_capacity(self.max_updates() as usize)
    }

    fn build_monitor_notifier(&self) -> Arc<tokio::sync::Notify> {
        Arc::new(tokio::sync::Notify::new())
    }

    #[instrument(level = "trace", skip(self))]
    async fn get_primary_key_from(&self, key: &DC::Key) -> Result<Option<DC::Key>, DC::Error> {
        Ok(if let Some(v) = self.cache().get(key).await {
            Some(match v {
                ValueState::Primary(_) => key.clone(),
                ValueState::Secondary(ref k) => k.clone(),
            })
        } else {
            self.data_controller().get_primary_key_for(key).await?
        })
    }

    // This method is for primaries only
    #[instrument(level = "trace", skip(self, f))]
    pub(crate) async fn get_and_try_compute_with_primary<F, Fut>(
        &self,
        key: &DC::Key,
        f: F,
    ) -> Result<WBCompResult<DC>, Arc<DC::Error>>
    where
        F: FnOnce(Option<WBEntry<DC>>) -> Fut,
        Fut: Future<Output = Result<Op<DC::Value>, DC::Error>>,
    {
        let myself = self.myself().unwrap();

        debug!("get_and_try_compute_with_primary(key: {key:?})");

        self.maybe_flush_one(key).await?;

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
                } else {
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
                        } else {
                            myself.on_new(key, v.clone()).await?;
                            Op::Nop
                        }
                    }
                    Op::Nop => {
                        // Even if user wants to do nothing we still need to put the newly fetched value into cache.
                        if fetched {
                            Op::Put(ValueState::Primary(old_v.unwrap()))
                        } else {
                            Op::Nop
                        }
                    }
                };

                Result::<Op<ValueState<DC::Key, DC::Value>>, Arc<DC::Error>>::Ok(op)
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
    #[instrument(level = "trace", skip(self, f))]
    pub(crate) async fn get_and_try_compute_with_secondary<F, Fut>(
        &self,
        key: &DC::Key,
        f: F,
    ) -> Result<WBCompResult<DC>, Arc<DC::Error>>
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
                } else {
                    self.get_primary_key_from(key).await?
                };

                let secondary_key = key.clone();

                let result = if let Some(ref pk) = primary_key {
                    self.get_and_try_compute_with_primary(pk, |entry| async move {
                        let secondary_entry = if let Some(entry) = entry {
                            // Some(WBEntry::new(&myself, secondary_key, entry.value().await?.clone()))
                            // XXX Temporary!
                            Some(WBEntry::new(
                                &myself,
                                secondary_key,
                                entry.value().await.unwrap().clone(),
                            ))
                        } else {
                            None
                        };
                        f(secondary_entry).await
                    })
                    .await?
                } else {
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

                Result::<Op<ValueState<DC::Key, DC::Value>>, Arc<DC::Error>>::Ok(op)
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

    #[instrument(level = "trace", skip(self, init))]
    pub(crate) async fn get_or_try_insert_with_primary(
        &self,
        key: &DC::Key,
        init: impl Future<Output = Result<DC::Value, DC::Error>>,
    ) -> Result<WBEntry<DC>, Arc<DC::Error>> {
        debug!("get_or_try_insert_with_primary(key: {key:?})");

        self.maybe_flush_one(key).await?;

        let cache_entry = self
            .cache()
            .entry(key.clone())
            .or_try_insert_with(async {
                Ok(ValueState::Primary(
                    if let Some(v) = self.data_controller().get_for_key(key).await? {
                        v
                    } else {
                        let new_value = init.await?;
                        self.on_new(key, new_value.clone()).await?;
                        new_value
                    },
                ))
            })
            .await?;
        Ok(WBEntry::new(self, key.clone(), cache_entry.into_value().into_value()))
    }

    #[instrument(level = "trace", skip(self, init))]
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
                    let ValueState::Secondary(pkey) = entry.value() else {
                        panic!("Not a secondary key: '{key}'")
                    };
                    self.get_or_try_insert_with_primary(pkey, init).await?;
                    pkey.clone()
                } else if let Some(pkey) = myself.data_controller().get_primary_key_for(key).await? {
                    self.get_or_try_insert_with_primary(&pkey, init).await?;
                    pkey
                } else {
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
    #[instrument(level = "trace", skip(self))]
    pub(crate) async fn get_update_state(
        &self,
        key: &DC::Key,
        value: Option<&DC::Value>,
    ) -> Result<Arc<WBUpdateState<DC>>, DC::Error> {
        let update_state;
        self.check_monitor_task().await;
        {
            let mut updates = self.updates_mut();

            let count = updates.len();
            let current_capacity = updates.capacity();
            if current_capacity <= (count - (count / 10)) {
                updates.reserve(current_capacity.max(self.max_updates() as usize) / 2);
            }

            let update_key = value
                .map(|v| self.data_controller().primary_key_of(v))
                .unwrap_or(key.clone());

            if !updates.contains_key(&update_key) {
                update_state = child_build!(self, WBUpdateState<DC>).unwrap();
                updates.insert(key.clone(), Arc::clone(&update_state));
            } else {
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

    #[instrument(level = "trace")]
    pub async fn entry(&self, key: DC::Key) -> Result<WBEntryKeySelector<DC>, Arc<DC::Error>> {
        check_error!(self);
        Ok(if let Some(pkey) = self.get_primary_key_from(&key).await? {
            child_build!(
                self,
                WBEntryKeySelector<DC> {
                    is_primary: pkey == key,
                    key: key,
                    primary_key: pkey,
                }
            )
        } else {
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

    #[instrument(level = "trace")]
    pub async fn get(&self, key: &DC::Key) -> Result<Option<DC::Value>, Arc<DC::Error>> {
        check_error!(self);
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

    #[instrument(level = "trace")]
    pub async fn insert(&self, value: DC::Value) -> Result<(), Arc<DC::Error>> {
        check_error!(self);
        let key = self.data_controller().primary_key_of(&value);
        Ok(self.on_new(&key, value).await?)
    }

    #[instrument(level = "trace")]
    pub async fn delete(&self, key: &DC::Key) -> Result<(), Arc<DC::Error>> {
        check_error!(self);
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

    #[instrument(level = "trace")]
    pub async fn invalidate(&self, key: &DC::Key) -> Result<(), Arc<DC::Error>> {
        check_error!(self);
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
        Ok(())
    }

    // Remove succesfully written updates from the pool.
    fn _clear_updates(&self, update_iter: Arc<WBUpdateIterator<DC>>) -> usize {
        // First of all, ensure minimal overhead on lock acquisition. The operation itself is not going to take too long
        // and must be deadlock-free as well.
        let mut updates = self.updates_mut();
        let count = 0;
        for (key, guard) in update_iter.worked_mut().iter() {
            // If the update entry has no update in it then it's been written and we can remove it from the pool.
            if guard.is_none() {
                updates.remove(key);
            }
        }
        // At this point the update iterator would be finally dropped and all the locks on update records that are still
        // present in the pool would be released.

        // Return the number of updates that have been actually flushed.
        count
    }

    pub async fn flush_many_raw(&self, keys: Vec<DC::Key>) -> Result<usize, DC::Error> {
        let update_iter = child_build!(
            self, WBUpdateIterator<DC> {
                keys: keys
            }
        )
        .expect("Internal error: WBUpdateIterator builder failure");

        wbc_event!(self, on_flush(update_iter.clone())?);

        // Allow the update iterator to be re-iterated.  Since it's a non-deterministic iterator whose outcomes depend
        // on the state of the cache update pool, its implementation supports such uncommon usage.
        update_iter.reset();

        self.data_controller().write_back(update_iter.clone()).await?;
        let updates_count = self._clear_updates(update_iter);
        self.set_last_flush(Instant::now());

        Ok(updates_count)
    }

    #[instrument(level = "trace")]
    pub async fn flush_raw(&self) -> Result<usize, DC::Error> {
        let update_keys = {
            let updates = self.updates();
            updates.keys().cloned().collect::<Vec<_>>()
        };
        self.flush_many_raw(update_keys).await
    }

    #[instrument(level = "trace")]
    pub async fn flush(&self) -> Result<usize, Arc<DC::Error>> {
        check_error!(self);
        self.flush_raw().await.map_err(Arc::new)
    }

    #[instrument(level = "trace")]
    pub async fn soft_flush(&self) -> Result<(), Arc<DC::Error>> {
        check_error!(self);
        self.monitor_notifier().notify_waiters();
        Ok(())
    }

    #[instrument(level = "trace")]
    pub async fn flush_one(&self, key: &DC::Key) -> Result<usize, Arc<DC::Error>> {
        check_error!(self);
        let update = self.updates().get(key).cloned();
        if let Some(update) = update {
            // At this point we have two options if the update is not empty:
            // 1. There is no lock on the update data. This either means that no processing is currently being done by
            //    the background task, or the update is queued for processing but the data controller hasn't gotten to
            //    it yet. In the latter case, the race could still be won by this call. The update iterator
            //    implementation will simply skip this record when it gets to it.
            // 2. There is a lock on the data. In this case, we must wait until it is released before proceeding
            //    further. When the lock is released, the update is likely to be empty. If it is not empty, then there
            //    was an error in the data controller. We can give it another try, and if it fails again, report the
            //    error back to the caller.

            let guard = update.data.clone().write_owned().await;

            // If the update is empty, we can safely skip it.
            if let Some(update_data) = guard.as_ref() {
                wbc_event!(self, on_flush_one(key, update_data)?);

                debug!("flush single key: {key:?}");

                let update_iter = child_build!(
                    self, WBUpdateIterator<DC> {
                        key_guard: (key.clone(), guard),
                    }
                )
                .expect("Internal error: WBUpdateIterator builder failure");

                self.data_controller().write_back(update_iter.clone()).await?;
                return Ok(self._clear_updates(update_iter));
            }
        }
        Ok(0)
    }

    #[instrument(level = "trace")]
    async fn monitor_updates(&self) {
        let flush_interval = self.flush_interval();
        let mut ticking_interval = interval(Duration::from_millis(self.monitor_tick_duration()));
        let max_updates = self.max_updates() as usize;

        loop {
            if self.shutdown() {
                // The cache is shutting down. No need to flush anything.
                return;
            }

            let mut forced = false;
            let notifier = self.monitor_notifier();
            tokio::select! {
                _ = notifier.notified() => {
                    // The flush was requested manually. Reset the timer.
                    ticking_interval.reset();
                    forced = true;
                }
                _ = ticking_interval.tick() => {
                    // Do nothing, just wait for the next tick.
                }
            }

            if self.updates().is_empty() {
                // Don't consume resources if no updates have been produced during the last flush interval.
                break;
            }

            if forced || self.updates().len() > max_updates || self.last_flush().elapsed() >= flush_interval {
                // When last flush took place earlier than the flush interval ago...
                if let Err(error) = self.flush().await {
                    self.set_error(error.clone());
                    wbc_event!(self, on_monitor_error(&error));
                }
            }
        }
    }

    #[instrument(level = "trace", skip(self))]
    async fn check_monitor_task(&self) {
        if self.shutdown() {
            return;
        }

        let mut task_guard = self.write_monitor_task();
        if self.flush_interval() != Duration::ZERO && task_guard.as_ref().map_or(true, |t| t.is_finished()) {
            let async_self = self.myself().unwrap();
            *task_guard = Some(tokio::spawn(async move { async_self.monitor_updates().await }));
        }
    }

    async fn _on_op(&self, op: WBDataControllerOp, key: &DC::Key, value: Option<DC::Value>) -> Result<(), DC::Error> {
        match op {
            WBDataControllerOp::Nop => (),
            WBDataControllerOp::Insert => {
                if let Some(value) = value {
                    self.cache().insert(key.clone(), ValueState::Primary(value)).await;
                }
            }
            WBDataControllerOp::Revoke => {
                self.cache().invalidate(key).await;
            }
            WBDataControllerOp::Drop => {
                self.cache().invalidate(key).await;
                self.updates_mut().remove(key);
            }
        }

        Ok(())
    }

    #[instrument(level = "trace")]
    pub(crate) async fn on_new(&self, key: &DC::Key, value: DC::Value) -> Result<(), DC::Error> {
        let op = self
            .get_update_state(key, Some(&value))
            .await?
            .on_new(key, &value)
            .await?;

        debug!("on_new(key: {key:?}, value: {value:?}) -> {op:?}");
        self._on_op(op, key, Some(value)).await?;

        Ok(())
    }

    #[instrument(level = "trace")]
    pub(crate) async fn on_change(
        &self,
        key: &DC::Key,
        value: &DC::Value,
        old_val: DC::Value,
    ) -> Result<(), Arc<DC::Error>> {
        let op = self
            .get_update_state(key, Some(value))
            .await?
            .on_change(key, value, old_val)
            .await?;

        self._on_op(op, key, Some(value.clone())).await?;

        Ok(())
    }

    #[instrument(level = "trace")]
    pub(crate) async fn on_access<'a>(
        &self,
        key: &DC::Key,
        value: &'a DC::Value,
    ) -> Result<&'a DC::Value, Arc<DC::Error>> {
        let op = self
            .get_update_state(key, Some(value))
            .await?
            .on_access(key, value)
            .await?;

        self._on_op(op, key, Some(value.clone())).await?;

        Ok(value)
    }

    #[instrument(level = "trace")]
    pub(crate) async fn on_delete(&self, key: &DC::Key, value: Option<DC::Value>) -> Result<(), Arc<DC::Error>> {
        let op = self.get_update_state(key, value.as_ref()).await?.on_delete(key).await?;
        self._on_op(op, key, value).await?;
        Ok(())
    }

    // If a key is in the updates but not in the cache, it means that to obtain its valid value, we need to flush it
    // first.
    #[instrument(level = "trace")]
    pub(crate) async fn maybe_flush_one(&self, key: &DC::Key) -> Result<usize, Arc<DC::Error>> {
        if self.cache().contains_key(key) {
            return Ok(0);
        }
        debug!("DO flush_one(key: {key:?})");
        return self.flush_one(key).await;
    }

    #[instrument(level = "trace")]
    pub async fn close(&self) -> Result<(), Arc<DC::Error>> {
        check_error!(self);
        self.set_shutdown(true);
        if let Some(monitor) = self.clear_monitor_task() {
            self.monitor_notifier().notify_waiters();
            tokio::pin!(monitor);
            tokio::select! {
                _ = &mut monitor => (),
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    wbc_event!(self, on_debug(&format!("[{}] Monitor task didn't finish in time.", self.name())));
                    let _ = &mut monitor.abort();
                }
            }
        }
        let _ = self.flush().await;
        self.clear_cache();
        Ok(())
    }
}

impl<DC> Debug for WBCache<DC>
where
    DC: WBDataController,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WBCache")
            .field("name", &self.name())
            .field("updates_count", &self.updates().len())
            .field("cache_entries", &self.cache().entry_count())
            .finish()
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
        } else {
            self._observers(vec![Box::new(observer)])
        }
    }
}
