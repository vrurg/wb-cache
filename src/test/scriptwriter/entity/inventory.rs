use fieldx::fxstruct;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug)]
#[fxstruct(no_new, builder, get(copy))]
pub struct InventoryRecord {
    product_id:    u32,
    #[fieldx(get_mut)]
    stock:         u32,
    handling_days: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncomingShipment {
    #[serde(rename = "p")]
    pub product_id: u32,
    #[serde(rename = "b")]
    pub batch_size: u32,
}

impl InventoryRecord {
    pub fn new(product_id: u32, stock: u32, handling_days: u8) -> Self {
        Self {
            product_id,
            stock,
            handling_days,
        }
    }
}

impl From<InventoryRecord> for crate::test::db::entity::InventoryRecord {
    fn from(record: InventoryRecord) -> Self {
        Self {
            product_id:    record.product_id,
            stock:         record.stock,
            handling_days: record.handling_days,
        }
    }
}
