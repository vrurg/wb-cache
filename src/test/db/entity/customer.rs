use async_trait::async_trait;
use fieldx_plus::fx_plus;
use sea_orm::entity::prelude::*;
use sea_orm::Condition;
use sea_orm::DeleteMany;
use sea_orm::IntoActiveModel;
use sea_orm::QuerySelect;
use serde::Deserialize;
use serde::Serialize;

use std::fmt::Debug;
use std::fmt::Display;
use std::sync::Arc;

use crate::test::db::cache::CacheUpdates;
use crate::test::db::cache::DBProvider;
use crate::test::db::cache::DCCommon;
use crate::test::types::Result;
use crate::test::types::SimErrorAny;
use crate::types::DataControllerResponse;
use crate::update_iterator::UpdateIterator;
use crate::DataController;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "customers")]
#[serde(deny_unknown_fields)]
pub struct Model {
    #[sea_orm(primary_key)]
    #[serde(rename = "i")]
    pub id: i32,
    #[sea_orm(unique, indexed)]
    #[serde(rename = "e")]
    pub email: String,
    #[serde(rename = "f")]
    pub first_name: String,
    #[serde(rename = "l")]
    pub last_name: String,
    // The day number when the user was registered.
    #[serde(rename = "d")]
    pub registered_on: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CustomerBy {
    Id(i32),
    Email(String),
}

impl Display for CustomerBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CustomerBy::Id(id) => write!(f, "#{id}"),
            CustomerBy::Email(email) => write!(f, "{email}"),
        }
    }
}

#[fx_plus(
    child(DBCP, unwrap(or_else(SimErrorAny, super::dbcp_gone("customer manager")))),
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
    pub async fn get_by_id(&self, id: i32) -> Result<Option<Model>> {
        let parent = self.parent()?;
        let db = parent.db_connection()?;
        Ok(Entity::find_by_id(id).one(&db).await?)
    }

    pub async fn get_by_email(&self, email: &str) -> Result<Option<Model>> {
        let parent = self.parent()?;
        let db = parent.db_connection()?;
        Ok(Entity::find().filter(Column::Email.eq(email)).one(&db).await?)
    }
}

impl<DBCP> Debug for Manager<DBCP>
where
    DBCP: DBProvider,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CustomerManager")
    }
}

#[async_trait]
impl<DBCP> DataController for Manager<DBCP>
where
    DBCP: DBProvider,
{
    type CacheUpdate = CacheUpdates<ActiveModel>;
    type Error = SimErrorAny;
    type Key = CustomerBy;
    type Value = Model;

    async fn get_for_key(&self, key: &Self::Key) -> Result<Option<Self::Value>> {
        Ok(match key {
            CustomerBy::Id(id) => self.get_by_id(*id).await?,
            CustomerBy::Email(email) => self.get_by_email(email).await?,
        })
    }

    async fn get_primary_key_for(&self, key: &Self::Key) -> Result<Option<Self::Key>> {
        Ok(match key {
            CustomerBy::Id(_) => Some(key.clone()),
            // Select the ID alone. Or fail...
            CustomerBy::Email(email) => Entity::find()
                .filter(Column::Email.eq(email))
                .select_only()
                .column(Column::Id)
                .into_tuple::<i32>()
                .one(&self.parent()?.db_connection()?)
                .await?
                .map(CustomerBy::Id),
        })
    }

    fn primary_key_of(&self, value: &Self::Value) -> Self::Key {
        CustomerBy::Id(value.id)
    }

    fn secondary_keys_of(&self, value: &Self::Value) -> Vec<Self::Key> {
        vec![CustomerBy::Email(value.email.clone())]
    }

    fn is_primary(&self, key: &Self::Key) -> bool {
        matches!(key, CustomerBy::Id(_))
    }

    async fn write_back(&self, update_records: Arc<UpdateIterator<Self>>) -> Result<()> {
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

#[async_trait]
impl<DBCP> DCCommon<Entity, DBCP> for Manager<DBCP>
where
    DBCP: DBProvider,
{
    fn delete_many_condition(dm: DeleteMany<Entity>, keys: Vec<Self::Key>) -> DeleteMany<Entity> {
        let mut by_id = vec![];
        let mut by_email = vec![];
        for key in keys {
            match key {
                CustomerBy::Id(id) => by_id.push(id),
                CustomerBy::Email(email) => by_email.push(email),
            }
        }

        let mut condition = Condition::any();

        if !by_id.is_empty() {
            condition = condition.add(Column::Id.is_in(by_id));
        }
        if !by_email.is_empty() {
            condition = condition.add(Column::Email.is_in(by_email));
        }

        dm.filter(condition)
    }
}
