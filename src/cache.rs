use crate::entry_selector::EntryKeySelector;
use crate::prelude::*;
use crate::traits::Observer;
use crate::update_iterator::UpdateIterator;
use crate::update_state::UpdateState;

use fieldx_plus::child_build;
use fieldx_plus::fx_plus;
use moka::future::Cache as MokaCache;
use moka::ops::compute::CompResult as MokaCompResult;
use moka::policy::EvictionPolicy;
use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Display;
use std::future::Future;
use std::hash::Hash;
use std::sync::atomic::AtomicBool;
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

type ArcCache<DC> =
    Arc<MokaCache<<DC as DataController>::Key, ValueState<<DC as DataController>::Key, <DC as DataController>::Value>>>;
type UpdatesHash<DC> = HashMap<<DC as DataController>::Key, Arc<UpdateState<DC>>>;

/// This is where all the magic happens!
///
/// ```ignore
/// let controller = MyDataController::new(host, port);
/// let cache = Cache::builder()
///     .data_controller(controller)
///     .max_updates(1000)
///     .max_capacity(100_000)
///    .flush_interval(Duration::from_secs(10))
///    .build();
///
/// // The key type is defined by the data controller implementation of DataController.
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
        post_build(initial_setup),
        doc("Builder object of [`Cache`].", "", "See [`Cache::builder()`] method."),
        method_doc("Implement builder pattern for [`Cache`]."),
    )
)]
pub struct Cache<DC>
where
    DC: DataController,
    DC::Key: Send + Sync + 'static,
    DC::Error: Send + Sync + 'static,
{
    /// The last error that occurred within the background task.  If set, any subsequent call to a public method
    /// operating on the cache object will return this error.
    #[fieldx(
        lock,
        clearer(doc("Clears and returns the last error or `None`.")),
        get(clone),
        set(private),
        builder(off)
    )]
    error: Arc<DC::Error>,

    /// The data controller object.
    #[fieldx(vis(pub), builder(vis(pub), required, into), get(clone))]
    data_controller: Arc<DC>,

    // This is a transitive attribute that we use to get a name from the builder and then bypass it into the cache
    // object builder.
    #[fieldx(lock, private, optional, clearer, get(off), builder(vis(pub), doc("Cache name.")))]
    name: &'static str,

    /// The maximum size of the updates pool. This not a hard limit but a threshold that triggers automatic flushing
    /// by the background task.
    ///
    /// Defaults to 100.
    #[fieldx(get(copy), default(100))]
    max_updates: u64,

    /// The maximum cache capacity. This is a hard limit on the number of key entries (not records).
    /// See the crate documentation for details.
    ///
    /// Defaults to 10000.
    #[fieldx(get(copy), default(10_000))]
    max_capacity: u64,

    /// The delay between two consecutive flushes. If a flush was manually requested then the timer is reset.
    #[fieldx(get(copy), set(doc("Change the flush interval.")), default(Duration::from_secs(10)))]
    flush_interval: Duration,

    #[fieldx(vis(pub(crate)), set(private), builder(off))]
    cache: Option<ArcCache<DC>>,

    // Here we keep the ensured update records, i.e. those ready to be submitted back to the data controller for processing.
    #[fieldx(lock, vis(pub(crate)), set(private), get, get_mut, builder(off))]
    updates: UpdatesHash<DC>,

    #[fieldx(private, clearer, writer, builder(off))]
    monitor_task: JoinHandle<()>,

    /// The period of time between two consecutive checks of the cache state by the background task.
    #[fieldx(get(copy), default(10))]
    monitor_tick_duration: u64,

    #[fieldx(private, get(clone), default(Arc::new(tokio::sync::Notify::new())))]
    flush_notifier: Arc<tokio::sync::Notify>,

    #[fieldx(private, get(clone), default(Arc::new(tokio::sync::Notify::new())))]
    cleanup_notifier: Arc<tokio::sync::Notify>,

    #[fieldx(lock, private, get(copy), set, builder(off), default(Instant::now()))]
    last_flush: Instant,

    #[fieldx(mode(async), private, lock, get, builder("_observers", private), default)]
    observers: Vec<Box<dyn Observer<DC>>>,

    #[fieldx(mode(async), private, writer, set, get(copy), builder(off), default)]
    closed: bool,

    #[fieldx(builder(off), default(false.into()))]
    shutdown: AtomicBool,
}

