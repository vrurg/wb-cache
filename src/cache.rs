use crate::{entry_selector::WBEntryKeySelector, prelude::*, update_state::WBUpdateState};
use fieldx_plus::{child_build, fx_plus};
use moka::{
    future::Cache,
    ops::compute::{CompResult, Op},
    policy::EvictionPolicy,
};
use std::{
    collections::{hash_map, HashMap},
    fmt::{Debug, Display},
    future::Future,
    hash::Hash,
    sync::Arc,
    time::{Duration, Instant},
};

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
    no_new,
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
    cache: Arc<Cache<DC::Key, ValueState<DC::Key, DC::Value>>>,

    // Here we keep the ensured update records, i.e. those ready to be submitted back to the data controller for processing.
    #[fieldx(private, lazy, clearer, get_mut, builder(off))]
    updates: HashMap<DC::Key, Arc<WBUpdateState<DC>>>,

    #[fieldx(private, clearer, lock, get, set, builder(off))]
    updater_task: tokio::task::JoinHandle<Result<(), Arc<DC::Error>>>,

    #[fieldx(lock, private, get(copy), set, builder(off), default(Instant::now()))]
    last_flush: Instant,
}

impl<DC> WBCache<DC>
where
    DC: WBDataController,
{
    fn build_cache(&self) -> Arc<Cache<DC::Key, ValueState<DC::Key, DC::Value>>> {
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
            self.data_controller().get_primary_key_for(&key).await?
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

        self.maybe_flush_key(key).await?;

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
                            let secondaries = myself.data_controller().secondary_keys_of(key, old_v);
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
                            myself.on_change(key, &v, old_v).await?
                        }
                        else {
                            myself.on_new(key, v.clone()).await?
                        }

                        Op::Put(ValueState::Primary(v))
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
                            let pkey = myself.data_controller().primary_key_of(&secondary_key, &new_value);
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
        self.maybe_flush_key(key).await?;

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
                    let pkey = self.data_controller().primary_key_of(key, &new_value);
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

    // This method allows for each individual thread to block the entire cache for no longer than it takes to create a
    // new hash entry.
    pub(crate) async fn get_update_state(
        &self,
        key: &DC::Key,
        value: Option<&DC::Value>,
    ) -> Result<Arc<WBUpdateState<DC>>, DC::Error> {
        // log::debug!("[{}] get_update_state({key})", self.name());
        let update_state;
        {
            let mut updates = self.updates_mut();

            let update_key = value
                .map(|v| self.data_controller().primary_key_of(key, v))
                .unwrap_or(key.clone());

            if !updates.contains_key(&update_key) {
                update_state = child_build!(self, WBUpdateState<DC>).unwrap();
                updates.insert(key.clone(), Arc::clone(&update_state));
            }
            else {
                update_state = updates.get(&update_key).unwrap().clone();
            }
        };

        if self.updates().len() as u64 >= self.max_updates() {
            self.flush().await?;
        }
        else {
            self.check_updater_task().await;
        }

        // log::debug!("[{}] get_update_state({key}) done", self.name());
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
        log::debug!("[{}] GET({key})", self.name());

        let outcome = self
            .entry(key.clone())
            .await?
            .and_try_compute_with(|_| async { Ok(Op::Nop) })
            .await?;
        Ok(match outcome {
            WBCompResult::Inserted(entry) | WBCompResult::Unchanged(entry) | WBCompResult::ReplacedWith(entry) => {
                // log::debug!("Key '{key}' found or inserted");
                let value = entry.into_value();
                self.on_access(&key, &value).await?;
                Some(value)
            }
            WBCompResult::StillNone(_) => None,
            _ => None,
        })
    }

    #[inline]
    pub async fn insert(&self, key: DC::Key, value: DC::Value) -> Result<(), Arc<DC::Error>> {
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
        // Ok(self.on_delete(key, value).await?)
        Ok(match result {
            WBCompResult::Removed(entry) => self.on_delete(&entry.key().clone(), Some(entry.into_value())).await?,
            WBCompResult::StillNone(_) => (),
            _ => panic!("Impossible result of removal operation: {result:?}"),
        })
    }

    #[inline]
    pub async fn invalidate(&self, key: &DC::Key) {
        let myself = self.myself().unwrap();
        self.cache()
            .entry(key.clone())
            .and_compute_with(|entry| async {
                if let Some(entry) = entry {
                    if let ValueState::Primary(value) = entry.value() {
                        for secondary in myself.data_controller().secondary_keys_of(key, value) {
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

        log::info!("Flushing {} cache; count={}", self.name(), updates_count);

        self.data_controller()
            .write_back(
                updates
                    .into_iter()
                    .filter_map(|(k, v)| v.clear_update().and_then(|u| Some((k, u)))),
            )
            .await?;

        self.set_last_flush(Instant::now());

        Ok(updates_count)
    }

    async fn monitor_updates(&self) -> Result<(), Arc<DC::Error>> {
        log::debug!("[{}] Starting updates monitor", self.name());
        loop {
            // log::debug!("[{}] Entering monitor loop.", self.name());
            if self.updates().is_empty() {
                log::debug!(
                    "[{}] Stopping monitoring task since no updates have been generated.",
                    self.name()
                );
                // Don't take resourses if no updates have been produced during the last flush interval.
                break;
            }

            let interval = self.flush_interval();
            let remaining = interval.saturating_sub(self.last_flush().elapsed());
            if remaining == Duration::ZERO {
                // When last flush took place earlier than the flush interval ago...
                self.flush().await?;
                log::debug!("[{}] Cache flushed by timeout.", self.name());
            }

            tokio::time::sleep(interval - remaining).await;
        }
        Ok(())
    }

    async fn check_updater_task(&self) {
        // log::debug!(
        //     "[{}] Checking updater. (some? {}, finished? {})",
        //     self.name(),
        //     self.updater_task().is_some(),
        //     self.updater_task().as_ref().map_or(false, |t| t.is_finished())
        // );
        if self.updater_task().as_ref().map_or(true, |t| t.is_finished()) {
            // log::debug!("[{}] Starting updater.", self.name());
            let async_self = self.myself().unwrap();
            self.set_updater_task(tokio::spawn(async move { async_self.monitor_updates().await }));
        }
        else {
            // log::debug!("[{}] Updater is alive.", self.name());
        }
    }

    pub(crate) async fn on_new<'a>(&self, key: &DC::Key, value: DC::Value) -> Result<(), DC::Error> {
        self.get_update_state(&key, Some(&value))
            .await?
            .on_new(key, value)
            .await?;
        Ok(())
    }

    pub(crate) async fn on_change<'a>(
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
        log::debug!("[{}] ACCESS({key})", self.name());
        self.get_update_state(key, Some(value))
            .await?
            .on_access(key, value)
            .await?;
        Ok(value)
    }

    pub(crate) async fn on_delete(&self, key: &DC::Key, value: Option<DC::Value>) -> Result<(), DC::Error> {
        self.get_update_state(key, value.as_ref()).await?.on_delete(key).await?;
        if self.delete_immediately() {
            self.maybe_flush_key(key).await?;
        }
        Ok(())
    }

    // If a key has been deleted but not flushed yet do it now to prevent re-reading from the backend.
    pub(crate) async fn maybe_flush_key(&self, key: &DC::Key) -> Result<usize, DC::Error> {
        let maybe_update = {
            // Localize the lock lexically so we keep it no longer than necessary; i.e. as long as it takes to possibly
            // remove an entry from from the buffer if it matches the criteria.
            let mut guard = self.updates_mut();
            if let hash_map::Entry::Occupied(update_entry) = guard.entry(key.clone()) {
                let update = update_entry.get();
                if update.has_update() && update.is_delete() {
                    Some(update_entry.remove_entry().1)
                }
                else {
                    None
                }
            }
            else {
                None
            }
        };

        if let Some(update) = maybe_update {
            // log::debug!("[{}] There is a 'delete' update entry for key '{key}'", self.name(),);
            self.data_controller()
                .write_back([(key.clone(), update.clear_update().unwrap())].into_iter())
                .await?;
            return Ok(1);
        }
        Ok(0)
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
