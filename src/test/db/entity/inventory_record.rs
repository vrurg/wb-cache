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
use crate::test::db::cache::DCCommon;
use crate::test::types::Result;
use crate::test::types::SimErrorAny;
use crate::types::DataControllerResponse;
use crate::update_iterator::UpdateIterator;
use crate::DataController;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "inventory_records")]
#[serde(deny_unknown_fields)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[serde(rename = "p")]
    pub product_id: i32,
    #[serde(rename = "s")]
    pub stock: i64,
    #[serde(rename = "h")]
    pub handling_days: i16,
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

#[fx_plus(
    child(DBCP, unwrap(or_else(SimErrorAny, super::dbcp_gone("inventory record manager")))),
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
    pub async fn get_by_product_id(&self, product_id: i32) -> Result<Option<Model>> {
        Ok(Entity::find_by_id(product_id)
            .one(&self.db_provider()?.db_connection()?)
            .await?)
    }
}

impl<DBCP> Debug for Manager<DBCP>
where
    DBCP: DBProvider,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "InventoryRecordManager")
    }
}

#[async_trait]
impl<DBCP> DataController for Manager<DBCP>
where
    DBCP: DBProvider,
{
    type CacheUpdate = CacheUpdates<ActiveModel>;
    type Error = SimErrorAny;
    type Key = i32;
    type Value = Model;

    async fn get_for_key(&self, key: &Self::Key) -> Result<Option<Self::Value>> {
        self.get_by_product_id(*key).await
    }

    fn primary_key_of(&self, value: &Self::Value) -> Self::Key {
        value.product_id
    }

    async fn write_back(&self, update_records: Arc<UpdateIterator<Self>>) -> Result<()> {
        self.wbdc_write_back(update_records).await
    }

    async fn on_new(
        &self,
        key: &Self::Key,
        value: &Self::Value,
    ) -> Result<DataControllerResponse<Self>, Self::Error> {
        self.wbdbc_on_new(key, &value.clone().into_active_model()).await
    }

    async fn on_delete(
        &self,
        key: &Self::Key,
        update: Option<&CacheUpdates<ActiveModel>>,
    ) -> Result<DataControllerResponse<Self>> {
        self.wbdc_on_delete(key, update).await
    }

    async fn on_change(
        &self,
        key: &Self::Key,
        value: &Self::Value,
        old_value: Self::Value,
        prev_update: Option<Self::CacheUpdate>,
    ) -> Result<DataControllerResponse<Self>> {
        self.wbdc_on_change(key, value, old_value, prev_update).await
    }
}

impl<DBCP> DCCommon<Entity, DBCP, true> for Manager<DBCP>
where
    DBCP: DBProvider,
{
    fn delete_many_condition(dm: sea_orm::DeleteMany<Entity>, keys: Vec<Self::Key>) -> sea_orm::DeleteMany<Entity> {
        dm.filter(Column::ProductId.is_in(keys))
    }
}
