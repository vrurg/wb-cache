use std::fmt::Debug;
use std::sync::Arc;

use crate::test::db::cache::CacheUpdates;
use crate::test::db::cache::DBProvider;
use crate::test::db::cache::WBDCCommon;
use crate::test::types::Result;
use crate::test::types::SimErrorAny;
use crate::update_iterator::WBUpdateIterator;
use crate::WBDataController;

use async_trait::async_trait;
use fieldx_plus::fx_plus;
use sea_orm::entity::prelude::*;
use sea_orm::IntoActiveModel;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "products")]
#[serde(deny_unknown_fields)]
pub struct Model {
    #[sea_orm(primary_key)]
    #[serde(rename = "i")]
    pub id: i32,
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "p")]
    pub price: f64,
    #[serde(rename = "v")]
    pub views: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

// Manager for product entity
#[fx_plus(child(DBCP, rc_strong), sync, rc)]
pub struct Manager<DBCP>
where
    DBCP: DBProvider, {}

impl<DBCP> Manager<DBCP>
where
    DBCP: DBProvider,
{
    pub async fn get_by_product_id(&self, product_id: i32) -> Result<Vec<Model>> {
        Ok(Entity::find()
            .filter(Column::Id.eq(product_id))
            .all(&self.db_provider().db_connection()?)
            .await?)
    }
}

impl<DBCP> Debug for Manager<DBCP>
where
    DBCP: DBProvider,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProductManager")
    }
}

#[async_trait]
impl<DBCP> WBDataController for Manager<DBCP>
where
    DBCP: DBProvider,
{
    type CacheUpdate = CacheUpdates<ActiveModel>;
    type Error = SimErrorAny;
    type Key = i32;
    type Value = Model;

    async fn get_for_key(&self, id: &Self::Key) -> Result<Option<Self::Value>> {
        Ok(Entity::find_by_id(*id)
            .one(&self.db_provider().db_connection()?)
            .await?)
    }

    fn primary_key_of(&self, value: &Self::Value) -> Self::Key {
        value.id
    }

    async fn write_back(&self, update_records: Arc<WBUpdateIterator<Self>>) -> Result<(), Self::Error> {
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
    DBCP: DBProvider,
{
    fn delete_many_condition(dm: sea_orm::DeleteMany<Entity>, keys: Vec<Self::Key>) -> sea_orm::DeleteMany<Entity> {
        dm.filter(Column::Id.is_in(keys))
    }
}
