use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use fieldx_plus::fx_plus;
use sea_orm::entity::prelude::*;
use sea_orm::IntoActiveModel;
use serde::Deserialize;
use serde::Serialize;

use crate::test::db::cache::CacheUpdates;
use crate::test::db::cache::DBProvider;
use crate::test::db::cache::WBDCCommon;
use crate::test::types::OrderStatus;
use crate::test::types::Result;
use crate::test::types::SimErrorAny;
use crate::types::WBDataControllerResponse;
use crate::update_iterator::WBUpdateIterator;
use crate::WBDataController;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[sea_orm(table_name = "orders")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[serde(rename = "i")]
    pub id: Uuid,
    #[serde(rename = "c")]
    pub customer_id: i32,
    #[serde(rename = "p")]
    pub product_id: i32,
    #[serde(rename = "q")]
    pub quantity: i32,
    #[serde(rename = "s")]
    pub status: OrderStatus,
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

#[fx_plus(
    child(DBCP, unwrap(or_else(SimErrorAny, super::dbcp_gone("order manager")))),
    sync,
    rc
)]
pub struct Manager<DBCP>
where
    DBCP: DBProvider, {}

impl<DBCP> Manager<DBCP>
where
    DBCP: DBProvider,
{
    pub async fn get_by_order_id(&self, order_id: Uuid) -> Result<Vec<Model>, SimErrorAny> {
        Ok(Entity::find()
            .filter(Column::Id.eq(order_id))
            .all(&self.db_provider()?.db_connection()?)
            .await?)
    }
}

impl<DBCP> Debug for Manager<DBCP>
where
    DBCP: DBProvider,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OrderManager")
    }
}

#[async_trait]
impl<DBCP> WBDataController for Manager<DBCP>
where
    DBCP: DBProvider,
{
    type CacheUpdate = CacheUpdates<ActiveModel>;
    type Error = SimErrorAny;
    type Key = Uuid;
    type Value = Model;

    async fn get_for_key(&self, id: &Self::Key) -> Result<Option<Self::Value>> {
        Ok(Entity::find_by_id(*id)
            .one(&self.db_provider()?.db_connection()?)
            .await?)
    }

    fn primary_key_of(&self, value: &Self::Value) -> Self::Key {
        value.id
    }

    async fn write_back(&self, update_records: Arc<WBUpdateIterator<Self>>) -> Result<()> {
        self.wbdc_write_back(update_records).await
    }

    async fn on_new(&self, key: &Self::Key, value: &Self::Value) -> Result<WBDataControllerResponse<Self>> {
        self.wbdbc_on_new(key, &value.clone().into_active_model()).await
    }

    async fn on_delete(
        &self,
        key: &Self::Key,
        update: Option<&CacheUpdates<ActiveModel>>,
    ) -> Result<WBDataControllerResponse<Self>> {
        self.wbdc_on_delete(key, update).await
    }

    async fn on_change(
        &self,
        key: &Self::Key,
        value: &Self::Value,
        old_value: Self::Value,
        prev_update: Option<Self::CacheUpdate>,
    ) -> Result<WBDataControllerResponse<Self>> {
        self.wbdc_on_change(key, value, old_value, prev_update).await
    }
}

impl<DBCP> WBDCCommon<Entity, DBCP, true> for Manager<DBCP>
where
    DBCP: DBProvider,
{
    fn delete_many_condition(dm: sea_orm::DeleteMany<Entity>, keys: Vec<Self::Key>) -> sea_orm::DeleteMany<Entity> {
        dm.filter(Column::Id.is_in(keys))
    }
}
