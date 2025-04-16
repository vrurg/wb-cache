use sea_orm::DeriveActiveEnum;
use sea_orm::EnumIter;
use serde::Deserialize;
use serde::Serialize;

use super::scriptwriter::steps::Step;

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "order_status")]
pub enum OrderStatus {
    #[sea_orm(string_value = "New")]
    #[serde(rename = "n")]
    New,
    #[sea_orm(string_value = "Backordered")]
    #[serde(rename = "b")]
    Backordered,
    #[sea_orm(string_value = "Pending")]
    #[serde(rename = "p")]
    Pending,
    #[sea_orm(string_value = "Shipped")]
    #[serde(rename = "s")]
    Shipped,
    #[sea_orm(string_value = "Refunded")]
    #[serde(rename = "r")]
    Refunded,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    #[serde(rename = "l")]
    pub length: usize,
    #[serde(rename = "s")]
    pub steps:  Vec<Step>,
}
