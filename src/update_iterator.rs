use fieldx_plus::child_build;
use fieldx_plus::fx_plus;
use tokio::sync::OwnedRwLockWriteGuard;

use crate::Cache;
use crate::DataController;

type KeyGuard<DC> = (
    <DC as DataController>::Key,
    OwnedRwLockWriteGuard<Option<<DC as DataController>::CacheUpdate>>,
);
type KeyOptGuard<DC> = (
    <DC as DataController>::Key,
    Option<OwnedRwLockWriteGuard<Option<<DC as DataController>::CacheUpdate>>>,
);

/// Iterator over updates in the cache.
///
/// This is a crucial part of the [write-back](DataController::write_back) mechanism. Despite its name reflecting
/// the primary function of the struct, it does not implement the `Iterator` trait but provides a `next()` method
/// to iterate over updates. This is because it incorporates aspects of a collection's behavior, making it more
/// than just a simple iterator.
///
/// <a id="confirmation"></a>
/// **Note** that an important part of implementing a write back method is to remember to confirm the successfully
/// processed updates. Unconfirmed ones will be put back into the update pool for later processing. Confirmation can be
/// done by calling the `confirm_all()` method or by calling `confirm()` on each item returned by the `next()` method.
/// The former approach is useful for transactional updates.
#[fx_plus(
    child(Cache<DC>, rc_strong),
    parent,
    default(off),
    sync,
    rc,
    get(off),
    builder(vis(pub(crate)))
)]
pub struct UpdateIterator<DC>
where
    DC: DataController + Send + Sync + 'static,
{
    #[fieldx(inner_mut, private, get, get_mut, builder(private))]
    unprocessed: Vec<KeyOptGuard<DC>>,

    #[fieldx(inner_mut, private, get(copy), set, builder(off))]
    next_idx: usize,

    // Collect owned guards here. When the iterator is dropped the locks are released.
    #[fieldx(inner_mut, get_mut(vis(pub(crate))), builder(off))]
    worked: Vec<KeyGuard<DC>>,
}

impl<DC> UpdateIterator<DC>
where
    DC: DataController + Send + Sync + 'static,
{
    #[inline(always)]
    fn take_back(&self, key_guard: (DC::Key, OwnedRwLockWriteGuard<Option<DC::CacheUpdate>>)) {
        if key_guard.1.is_some() {
            // If the update data is Some, then it means the data controller hasn't confirmed it yet.
            // Leaving aside the hypothesis of a bug in the DC implementation, this indicates that
            // either a transaction is in progress or there was an error while processing the update.
            // Retain the lock for later so that the DC can confirm the entire transaction at once when it is completed.
            self.worked_mut().push(key_guard);
        }
    }

    /// Confirm all updates at once. Useful for transactional updates.
    #[inline]
    pub fn confirm_all(&self) {
        for (_key, guard) in self.worked_mut().iter_mut() {
            guard.take();
        }
    }

    /// The number of update records to process.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.unprocessed().len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.unprocessed().is_empty()
    }

    /// Get the next update item to process or `None` if there are no more items left.
    ///
    /// It is possible that not all of the updates, bundled with the current iterator, will be returned by this method.
    /// This can happen due to the concurrent nature of the cache and the fact that updates can be flushed by other
    /// threads.
    pub fn next(&self) -> Option<UpdateIteratorItem<DC>> {
        let mut unprocessed = self.unprocessed_mut();
        loop {
            let next_idx = self.next_idx();
            if next_idx >= unprocessed.len() {
                return None;
            }

            let Some((key, guard)) = unprocessed.get_mut(next_idx).map(|(k, g)| (k.clone(), g.take()))
            else {
                panic!(
                    "Internal error of UpdateIterator<{}>: next update key not found at index {next_idx}",
                    std::any::type_name::<DC>()
                );
            };

            self.set_next_idx(next_idx + 1);

            let guard = if let Some(g) = guard {
                g
            }
            else if let Some(update) = self.parent().updates().get(&key).cloned() {
                // Either we're able to get a write lock immediately or we skip this update. This way two problems are
                // avoided:
                //
                // 1. Deadlock: waiting for the update lock may block the entire cache.
                // 2. A locked update is assumed to be already processed by another thread, typically as a result of
                //    flush_one call.
                let Ok(guard) = update.data.clone().try_write_owned()
                else {
                    continue;
                };
                guard
            }
            else {
                // Skip if there is no update for this key. Most likely it was already flushed.
                continue;
            };

            // If guard's content is None, it means the update was already flushed and we can skip it.
            if guard.is_some() {
                return Some(
                    child_build!(
                        self, UpdateIteratorItem<DC> {
                            key_guard: Some((key.clone(), guard)),
                        }
                    )
                    .unwrap(),
                );
            }
        }
    }

    /// Reset the iterator to the initial state to allow re-iterating over the same updates. Mostly useful for cache
    /// controller's own purposes.
    #[inline]
    pub fn reset(&self) {
        self.set_next_idx(0);
        self.worked_mut().truncate(0);
    }
}

