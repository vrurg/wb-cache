use sea_orm::entity::prelude::*;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "inventory_records")]
#[serde(deny_unknown_fields)]
pub struct Model {
    #[sea_orm(primary_key)]
    #[serde(rename = "p")]
    pub product_id:    u32,
    #[serde(rename = "s")]
    pub stock:         u32,
    #[serde(rename = "h")]
    pub handling_days: u8,
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
