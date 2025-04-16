use sea_orm::entity::prelude::*;
use serde::Deserialize;
use serde::Serialize;

use crate::test::types::OrderStatus;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[sea_orm(table_name = "orders")]
pub struct Model {
    #[sea_orm(primary_key)]
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
