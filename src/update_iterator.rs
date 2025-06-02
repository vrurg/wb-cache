use fieldx_plus::{child_build, fx_plus};
use tokio::sync::OwnedRwLockWriteGuard;

use crate::{Cache, DataController};

type KeyGuard<DC> = (
    <DC as DataController>::Key,
    OwnedRwLockWriteGuard<Option<<DC as DataController>::CacheUpdate>>,
);
type KeyOptGuard<DC> = (
    <DC as DataController>::Key,
    Option<OwnedRwLockWriteGuard<Option<<DC as DataController>::CacheUpdate>>>,
);

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
    pub fn confirm_all(&self) {
        for (_key, guard) in self.worked_mut().iter_mut() {
            guard.take();
        }
    }

    pub fn len(&self) -> usize {
        self.unprocessed().len()
    }

    pub fn next(&self) -> Option<UpdateIteratorItem<DC>> {
        let mut unprocessed = self.unprocessed_mut();
        loop {
            let next_idx = self.next_idx();
            if next_idx >= unprocessed.len() {
                return None;
            }

            let Some((key, guard)) = unprocessed.get_mut(next_idx).map(|(k, g)| (k.clone(), g.take())) else {
                panic!(
                    "Internal error of UpdateIterator<{}>: next update key not found at index {next_idx}",
                    std::any::type_name::<DC>()
                );
            };

            self.set_next_idx(next_idx + 1);

            let guard = if let Some(g) = guard {
                g
            } else if let Some(update) = self.parent().updates().get(&key).cloned() {
                // Either we're able to get a write lock immediately or we skip this update. This way two problems are
                // avoided:
                // 1. Deadlock: if we wait for a lock on the update, we may block the whole cache
                // 2. If the update is locked, it is likely already being processed by another thread — most likely due
                // to flush_one being called.
                let Ok(guard) = update.data.clone().try_write_owned() else {
                    continue;
                };
                guard
            } else {
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
    pub fn keys(self, keys: Vec<DC::Key>) -> Self {
        let unprocessed = keys.into_iter().map(|key| (key, None)).collect::<Vec<_>>();
        self.unprocessed(unprocessed)
    }

    /// Setup the iterator from a single key/guard pair. This is to support single entry flushes where the guard is
    /// pre-collected.
    pub fn key_guard(self, kg: (DC::Key, OwnedRwLockWriteGuard<Option<DC::CacheUpdate>>)) -> Self {
        self.unprocessed(vec![(kg.0, Some(kg.1))])
    }
}

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

    pub fn key(&self) -> &DC::Key {
        // The .expect must never fire because we own the exclusive lock and the iterator is checking for None before
        // returning this item.
        &self.key_guard.as_ref().expect("Internal error: guard is None").0
    }

    pub fn confirm(mut self) {
        if let Some(mut guard) = self.key_guard.take() {
            guard.1.take();
        } else {
            unreachable!("Internal error: guard is None");
        }
    }
}

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