impl<DC> Cache<DC>
where
    DC: DataController,
{
    fn initial_setup(mut self) -> Self {
        self.closed = false.into();
        self.set_updates(HashMap::with_capacity(self.max_updates() as usize));
        self.set_cache(Some(Arc::new(
            MokaCache::builder()
                .max_capacity(self.max_capacity())
                .name(self.clear_name().unwrap_or_else(|| std::any::type_name::<DC::Value>()))
                .eviction_policy(EvictionPolicy::tiny_lfu())
                .build(),
        )));

        self
    }

    fn cache(&self) -> ArcCache<DC> {
        Arc::clone(self.cache.as_ref().expect("Internal error: cache not initialized"))
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
    ) -> Result<CompResult<DC>, Arc<DC::Error>>
    where
        F: FnOnce(Option<Entry<DC>>) -> Fut,
        Fut: Future<Output = Result<Op<DC::Value>, DC::Error>>,
    {
        let myself = self.myself().unwrap();

        self.maybe_flush_one(key).await?;
        debug!("[{}] get_and_try_compute_with_primary(key: {key:?})", myself.name());

        let result = self
            .cache()
            .entry(key.clone())
            .and_try_compute_with(|entry| async {
                let (wb_entry, old_v, fetched) = if let Some(entry) = entry {
                    let old_v = entry.into_value().into_value();
                    (
                        Some(Entry::new(&myself, key.clone(), old_v.clone())),
                        Some(old_v),
                        false,
                    )
                } else {
                    let old_v = myself.data_controller().get_for_key(key).await?;
                    old_v.map_or((None, None, false), |v| {
                        (Some(Entry::new(&myself, key.clone(), v.clone())), Some(v), true)
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

                        myself.on_delete(key, old_v.map(Arc::new)).await?
                    }
                    Op::Put(v) => {
                        let v = Arc::new(v);
                        if let Some(old_v) = old_v {
                            myself.on_change(key, v, old_v).await?
                        } else {
                            myself.on_new(key, v).await?
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
            MokaCompResult::Inserted(e) => CompResult::Inserted(Entry::from_primary_entry(self, e)),
            MokaCompResult::Removed(e) => CompResult::Removed(Entry::from_primary_entry(self, e)),
            MokaCompResult::ReplacedWith(e) => CompResult::ReplacedWith(Entry::from_primary_entry(self, e)),
            MokaCompResult::Unchanged(e) => CompResult::Unchanged(Entry::from_primary_entry(self, e)),
            MokaCompResult::StillNone(k) => CompResult::StillNone(k),
        })
    }

    // This method is for secondaries only. If CompResult wraps an Entry then the entry would have secondary key and
    // the primary's value in it because this is what'd be expected by user.
    #[instrument(level = "trace", skip(self, f))]
    pub(crate) async fn get_and_try_compute_with_secondary<F, Fut>(
        &self,
        key: &DC::Key,
        f: F,
    ) -> Result<CompResult<DC>, Arc<DC::Error>>
    where
        F: FnOnce(Option<Entry<DC>>) -> Fut,
        Fut: Future<Output = Result<Op<DC::Value>, DC::Error>>,
    {
        let myself = self.myself().unwrap();
        let mut primary_value = None;

        let result = self
            .cache()
            .entry(key.clone())
            .and_try_compute_with(|entry| async {
                let primary_key = if let Some(entry) = entry {
                    Some(match entry.into_value() {
                        ValueState::Secondary(k) => k,
                        _ => panic!(
                            "Key '{key}' is submitted as a secondary but the corresponding cache entry is primary"
                        ),
                    })
                } else {
                    self.get_primary_key_from(key).await?
                };

                let secondary_key = key.clone();

                let result = if let Some(ref pkey) = primary_key {
                    self.get_and_try_compute_with_primary(pkey, |entry| async move {
                        let secondary_entry = if let Some(entry) = entry {
                            // Some(Entry::new(&myself, secondary_key, entry.value().await?.clone()))
                            // XXX Temporary!
                            Some(Entry::new(&myself, secondary_key, entry.value().await.unwrap().clone()))
                        } else {
                            None
                        };
                        f(secondary_entry).await
                    })
                    .await?
                } else {
                    // We don't know the primary key yet. Get the user's response and see what to do with it.
                    // Since the primary key will be known only if the user has provided a value it is only possible
                    // to use the get_and_try_compute_with_primary method if Op::Put is returned.
                    match f(None).await? {
                        Op::Nop | Op::Remove => CompResult::StillNone(Arc::new(secondary_key)),
                        Op::Put(new_value) => {
                            let pkey = myself.data_controller().primary_key_of(&new_value);
                            self.get_and_try_compute_with_primary(&pkey, |_| async { Ok(Op::Put(new_value)) })
                                .await?
                        }
                    }
                };

                let op = match result {
                    CompResult::Inserted(v) | CompResult::ReplacedWith(v) | CompResult::Unchanged(v) => {
                        primary_value = Some(v.into_value());
                        Op::Put(ValueState::Secondary(primary_key.unwrap()))
                    }
                    CompResult::Removed(e) => {
                        primary_value = Some(e.into_value());
                        Op::Remove
                    }
                    CompResult::StillNone(_) => Op::Nop,
                };

                Result::<Op<ValueState<DC::Key, DC::Value>>, Arc<DC::Error>>::Ok(op)
            })
            .await?;

        Ok(match result {
            MokaCompResult::Inserted(_) => CompResult::Inserted(Entry::new(self, key.clone(), primary_value.unwrap())),
            MokaCompResult::ReplacedWith(_) => {
                CompResult::ReplacedWith(Entry::new(self, key.clone(), primary_value.unwrap()))
            }
            MokaCompResult::Unchanged(_) => {
                CompResult::Unchanged(Entry::new(self, key.clone(), primary_value.unwrap()))
            }
            MokaCompResult::Removed(_) => CompResult::Removed(Entry::new(self, key.clone(), primary_value.unwrap())),
            MokaCompResult::StillNone(k) => CompResult::StillNone(k),
        })
    }

    #[instrument(level = "trace", skip(self, init))]
    pub(crate) async fn get_or_try_insert_with_primary(
        &self,
        key: &DC::Key,
        init: impl Future<Output = Result<DC::Value, DC::Error>>,
    ) -> Result<Entry<DC>, Arc<DC::Error>> {
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
                        self.on_new(key, Arc::new(new_value.clone())).await?;
                        new_value
                    },
                ))
            })
            .await?;
        Ok(Entry::new(self, key.clone(), cache_entry.into_value().into_value()))
    }

    #[instrument(level = "trace", skip(self, init))]
    pub(crate) async fn get_or_try_insert_with_secondary(
        &self,
        key: &DC::Key,
        init: impl Future<Output = Result<DC::Value, DC::Error>>,
    ) -> Result<Entry<DC>, Arc<DC::Error>> {
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
                    self.insert(new_value).await?;
                    pkey
                };

                Result::<_, Arc<DC::Error>>::Ok(Op::Put(ValueState::Secondary(primary_key)))
            })
            .await?;

        Ok(match result {
            MokaCompResult::Inserted(e) | MokaCompResult::Unchanged(e) | MokaCompResult::ReplacedWith(e) => {
                Entry::new(self, key.clone(), e.into_value().into_value())
            }
            _ => panic!("Unexpected outcome of get_or_try_insert_with_secondary: {result:?}"),
        })
    }

    // This method ensures that each thread locks the entire cache only for the duration required to create a new hash
    // entry.
    #[instrument(level = "trace", skip(self, f))]
    #[allow(clippy::type_complexity)]
    pub(crate) async fn get_update_state_and_compute<'a, F, Fut>(
        &self,
        key: &'a DC::Key,
        value: Option<Arc<DC::Value>>,
        f: F,
    ) -> Result<Op<ValueState<DC::Key, DC::Value>>, DC::Error>
    where
        F: FnOnce(&'a DC::Key, Option<Arc<DC::Value>>, Arc<UpdateState<DC>>) -> Fut,
        Fut: Future<Output = Result<DataControllerOp, DC::Error>>,
    {
        let update_state;
        self.check_task().await;
        {
            let mut updates = self.updates_mut();

            let count = updates.len();
            let current_capacity = updates.capacity();
            if current_capacity <= (count - (count / 10)) {
                updates.reserve(current_capacity.max(self.max_updates() as usize) / 2);
            }

            let update_key = value
                .as_ref()
                .map(|v| self.data_controller().primary_key_of(v))
                .unwrap_or(key.clone());

            if !updates.contains_key(&update_key) {
                update_state = child_build!(self, UpdateState<DC>).unwrap();
                updates.insert(key.clone(), Arc::clone(&update_state));
            } else {
                update_state = updates.get(&update_key).unwrap().clone();
            }
            // At this point, the update state has at least two references, which protects it from being collected by
            // the _purify_updates method.
        };

        let op = f(key, value.as_ref().map(Arc::clone), Arc::clone(&update_state)).await?;

        // There is a chance that if the update was empty when fetched, it was collected by the _purify_updates method.
        // For security reasons, we insert the update back into the pool unconditionally.
        self.updates_mut().insert(key.clone(), update_state.clone());

        self._on_dc_op(op, key, value).await
    }

    /// Cache name. Most useful for debugging and logging.
    #[allow(dead_code)]
    #[inline]
    pub fn name(&self) -> String {
        self.cache().name().unwrap_or("<anon>").to_string()
    }

    /// Returns an object that represents a key in the cache.
    #[instrument(level = "trace")]
    pub async fn entry(&self, key: DC::Key) -> Result<EntryKeySelector<DC>, Arc<DC::Error>> {
        check_error!(self);
        Ok(if let Some(pkey) = self.get_primary_key_from(&key).await? {
            child_build!(
                self,
                EntryKeySelector<DC> {
                    primary: pkey == key,
                    key: key,
                    primary_key: pkey,
                }
            )
        } else {
            child_build!(
                self,
                EntryKeySelector<DC> {
                    primary: self.data_controller().is_primary(&key),
                    key: key,
                }
            )
        }
        .unwrap())
    }

    /// Try to get a value from the cache by its key. If the value is not present, the controller attempts to fetch it
    /// via the data controller. Returns `None` if such a key does not exist.
    #[instrument(level = "trace")]
    pub async fn get(&self, key: &DC::Key) -> Result<Option<DC::Value>, Arc<DC::Error>> {
        check_error!(self);
        let outcome = self
            .entry(key.clone())
            .await?
            .and_try_compute_with(|_| async { Ok(Op::Nop) })
            .await?;

        Ok(match outcome {
            CompResult::Inserted(entry) | CompResult::Unchanged(entry) | CompResult::ReplacedWith(entry) => {
                let value = entry.into_value();
                self.on_access(key, Arc::new(value.clone())).await?;
                Some(value)
            }
            CompResult::StillNone(_) => None,
            _ => None,
        })
    }

    /// Insert a value into the cache. This operation triggers the [`on_new`](crate#on_new) data controller
    /// chain of events even in case there is already a value present for the same key.
    #[instrument(level = "trace")]
    pub async fn insert(&self, value: DC::Value) -> Result<Option<DC::Value>, Arc<DC::Error>> {
        check_error!(self);
        let key = self.data_controller().primary_key_of(&value);
        let res = self
            .cache()
            .entry(key.clone())
            .and_try_compute_with(|_| async {
                let op = self.on_new(&key, Arc::new(value)).await;
                op
            })
            .await?;

        match res {
            MokaCompResult::Inserted(entry)
            | MokaCompResult::ReplacedWith(entry)
            | MokaCompResult::Unchanged(entry) => {
                let value = entry.into_value().into_value();
                Ok(Some(value))
            }
            MokaCompResult::StillNone(_) => Ok(None),
            _ => panic!("Impossible result of insert operation: {res:?}"),
        }
    }

    /// Delete a value from the cache by its key. If the value is not cached yet, it will be fetched from the backend
    /// first. This behavior is subject to further optimization. In most cases, however, this should not be a problem
    /// because if one knows what to delete without inspecting it first or using it in other ways, then it would be more
    /// efficient to delete directly in the backend.
    #[instrument(level = "trace")]
    pub async fn delete(&self, key: &DC::Key) -> Result<Option<DC::Value>, Arc<DC::Error>> {
        check_error!(self);
        // let value = self.cache().remove(key).await;
        let result = self
            .entry(key.clone())
            .await?
            .and_try_compute_with(|entry| async move { Ok(if entry.is_some() { Op::Remove } else { Op::Nop }) })
            .await?;

        Ok(match result {
            CompResult::Removed(entry) => Some(entry.into_value()),
            CompResult::StillNone(_) => None,
            _ => panic!("Impossible result of delete operation: {result:?}"),
        })
    }

    /// Invalidate a key in the cache. A secondary key is invalidated by removing it from the cache, while a primary key
    /// is invalidated by removing it and all its secondary keys from the cache.
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
    fn _purify_updates(&self, update_iter: Arc<UpdateIterator<DC>>) -> usize {
        // First of all, ensure minimal overhead on lock acquisition. The operation itself is not going to take too long
        // and must be deadlock-free as well.
        let mut updates = self.updates_mut();
        let count = 0;
        for (key, guard) in update_iter.worked_mut().iter() {
            let update = updates.get_mut(key).unwrap();
            if Arc::strong_count(update) > 1 {
                // This update is being used somewhere else. We cannot remove it from the pool yet.
                continue;
            }

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

    /// Low-level immediate flush operation that requires a list of keys to flush.  It does not check for
    /// [error](#method.error) generated by the background task.
    pub async fn flush_many_raw(&self, keys: Vec<DC::Key>) -> Result<usize, DC::Error> {
        let update_iter = child_build!(
            self, UpdateIterator<DC> {
                keys: keys
            }
        )
        .expect("Internal error: UpdateIterator builder failure");

        wbc_event!(self, on_flush(update_iter.clone())?);

        // Allow the update iterator to be re-iterated.  Since it's a non-deterministic iterator whose outcomes depend
        // on the state of the cache update pool, its implementation supports such uncommon usage.
        update_iter.reset();

        self.data_controller().write_back(update_iter.clone()).await?;
        let updates_count = self._purify_updates(update_iter);
        self.set_last_flush(Instant::now());

        Ok(updates_count)
    }

    /// The same as the [`flush`](#method.flush) method but does not check for [error](#method.error) that may have
    /// occurred in the background task.
    #[instrument(level = "trace")]
    pub async fn flush_raw(&self) -> Result<usize, DC::Error> {
        let update_keys = {
            let updates = self.updates();
            updates.keys().cloned().collect::<Vec<_>>()
        };
        self.flush_many_raw(update_keys).await
    }

    /// Immediately flushes all updates in the pool but will do nothing and immediately error out if the background task
    /// has encountered an [error](#method.error).
    #[instrument(level = "trace")]
    pub async fn flush(&self) -> Result<usize, Arc<DC::Error>> {
        check_error!(self);
        self.flush_raw().await.map_err(Arc::new)
    }

    /// Initiates a flush via the background task.
    ///
    /// This method does not wait for the flush to complete; it returns immediately.
    /// If the background task has previously encountered an [error](#method.error),
    /// this method will do nothing and return that error.
    #[instrument(level = "trace")]
    pub async fn soft_flush(&self) -> Result<(), Arc<DC::Error>> {
        check_error!(self);
        self.flush_notifier().notify_waiters();
        Ok(())
    }

    /// Flushes a single key in the cache. Although the operation itself is inefficient from the caching perspective, it
    /// is useful in a multi-cache environment where values in one backend refer to values in another.  For example,
    /// when there is a foreign key relationship between two tables in a database, a dependent record cannot be written
    /// into the backend until its dependency is written first.
    ///
    /// This method is most useful when invoked by an [observer](crate#observers).
    ///
    /// Does nothing and returns an error if the background task has encountered an [error](#method.error).
    #[instrument(level = "trace")]
    pub async fn flush_one(&self, key: &DC::Key) -> Result<usize, Arc<DC::Error>> {
        check_error!(self);
        // Use a local variable to avoid holding the lock on the updates pool for too long.
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
                    self, UpdateIterator<DC> {
                        key_guard: (key.clone(), guard),
                    }
                )
                .expect("Internal error: UpdateIterator builder failure");

                self.data_controller().write_back(update_iter.clone()).await?;
                return Ok(self._purify_updates(update_iter));
            }
        }
        Ok(0)
    }

    #[instrument(level = "trace")]
    async fn monitor_updates(&self) {
        let flush_interval = self.flush_interval();
        let mut ticking_interval = interval(Duration::from_millis(self.monitor_tick_duration()));
        ticking_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let max_updates = self.max_updates() as usize;
        // Obtain the exclusive lock on the closed flag. This will be released when the task is finished. This
        // behavior allows the `close` method to wait for the task to finish before returning.
        let mut closed_guard = self.write_closed().await;

        loop {
            if *closed_guard {
                break;
            }

            let mut forced = false;
            let flush_notifier = self.flush_notifier();
            let cleanup_notifier = self.cleanup_notifier();
            tokio::select! {
                _ = flush_notifier.notified() => {
                    // The flush was requested manually. Reset the timer.
                    forced = true;
                }
                _ = cleanup_notifier.notified() => {
                    // The cleanup was requested manually. Reset the timer.
                    forced = true;
                }
                _ = ticking_interval.tick() => {
                    // Do nothing, just wait for the next tick.
                }
            }

            *closed_guard = self.is_shut_down();

            if self.updates().is_empty() {
                // Don't consume resources if no updates have been produced.
                continue;
            }

            if forced
                || *closed_guard
                // When the updates pool size exceeded the limit
                || self.updates().len() > max_updates
                // When last flush took place earlier than the flush interval ago...
                || self.last_flush().elapsed() >= flush_interval
            {
                if let Err(error) = self.flush().await {
                    self.set_error(error.clone());
                    wbc_event!(self, on_monitor_error(&error));
                }
            }
        }
    }

    #[instrument(level = "trace", skip(self))]
    async fn check_task(&self) {
        let mut task_guard = self.write_monitor_task();
        if task_guard.as_ref().is_none_or(|t| t.is_finished()) {
            let async_self = self.myself().unwrap();
            *task_guard = Some(tokio::spawn(async move { async_self.monitor_updates().await }));
        }
    }

    async fn _on_dc_op(
        &self,
        op: DataControllerOp,
        key: &DC::Key,
        value: Option<Arc<DC::Value>>,
    ) -> Result<Op<ValueState<DC::Key, DC::Value>>, DC::Error> {
        Ok(match op {
            DataControllerOp::Nop => Op::Nop,
            DataControllerOp::Insert => {
                if let Some(value) = value {
                    Op::Put(ValueState::Primary(value.as_ref().clone()))
                } else {
                    Op::Nop
                }
            }
            DataControllerOp::Revoke => Op::Remove,
            DataControllerOp::Drop => {
                self.updates_mut().remove(key);
                Op::Remove
            }
        })
    }

    #[instrument(level = "trace")]
    #[allow(clippy::type_complexity)]
    pub(crate) async fn on_new(
        &self,
        key: &DC::Key,
        value: Arc<DC::Value>,
    ) -> Result<Op<ValueState<DC::Key, DC::Value>>, DC::Error> {
        self.get_update_state_and_compute(key, Some(value), |key, value, update_state| async move {
            update_state.on_new(key, value.as_ref().unwrap()).await
        })
        .await
    }

    #[instrument(level = "trace")]
    #[allow(clippy::type_complexity)]
    pub(crate) async fn on_change(
        &self,
        key: &DC::Key,
        value: Arc<DC::Value>,
        old_val: DC::Value,
    ) -> Result<Op<ValueState<DC::Key, DC::Value>>, DC::Error> {
        self.get_update_state_and_compute(key, Some(value), |key, value, update_state| async move {
            update_state.on_change(key, value.unwrap(), old_val).await
        })
        .await
    }

    #[instrument(level = "trace")]
    pub(crate) async fn on_access<'a>(&self, key: &DC::Key, value: Arc<DC::Value>) -> Result<(), DC::Error> {
        self.get_update_state_and_compute(key, Some(value), |key, value, update_state| async move {
            update_state.on_access(key, value.unwrap()).await
        })
        .await?;
        Ok(())
    }

    #[instrument(level = "trace")]
    #[allow(clippy::type_complexity)]
    pub(crate) async fn on_delete(
        &self,
        key: &DC::Key,
        value: Option<Arc<DC::Value>>,
    ) -> Result<Op<ValueState<DC::Key, DC::Value>>, DC::Error> {
        self.get_update_state_and_compute(key, value, |key, _value, update_state| async move {
            update_state.on_delete(key).await
        })
        .await
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

    /// Prepares the cache for shutdown. This method will notify the background task to stop and flush all updates.
    /// It will block until the task is finished.
    ///
    /// The method is mandatory to be called before the cache is dropped to ensure that no data is lost.
    ///
    /// Will do nothing and return an error if the background task has encountered an [error](#method.error).
    #[instrument(level = "trace")]
    pub async fn close(&self) -> Result<(), Arc<DC::Error>> {
        check_error!(self);

        if self.is_shut_down() {
            return Ok(());
        }

        self.shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        self.cleanup_notifier().notify_waiters();

        // This will block until the monitor task is finished.
        let _ = self.write_closed().await;

        Ok(())
    }

    /// Returns the shutdown status of the cache.
    pub fn is_shut_down(&self) -> bool {
        self.shutdown.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl<DC> Debug for Cache<DC>
where
    DC: DataController,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("name", &self.name())
            .field("updates_count", &self.updates().len())
            .field("cache_entries", &self.cache().entry_count())
            .finish()
    }
}

impl<DC> CacheBuilder<DC>
where
    DC: DataController,
{
    /// Adds an [observer](crate#observers) to the cache.
    pub fn observer(mut self, observer: impl Observer<DC>) -> Self {
        if let Some(ref mut observers) = self.observers {
            observers.push(Box::new(observer));
            self
        } else {
            self._observers(vec![Box::new(observer)])
        }
    }
}