impl<DC> UpdateIteratorBuilder<DC>
where
    DC: DataController + Send + Sync + 'static,
{
    /// Setup the iterator from a list of keys. In this case it will attemp to collect the write locks from the update
    /// records.
    pub(crate) fn keys(self, keys: Vec<DC::Key>) -> Self {
        let unprocessed = keys.into_iter().map(|key| (key, None)).collect::<Vec<_>>();
        self.unprocessed(unprocessed)
    }

    /// Setup the iterator from a single key/guard pair. This is to support single entry flushes where the guard is
    /// pre-collected.
    pub(crate) fn key_guard(self, kg: (DC::Key, OwnedRwLockWriteGuard<Option<DC::CacheUpdate>>)) -> Self {
        self.unprocessed(vec![(kg.0, Some(kg.1))])
    }
}

/// Update item provides access to the update record, its key, and allows confirming the update.
///
/// When the item is dropped, it returns itself to the iterator for post-processing.  This is particularly important
/// when transactional updates are used and confirmed using the [`confirm_all()`](UpdateIterator::confirm_all) method,
/// because that method confirms only the
/// items that were returned to the iterator.
#[fx_plus(
    child(UpdateIterator<DC>, rc_strong),
    default(off),
    sync,
)]
pub struct UpdateIteratorItem<DC>
where
    DC: DataController + Send + Sync + 'static,
{
    // Option here is to allow the Drop trait to take the guard back to the iterator.
    key_guard: Option<KeyGuard<DC>>,
}

impl<DC> UpdateIteratorItem<DC>
where
    DC: DataController + Send + Sync + 'static,
{
    /// Get the update record.
    pub fn update(&self) -> &DC::CacheUpdate {
        // The .expect must never fire because we own the exclusive lock and the iterator is checking for None before
        // returning this item.
        self.key_guard
            .as_ref()
            .expect("Internal error: guard is None")
            .1
            .as_ref()
            .expect("Internal error: update data cannot be None")
    }

    /// Get the update record's key.
    pub fn key(&self) -> &DC::Key {
        // The .expect must never fire because we own the exclusive lock and the iterator is checking for None before
        // returning this item.
        &self.key_guard.as_ref().expect("Internal error: guard is None").0
    }

    /// Confirm writing this update record to the backend.
    pub fn confirm(mut self) {
        if let Some(mut guard) = self.key_guard.take() {
            guard.1.take();
        }
        else {
            unreachable!("Internal error: guard is None");
        }
    }
}

/// Returns this item back to the iterator for post-processing.
impl<DC> Drop for UpdateIteratorItem<DC>
where
    DC: DataController + Send + Sync + 'static,
{
    fn drop(&mut self) {
        if let Some(key_guard) = self.key_guard.take() {
            self.parent().take_back(key_guard);
        }
    }
}
