use std::sync::Arc;

use async_trait::async_trait;
use fieldx_plus::fx_plus;
use sea_orm::entity::prelude::*;
use sea_orm::IntoActiveModel;
use serde::Deserialize;
use serde::Serialize;
use tokio_stream::Stream;

use crate::test::db::cache::CacheUpdates;
use crate::test::db::cache::DBConnectionProvider;
use crate::test::db::cache::WBDCCommon;
use crate::test::types::Result;
use crate::test::types::SimErrorAny;
use crate::WBDataController;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "inventory_records")]
#[serde(deny_unknown_fields)]
pub struct Model {
    #[sea_orm(primary_key)]
    #[serde(rename = "p")]
    pub product_id:    u32,
    #[serde(rename = "s")]
    pub stock:         u32,
    #[serde(rename = "h")]
    pub handling_days: u8,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::product::Entity",
        from = "Column::ProductId",
        to = "super::product::Column::Id"
    )]
    Product,
}

impl ActiveModelBehavior for ActiveModel {}

#[fx_plus(child(DBCP, rc_strong), sync, rc)]
pub struct Manager<DBCP>
where
    DBCP: DBConnectionProvider, {}

impl<DBCP> Manager<DBCP>
where
    DBCP: DBConnectionProvider,
{
    pub async fn get_by_product_id(&self, product_id: u32) -> Result<Option<Model>> {
        let db = self.db_conn_provider().db_connection().await?;
        Ok(Entity::find()
            .filter(Column::ProductId.eq(product_id))
            .one(db.as_ref())
            .await?)
    }
}

#[async_trait]
impl<DBCP> WBDataController for Manager<DBCP>
where
    DBCP: DBConnectionProvider,
{
    type CacheUpdate = CacheUpdates<ActiveModel>;
    type Error = SimErrorAny;
    type Key = u32;
    type Value = Model;

    async fn get_for_key(&self, key: &Self::Key) -> Result<Option<Self::Value>> {
        self.get_by_product_id(*key).await
    }

    fn primary_key_of(&self, value: &Self::Value) -> Self::Key {
        value.product_id
    }

    async fn write_back(
        &self,
        update_records: impl Stream<Item = (Self::Key, Self::CacheUpdate)> + Send,
    ) -> Result<(), Self::Error> {
        self.wbdc_write_back(update_records).await
    }

    async fn on_new(&self, key: &Self::Key, value: &Self::Value) -> Result<Option<Self::CacheUpdate>, Self::Error> {
        self.wbdbc_on_new(key, &value.clone().into_active_model()).await
    }

    async fn on_delete(&self, key: &Self::Key) -> Result<Option<Self::CacheUpdate>, Self::Error> {
        self.wbdc_on_delete(key).await
    }

    async fn on_access(
        &self,
        _key: &Self::Key,
        _value: &Self::Value,
        prev_update: Option<Self::CacheUpdate>,
    ) -> Result<Option<Self::CacheUpdate>> {
        Ok(prev_update)
    }

    async fn on_change(
        &self,
        key: &Self::Key,
        value: &Self::Value,
        old_value: Self::Value,
        prev_update: Option<Self::CacheUpdate>,
    ) -> Result<Option<Self::CacheUpdate>> {
        self.wbdc_on_change(key, value, old_value, prev_update).await
    }
}

impl<DBCP> WBDCCommon<Entity, DBCP> for Manager<DBCP>
where
    DBCP: DBConnectionProvider,
{
    fn delete_many_condition(
        &self,
        dm: sea_orm::DeleteMany<Entity>,
        keys: Vec<Self::Key>,
    ) -> sea_orm::DeleteMany<Entity> {
        dm.filter(Column::ProductId.is_in(keys))
    }
}
