use std::sync::Arc;

use crate::test::db::cache::CacheUpdates;
use crate::test::db::cache::DBConnectionProvider;
use crate::test::db::cache::WBDCCommon;
use crate::test::types::Result;
use crate::test::types::SimErrorAny;
use crate::WBDataController;

use async_trait::async_trait;
use fieldx_plus::fx_plus;
use sea_orm::entity::prelude::*;
use sea_orm::IntoActiveModel;
use serde::Deserialize;
use serde::Serialize;
use tokio_stream::Stream;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "sessions")]
#[serde(deny_unknown_fields)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[serde(rename = "i")]
    pub id:          i64,
    #[serde(rename = "c")]
    pub customer_id: Option<u32>,
    #[serde(rename = "e")]
    pub expires_on:  i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::customer::Entity",
        from = "Column::CustomerId",
        to = "super::customer::Column::Id"
    )]
    Customer,
}

impl ActiveModelBehavior for ActiveModel {}

// Manager for session entity
#[fx_plus(child(DBCP, rc_strong), sync, rc)]
pub struct Manager<DBCP>
where
    DBCP: DBConnectionProvider, {}

impl<DBCP> Manager<DBCP>
where
    DBCP: DBConnectionProvider,
{
    pub async fn get_by_session_id(&self, session_id: i64) -> Result<Vec<Model>> {
        let db = self.db_conn_provider().db_connection().await?;
        Ok(Entity::find()
            .filter(Column::Id.eq(session_id))
            .all(db.as_ref())
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
    type Key = i64;
    type Value = Model;

    async fn get_for_key(&self, id: &Self::Key) -> Result<Option<Self::Value>> {
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
        dm.filter(Column::Id.is_in(keys))
    }
}
