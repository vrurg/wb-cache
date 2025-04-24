use serde::Deserialize;
use serde::Serialize;
use strum::Display;

use crate::test::db::entity::Customer as DbCustomer;
use crate::test::db::entity::InventoryRecord as DbInventoryRecord;
use crate::test::db::entity::Order as DbOrder;
use crate::test::db::entity::Product as DbProduct;

use super::entity::inventory::IncomingShipment;

#[derive(Display, Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Step {
    /// Initial header for the script with the introductory information.
    #[serde(rename = "h")]
    Header {
        period:          u32,
        products:        u32,
        market_capacity: u32,
    },
    /// The simulation day number.
    #[serde(rename = "d")]
    Day(u32),
    /// Add a new product into the database.
    // #[serde(rename = "ap")]
    AddProduct(DbProduct),
    /// Register a new customer
    #[serde(rename = "ac")]
    AddCustomer(DbCustomer),
    /// A new purchase order from customer
    #[serde(rename = "ao")]
    AddOrder(DbOrder),
    /// Update an existing order.
    #[serde(rename = "uo")]
    UpdateOrder(DbOrder),
    /// Add a new inventory record.
    #[serde(rename = "ai")]
    AddInventoryRecord(DbInventoryRecord),
    /// Update an existing inventory record.
    #[serde(rename = "as")]
    AddStock(IncomingShipment),
    /// Check the inventory stock and make sure it corresponds to the expected value.
    /// This is useful for testing purposes to ensure that scenario is in sync with the simulation.
    #[serde(rename = "ci")]
    CheckInventory { product_id: u32, stock: u32 },
}
