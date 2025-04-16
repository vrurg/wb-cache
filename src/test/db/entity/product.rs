use sea_orm::entity::prelude::*;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "products")]
#[serde(deny_unknown_fields)]
pub struct Model {
    #[sea_orm(primary_key)]
    #[serde(rename = "i")]
    pub id:    u32,
    #[serde(rename = "n")]
    pub name:  String,
    #[serde(rename = "p")]
    pub price: f64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
