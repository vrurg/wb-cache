use crate::prelude::*;
use crate::test::types::simerr;
use crate::test::types::Result;
use crate::test::types::SimErrorAny;
use crate::update_iterator::WBUpdateIterator;
#[cfg(feature = "log")]
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

// Every model would implement WBCacheUpdate trait for this enum.
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
    pub fn replace(&self, am: AM) -> CacheUpdates<AM> {
        match self {
            CacheUpdates::Insert(_) => CacheUpdates::Insert(am),
            CacheUpdates::Update(_) => CacheUpdates::Update(am),
            CacheUpdates::Delete => CacheUpdates::Delete,
        }
    }
}

pub trait DBProvider: Sync + Send + 'static {
    fn db_driver(&self) -> Result<&impl DatabaseDriver>;
    fn db_connection(&self) -> Result<DatabaseConnection>;
}

// Common implementation of WBDataController methods.
// Preconditions:
// - The db::CacheUpdates enum is used to track updates.
// - It is possible to delete database rows in batches based on a list of keys.
#[async_trait]
pub trait WBDCCommon<T, PARENT>:
    WBDataController<CacheUpdate = CacheUpdates<T::ActiveModel>, Error = SimErrorAny> + Send + Debug
where
    T: EntityTrait + Send + Sync + 'static,
    T::Model: IntoActiveModel<T::ActiveModel>,
    T::Column: Iterable,
    T::ActiveModel: ActiveModelTrait + Send + Sync + 'static + From<Self::Value>,
    PARENT: DBProvider,
    Self::Value: Send + Sync + 'static,
    Self::Key: ToString + Send + Sync + 'static,
    Self: ::fieldx_plus::Child<WeakParent = Weak<PARENT>, RcParent = Arc<PARENT>, FXPParent = Arc<PARENT>>,
{
    fn delete_many_condition(dm: DeleteMany<T>, keys: Vec<Self::Key>) -> DeleteMany<T>;

    fn db_provider(&self) -> <Self as ::fieldx_plus::Child>::RcParent {
        self.parent()
    }

    #[instrument(level = "trace", skip(update_records))]
    async fn wbdc_write_back(&self, update_records: Arc<WBUpdateIterator<Self>>) -> Result<(), Self::Error> {
        let conn_provider = self.db_provider();
        let db_conn = conn_provider.db_connection()?;

        let transaction = db_conn.begin().await?;
        let mut inserts = Vec::<T::ActiveModel>::new();
        let mut deletes = vec![];

        while let Some(update_item) = (*update_records).next() {
            let upd = update_item.update();
            match upd {
                CacheUpdates::Insert(a) => inserts.push(a.clone()),
                CacheUpdates::Update(a) => {
                    // Updates cannot be done all at once. Do them right in place then.
                    let am: T::ActiveModel = a.clone();
                    am.update(&transaction).await?;
                }
                CacheUpdates::Delete => deletes.push(update_item.key().clone()),
            };
        }

        if !inserts.is_empty() {
            T::insert_many(inserts)
                .exec_without_returning(&transaction)
                .await
                .inspect_err(|err| eprintln!("Error inserting records: {}", err))?;
        }

        if !deletes.is_empty() {
            Self::delete_many_condition(T::delete_many(), deletes)
                .exec(&transaction)
                .await?;
        }

        transaction.commit().await?;

        // As long as the transaction succeeded, we can confirm all updates.
        update_records.confirm_all();

        Ok(())
    }

    #[instrument(level = "trace", skip(value))]
    async fn wbdbc_on_new<AM>(&self, _key: &Self::Key, value: &AM) -> Result<Option<Self::CacheUpdate>, Self::Error>
    where
        AM: Into<T::ActiveModel> + Clone + Send + Sync + 'static,
    {
        Ok(Some(CacheUpdates::Insert(value.clone().into())))
    }

    #[instrument(level = "trace")]
    async fn wbdc_on_delete(&self, _key: &Self::Key) -> Result<Option<Self::CacheUpdate>, Self::Error> {
        Ok(Some(CacheUpdates::Delete))
    }

    #[instrument(level = "trace", skip(value, old_value, prev_update))]
    async fn wbdc_on_change(
        &self,
        key: &Self::Key,
        value: &Self::Value,
        old_value: Self::Value,
        prev_update: Option<Self::CacheUpdate>,
    ) -> Result<Option<Self::CacheUpdate>, Self::Error> {
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
            Some(if prev_update.is_none() {
                CacheUpdates::Update(prev_am)
            } else {
                prev_update.unwrap().replace(prev_am)
            })
        } else {
            prev_update
        })
    }
}
