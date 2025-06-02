use std::fmt::Debug;
use std::sync::Arc;

use crate::test::simulation::db::cache::CacheUpdates;
use crate::test::simulation::db::cache::DBProvider;
use crate::test::simulation::db::cache::DCCommon;
use crate::test::simulation::db::driver::DatabaseDriver;
use crate::test::simulation::types::Result;
use crate::test::simulation::types::SimErrorAny;
use crate::types::DataControllerResponse;
use crate::update_iterator::UpdateIterator;
use crate::DataController;

use async_trait::async_trait;
use fieldx_plus::fx_plus;
use sea_orm::entity::prelude::*;
use sea_orm::IntoActiveModel;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "sessions")]
#[serde(deny_unknown_fields)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[serde(rename = "i")]
    pub id: i64,
    /// If the session was used to login then it will get an associated customer ID.
    #[serde(rename = "c")]
    pub customer_id: Option<i32>,
    /// On what simulation day the session must be expired.
    #[serde(rename = "e")]
    pub expires_on: i32,
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

/// The manager and the data controller for the session model.
#[fx_plus(
    child(DBCP, unwrap(or_else(SimErrorAny, super::dbcp_gone("session manager")))),
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
    /// Get session by its ID.
    pub async fn get_by_session_id(&self, session_id: i64) -> Result<Vec<Model>> {
        Ok(Entity::find()
            .filter(Column::Id.eq(session_id))
            .all(&self.db_provider()?.db_driver()?.connection())
            .await?)
    }
}

impl<DBCP> Debug for Manager<DBCP>
where
    DBCP: DBProvider,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionManager")
    }
}

#[async_trait]
impl<DBCP> DataController for Manager<DBCP>
where
    DBCP: DBProvider,
{
    type CacheUpdate = CacheUpdates<ActiveModel>;
    type Error = SimErrorAny;
    type Key = i64;
    type Value = Model;

    async fn get_for_key(&self, id: &Self::Key) -> Result<Option<Self::Value>> {
        Ok(Entity::find_by_id(*id)
            .one(&self.db_provider()?.db_connection()?)
            .await?)
    }

    fn primary_key_of(&self, value: &Self::Value) -> Self::Key {
        value.id
    }

    async fn write_back(&self, update_records: Arc<UpdateIterator<Self>>) -> Result<(), Self::Error> {
        self.wbdc_write_back(update_records).await
    }

    async fn on_new(&self, key: &Self::Key, value: &Self::Value) -> Result<DataControllerResponse<Self>, Self::Error> {
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
        dm.filter(Column::Id.is_in(keys))
    }
}
