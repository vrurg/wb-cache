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

// Every model would implement CacheUpdate trait for this enum.
#[derive(Debug)]
pub enum CacheUpdates<AM>
where
    AM: ActiveModelTrait + Sized + Send + Sync + 'static,
{
    Insert(AM),
    Update(AM),
    Delete,
}

impl<AM> CacheUpdates<AM>
where
    AM: ActiveModelTrait + Sized + Send + Sync + 'static,
{
    /// Creates a new ActiveModel with the same discriminant as the current one. To an external observer, it functions
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

// Common implementation of DataController methods.
// Preconditions:
// - The db::CacheUpdates enum is used to track updates.
// - It is possible to delete database rows in batches based on a list of keys.
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
    fn delete_many_condition(dm: DeleteMany<T>, keys: Vec<Self::Key>) -> DeleteMany<T>;

    fn db_provider(&self) -> <Self as ::fieldx_plus::Child>::RcParent {
        self.parent()
    }

    #[instrument(level = "trace", skip(update_records))]
    async fn wbdc_write_back(&self, update_records: Arc<UpdateIterator<Self>>) -> Result<(), Self::Error> {
        let conn_provider = self.db_provider()?;
        let db_conn = conn_provider.db_connection()?;

        let transaction = db_conn.begin().await?;
        let mut inserts = vec![];
        let mut deletes = vec![];

        loop {
            let update_item = (*update_records).next();
            let last_loop = update_item.is_none();

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
                Self::delete_many_condition(T::delete_many(), delete_chunk)
                    .exec(&transaction)
                    .await?;
                deletes.clear();
            }

            if update_item.is_none() {
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

    #[instrument(level = "trace", skip(value))]
    async fn wbdbc_on_new<AM>(&self, _key: &Self::Key, value: &AM) -> Result<DataControllerResponse<Self>, Self::Error>
    where
        AM: Into<T::ActiveModel> + Clone + Send + Sync + 'static,
    {
        // Ok(Some(CacheUpdates::Insert(value.clone().into())))
        let op = if IMMUTABLE {
            DataControllerOp::Insert
        } else {
            DataControllerOp::Nop
        };
        Ok(wbdc_response!(op, Some(CacheUpdates::Insert(value.clone().into()))))
    }

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

        Ok(if changed {
            let op = if IMMUTABLE {
                DataControllerOp::Insert
            } else {
                DataControllerOp::Nop
            };

            wbdc_response!(
                op,
                Some(if prev_update.is_none() {
                    CacheUpdates::Update(prev_am)
                } else {
                    prev_update.unwrap().replace(prev_am)
                })
            )
        } else {
            wbdc_response!(DataControllerOp::Nop, prev_update)
        })
    }
}
