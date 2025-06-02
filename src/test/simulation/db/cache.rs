//! Common implementation of the `DataController` methods and types for the simulation code.
use crate::prelude::*;
use crate::test::simulation::types::simerr;
use crate::test::simulation::types::Result;
use crate::test::simulation::types::SimErrorAny;
use crate::update_iterator::UpdateIterator;
use crate::update_iterator::UpdateIteratorItem;
use crate::wbdc_response;
use sea_orm::entity::Iterable;
use sea_orm::ActiveModelTrait;
use sea_orm::DatabaseConnection;
use sea_orm::DeleteMany;
use sea_orm::EntityTrait;
use sea_orm::IntoActiveModel;
use sea_orm::TransactionTrait;
use sea_orm_migration::async_trait::async_trait;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::Weak;
use tracing::instrument;

use super::driver::DatabaseDriver;

/// The type of cache controller update pool records.
#[derive(Debug)]
pub enum CacheUpdates<AM>
where
    AM: ActiveModelTrait + Sized + Send + Sync + 'static,
{
    /// The active model of this variant is to be inserted into the the database.
    Insert(AM),
    /// Database record is to be update with this active model.
    Update(AM),
    /// Delete the record from the database.
    Delete,
}

impl<AM> CacheUpdates<AM>
where
    AM: ActiveModelTrait + Sized + Send + Sync + 'static,
{
    /// Create a new ActiveModel with the same discriminant as the current one. To an external observer, it functions
    /// as replacing a container value.
    pub fn replace(&self, am: AM) -> CacheUpdates<AM> {
        match self {
            CacheUpdates::Insert(_) => CacheUpdates::Insert(am),
            CacheUpdates::Update(_) => CacheUpdates::Update(am),
            CacheUpdates::Delete => CacheUpdates::Delete,
        }
    }
}

pub trait DBProvider: Sync + Send + 'static {
    fn db_driver(&self) -> Result<Arc<impl DatabaseDriver>>;
    fn db_connection(&self) -> Result<DatabaseConnection>;
}

/// Common implementation of DataController methods.
///
/// All models in this simulation share a lot of properties. Primarily due to the use
/// of SeaORM framework. Because of this most of the `DataController` functionality
/// can be shared among them and this is what is this trait made for.
///
/// Though an attempt to provide as much information about the implementation as possible
/// in this documentation will be made, it is recommended to look into the source code
/// as some comments in the code require the context to be understood.
///
/// # Type parameters
///
/// | Parameter | Description |
/// |-----------|-------------|
/// | `T`       | The SeaORM `Entity` type that this data controller is responsible for. It must implement the [`EntityTrait`](sea_orm::entity::EntityTrait) trait.
/// | `PARENT`  | The data controller implementing `DCCommon` is expected to be a child of an entity that implements the [`DBProvider`] trait, thereby providing a database connection.
/// | `IMMUTABLE` | Setting this to `true` indicates that the database does not modify records when they are written to it; in particular, the primary key is configured with auto-increment disabled.
#[async_trait]
pub trait DCCommon<T, PARENT, const IMMUTABLE: bool = false>:
    DataController<CacheUpdate = CacheUpdates<T::ActiveModel>, Error = SimErrorAny> + Send + Debug
