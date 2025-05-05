use crate::prelude::*;
use crate::test::types::simerr;
use crate::test::types::Result;
use crate::test::types::SimErrorAny;
use sea_orm::entity::Iterable;
use sea_orm::ActiveModelTrait;
use sea_orm::DatabaseConnection;
use sea_orm::DeleteMany;
use sea_orm::EntityTrait;
use sea_orm::IntoActiveModel;
use sea_orm::TransactionTrait;
use sea_orm_migration::async_trait::async_trait;
use std::sync::Arc;
use std::sync::Weak;
use tokio_stream::Stream;
use tokio_stream::StreamExt;

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

#[async_trait]
pub trait DBConnectionProvider: Sync + Send + 'static {
    async fn db_connection(&self) -> Result<&DatabaseConnection>;
}

// Common implementation of WBDataController methods.
// Preconditions:
// - The db::CacheUpdates enum is used to track updates.
// - It is possible to delete database rows in batches based on a list of keys.
#[async_trait]
pub trait WBDCCommon<T, PARENT>:
    WBDataController<CacheUpdate = CacheUpdates<T::ActiveModel>, Error = SimErrorAny>
where
    T: EntityTrait + Send + Sync + 'static,
    T::Model: IntoActiveModel<T::ActiveModel>,
    T::Column: Iterable,
    T::ActiveModel: ActiveModelTrait + Send + Sync + 'static + From<Self::Value>,
    PARENT: DBConnectionProvider,
    Self::Value: Send + Sync + 'static,
    Self::Key: ToString + Send + Sync + 'static,
    Self: ::fieldx_plus::Child<WeakParent = Weak<PARENT>, RcParent = Arc<PARENT>, FXPParent = Arc<PARENT>>,
{
    fn delete_many_condition(&self, dm: DeleteMany<T>, keys: Vec<Self::Key>) -> DeleteMany<T>;

    fn db_conn_provider(&self) -> <Self as ::fieldx_plus::Child>::RcParent {
        self.parent()
    }

    async fn wbdc_write_back(
        &self,
        update_records: impl Stream<Item = (Self::Key, Self::CacheUpdate)> + Send,
    ) -> Result<(), Self::Error> {
        let mut inserts = vec![];
        let mut deletes = vec![];

        let conn_provider = self.db_conn_provider();
        let db_conn = conn_provider.db_connection().await?;

        let transaction = db_conn
            .begin()
            .await
            .inspect_err(|e| eprintln!("Failed to start transaction: {e:?}"))?;

        tokio::pin!(update_records);

        while let Some((key, upd)) = update_records.next().await {
            match upd {
                CacheUpdates::Insert(a) => inserts.push(a),
                CacheUpdates::Update(a) => {
                    // Updates cannot be done all at once. Do them right in place then.
                    let a_copy = a.clone();
                    a.update(&transaction)
                        .await
                        .inspect_err(|e| eprintln!("Failed to update record: {e:?}\n{a_copy:#?}"))?;
                }
                CacheUpdates::Delete => deletes.push(key),
            };
        }

        if !inserts.is_empty() {
            T::insert_many(inserts).exec(&transaction).await?;
        }

        if !deletes.is_empty() {
            self.delete_many_condition(T::delete_many(), deletes)
                .exec(&transaction)
                .await?;
        }

        transaction
            .commit()
            .await
            .inspect_err(|err| eprintln!("Failed to commit transaction: {err:?}"))?;

        Ok(())
    }

    async fn wbdbc_on_new<AM>(&self, _key: &Self::Key, value: &AM) -> Result<Option<Self::CacheUpdate>, Self::Error>
    where
        AM: Into<T::ActiveModel> + Clone + Send + Sync + 'static,
    {
        Ok(Some(CacheUpdates::Insert(value.clone().into())))
    }

    async fn wbdc_on_delete(&self, _key: &Self::Key) -> Result<Option<Self::CacheUpdate>, Self::Error> {
        Ok(Some(CacheUpdates::Delete))
    }

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
        }
        else {
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
                }
                else {
                    prev_am.not_set(c);
                }
            }
        }

        Ok(if changed {
            Some(if prev_update.is_none() {
                CacheUpdates::Update(prev_am)
            }
            else {
                prev_update.unwrap().replace(prev_am)
            })
        }
        else {
            prev_update
        })
    }
}
