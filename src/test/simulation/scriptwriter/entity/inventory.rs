use fieldx::fxstruct;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug)]
#[fxstruct(no_new, builder, get(copy))]
pub struct InventoryRecord {
    product_id:    i32,
    #[fieldx(get_mut)]
    stock:         i64,
    handling_days: i16,
}

impl InventoryRecord {
    pub fn new(product_id: i32, stock: i64, handling_days: i16) -> Self {
        Self {
            product_id,
            stock,
            handling_days,
        }
    }
}

impl From<InventoryRecord> for crate::test::simulation::db::entity::InventoryRecord {
    fn from(record: InventoryRecord) -> Self {
        Self {
            product_id:    record.product_id,
            stock:         record.stock,
            handling_days: record.handling_days,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncomingShipment {
    #[serde(rename = "p")]
    pub product_id: i32,
    #[serde(rename = "b")]
    pub batch_size: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryCheck {
    #[serde(rename = "p")]
    pub product_id: i32,
    #[serde(rename = "s")]
    pub stock:      i64,
    #[serde(rename = "c")]
    pub comment:    String,
}

impl InventoryCheck {
    pub fn new<S: ToString>(product_id: i32, stock: i64, comment: S) -> Self {
        Self {
            product_id,
            stock,
            comment: comment.to_string(),
        }
    }
}