where
    T: EntityTrait + Send + Sync + 'static,
    T::Model: IntoActiveModel<T::ActiveModel>,
    T::Column: Iterable,
    T::ActiveModel: ActiveModelTrait + Send + Sync + 'static + From<Self::Value>,
    PARENT: DBProvider,
    Self::Value: Send + Sync + 'static,
    Self::Key: ToString + Send + Sync + 'static,
    Self: ::fieldx_plus::Child<
        WeakParent = Weak<PARENT>,
        RcParent = Result<Arc<PARENT>, Self::Error>,
        FXPParent = Weak<PARENT>,
    >,
{
    /// Provide correct condition for SeaORM's [`DeleteMany`] operation. See, for example,
    /// [`CustomerManager::delete_many_condition`](crate::test::simulation::db::entity::customer::Manager::delete_many_condition)
    /// source code.
    fn delete_many_condition(dm: DeleteMany<T>, keys: Vec<Self::Key>) -> DeleteMany<T>;

    /// Where we get our database connection from.
    fn db_provider(&self) -> <Self as ::fieldx_plus::Child>::RcParent {
        self.parent()
    }

    /// Try to send given update records to the database in the most efficient way. This task is accomplished by:
    ///
    /// - using a transaction to group all updates together and possibly avoid re-indexing overhead;
    /// - sorting updates into inserts, updates, and deletes;
    /// - batching inserts and deletes.
    ///
    /// Updates are performed on a per-record basis because they are, by nature, not batchable.
    ///
    /// To be on the safe side, the batches are limited to 1000 records each. Technically, the PostgreSQL protocol
    /// allows for up to 65,535 records in a single batch. However, practically, even half of that was causing errors.
    /// Considering that even with the limit of 1000 the simulation demonstrates an 80-100 times improvement over the
    /// non-cached approach, this is considered a good trade-off.
    #[instrument(level = "trace", skip(update_records))]
    async fn wbdc_write_back(&self, update_records: Arc<UpdateIterator<Self>>) -> Result<(), Self::Error> {
        let conn_provider = self.db_provider()?;
        let db_conn = conn_provider.db_connection()?;

        // The data safety of this method is ensured by the following critical implementation details:
        //
        // - All errors are immediately propagated.
        // - The transaction object automatically rolls back when dropped.
        // - Update records are not confirmed until the transaction is successfully committed.

        let transaction = db_conn.begin().await?;
        let mut inserts = vec![];
        let mut deletes = vec![];

        loop {
            let update_item = (*update_records).next();
            let last_loop = update_item.is_none();

            // Send inserts and deletes to the DB either when done iterating over the updates or when the batch
            // size reaches 1000 records.
            if (last_loop && !inserts.is_empty()) || inserts.len() >= 1000 {
                let am_list = inserts
                    .iter()
                    .map(|i: &UpdateIteratorItem<Self>| {
                        let CacheUpdates::<T::ActiveModel>::Insert(am) = i.update() else {
                            unreachable!("Expected insert update, but got: {:?}", i.update())
                        };
                        am
                    })
                    .cloned()
                    .collect::<Vec<_>>();

                T::insert_many(am_list).exec_without_returning(&transaction).await?;
                inserts.clear();
            }

            if (last_loop && !deletes.is_empty()) || deletes.len() >= 1000 {
                let delete_chunk = deletes
                    .iter()
                    .map(|i: &UpdateIteratorItem<Self>| {
                        let CacheUpdates::<T::ActiveModel>::Delete = i.update() else {
                            unreachable!("Expected delete update, but got: {:?}", i.update())
                        };
                        i.key()
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                // To implement correct deletion, we delegate the responsibility of determining the filter condition to
                // the data controller itself, because only the data controller knows the important specifics of the
                // model, such as which keys are to be used.
                Self::delete_many_condition(T::delete_many(), delete_chunk)
                    .exec(&transaction)
                    .await?;
                deletes.clear();
            }

            if last_loop {
                break;
            }

            let update_item = update_item.unwrap();
            let upd = update_item.update();

            match upd {
                CacheUpdates::Insert(_) => {
                    inserts.push(update_item);
                }
                CacheUpdates::Update(a) => {
                    // Updates cannot be done all at once. Do them right in place then.
                    let am: T::ActiveModel = a.clone();
                    am.update(&transaction).await?;
                }
                CacheUpdates::Delete => deletes.push(update_item),
            };
        }

        transaction.commit().await?;

        // As long as the transaction succeeded, we can confirm all updates.
        update_records.confirm_all();

        Ok(())
    }

    /// For every new data record added this method respond with [`CacheUpdates::Insert(record_active_mode)`] update.
    /// The [`DataControllerOp`] depends on the `IMMUTABLE` flag: if it is set to `true`, the operation is
    /// [`DataControllerOp::Insert`], otherwise it is [`DataControllerOp::Nop`].
    #[instrument(level = "trace", skip(value))]
    async fn wbdbc_on_new<AM>(&self, _key: &Self::Key, value: &AM) -> Result<DataControllerResponse<Self>, Self::Error>
    where
        AM: Into<T::ActiveModel> + Clone + Send + Sync + 'static,
    {
        let op = if IMMUTABLE {
            DataControllerOp::Insert
        } else {
            DataControllerOp::Nop
        };
        Ok(wbdc_response!(op, Some(CacheUpdates::Insert(value.clone().into()))))
    }

    /// Execute record deletion.
    ///
    /// A special perk of this method is that when a new record is inserted into the cache, its associated update record
    /// remains as [`CacheUpdates::Insert`] until the next flush, even if the record is later modified by the user.
    /// This means that for `IMMUTABLE` data controllers, writing the record to the backend and then deleting it has no
    /// side effects and can be safely collapsed into a single operation.  This is exactly what this method does: when
    /// it finds that the previous update state of an immutable model for the key is an insert, it simply returns a
    /// [`DataControllerOp::Drop`] operation, meaning that both data and update records
    /// are removed without having wasted time writing to the backend.
    #[instrument(level = "trace")]
    async fn wbdc_on_delete(
        &self,
        _key: &Self::Key,
        update: Option<&CacheUpdates<T::ActiveModel>>,
    ) -> Result<DataControllerResponse<Self>, Self::Error> {
        let op = if IMMUTABLE && update.is_some() && matches!(update.unwrap(), CacheUpdates::Insert(_)) {
            // The perfect case where an insert update hasn't been written to the backend yet and can be just dropped
            // altogether.
            DataControllerOp::Drop
        } else {
            // In other cases we only request for cache removal and expect the next flush to remove the data from the
            // backend.
            DataControllerOp::Revoke
        };
        Ok(wbdc_response!(op, Some(CacheUpdates::Delete)))
    }

    /// Called when a record is modified by the user.
    ///
    /// This method creates a "diff" active model representing the differences between the unmodified and modified
    /// values, where only the changed fields are set. If an update record for the key already exists, it is merged with
    /// the diff, and the resulting update record is returned.
    ///
    /// The new update record always maintains the same discriminant as the previous one unless no prior update exists.
    ///
    /// The [`DataControllerOp`] is set to [`DataControllerOp::Insert`] if the `IMMUTABLE` flag is set to `true`;
    /// otherwise, it is [`DataControllerOp::Nop`].
    #[instrument(level = "trace", skip(value, old_value, prev_update))]
    async fn wbdc_on_change(
        &self,
        key: &Self::Key,
        value: &Self::Value,
        old_value: Self::Value,
        prev_update: Option<Self::CacheUpdate>,
    ) -> Result<DataControllerResponse<Self>, Self::Error> {
        // We use the previous update state as the base when it exists. Otherwise the previous cached value would
        // implement this role.
        let mut prev_am: T::ActiveModel = if let Some(ref prev) = prev_update {
            match prev {
                CacheUpdates::Delete => Err(simerr!("Attempt to modify a previously deleted key: '{key}'"))?,
                CacheUpdates::Insert(a) => a.clone(),
                CacheUpdates::Update(a) => a.clone(),
            }
        } else {
            old_value.into()
        };

        let mut changed = false;
        let new_am: T::ActiveModel = value.clone().into();

        for c in T::Column::iter() {
            let new_v = new_am.get(c);
            if prev_am.get(c) != new_v {
                changed = true;
                if let Some(v) = new_v.into_value() {
                    prev_am.set(c, v);
                } else {
                    prev_am.not_set(c);
                }
            }
        }

        // With IMMUTABLE model we can safely put the changed data record back into the cache.
        let op = if changed && IMMUTABLE {
            DataControllerOp::Insert
        } else {
            DataControllerOp::Nop
        };

        // If there is no difference between the old and new values, then the previous update record remains
        // intact. Otherwise, we ensure that the previous update only modifies the contained active model and
        // is not mistakenly replaced with the [`CacheUpdates::Update`] variant.
        let update = if changed {
            Some(if prev_update.is_none() {
                CacheUpdates::Update(prev_am)
            } else {
                prev_update.unwrap().replace(prev_am)
            })
        } else {
            prev_update
        };

        Ok(wbdc_response!(op, update))
    }
}
