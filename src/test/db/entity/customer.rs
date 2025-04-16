use sea_orm::entity::prelude::*;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "customers")]
#[serde(deny_unknown_fields)]
pub struct Model {
    #[sea_orm(primary_key)]
    #[serde(rename = "i")]
    pub id:            u32,
    #[sea_orm(unique)]
    #[serde(rename = "e")]
    pub email:         String,
    #[serde(rename = "f")]
    pub first_name:    String,
    #[serde(rename = "l")]
    pub last_name:     String,
    // The day number when the user was registered.
    #[serde(rename = "d")]
    pub registered_on: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
