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
use crate::test::types::OrderStatus;
use crate::test::types::SimErrorAny;
use crate::WBDataController;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[sea_orm(table_name = "orders")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[serde(rename = "i")]
    pub id:           Uuid,
    #[serde(rename = "c")]
    pub customer_id:  u32,
    #[serde(rename = "p")]
    pub product_id:   u32,
    #[serde(rename = "q")]
    pub quantity:     u32,
    #[serde(rename = "s")]
    pub status:       OrderStatus,
    // The day number when the order was purchased.
    #[serde(rename = "d")]
    pub purchased_on: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::customer::Entity",
        from = "Column::CustomerId",
        to = "super::customer::Column::Id"
    )]
    Customer,
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
    pub async fn get_by_order_id(&self, order_id: Uuid) -> Result<Vec<Model>, SimErrorAny> {
        let db = self.db_conn_provider().db_connection().await?;
        Ok(Entity::find().filter(Column::Id.eq(order_id)).all(db.as_ref()).await?)
    }
}

#[async_trait]
impl<DBCP> WBDataController for Manager<DBCP>
where
    DBCP: DBConnectionProvider,
{
    type CacheUpdate = CacheUpdates<ActiveModel>;
    type Error = SimErrorAny;
    type Key = Uuid;
    type Value = Model;

    async fn get_for_key(&self, id: &Self::Key) -> Result<Option<Self::Value>, Self::Error> {
        let db = self.db_conn_provider().db_connection().await?;
        Ok(Entity::find_by_id(*id).one(db.as_ref()).await?)
    }

    fn primary_key_of(&self, value: &Self::Value) -> Self::Key {
        value.id
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
    ) -> Result<Option<Self::CacheUpdate>, Self::Error> {
        Ok(prev_update)
    }

    async fn on_change(
        &self,
        key: &Self::Key,
        value: &Self::Value,
        old_value: Self::Value,
        prev_update: Option<Self::CacheUpdate>,
    ) -> Result<Option<Self::CacheUpdate>, Self::Error> {
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
        dm.filter(Column::Id.is_in(keys))
    }
}
