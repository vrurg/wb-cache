use serde::Deserialize;
use serde::Serialize;
use strum::Display;

use crate::test::simulation::db::entity::Customer as DbCustomer;
use crate::test::simulation::db::entity::InventoryRecord as DbInventoryRecord;
use crate::test::simulation::db::entity::Order as DbOrder;
use crate::test::simulation::db::entity::Product as DbProduct;
use crate::test::simulation::db::entity::Session as DbSession;

use super::entity::inventory::IncomingShipment;
use super::entity::inventory::InventoryCheck;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptTitle {
    pub period: i32,
    pub products: i32,
    pub market_capacity: u32,
}

#[derive(Display, Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Step {
    /// Initial header for the script with the introductory information.
    #[serde(rename = "h")]
    Title(ScriptTitle),
    #[serde(rename = "d")]
    /// The simulation day number.
    Day(i32),
    /// Add a new product into the database.
    // #[serde(rename = "ap")]
    AddProduct(DbProduct),
    /// Register a new customer
    #[serde(rename = "ac")]
    AddCustomer(DbCustomer),
    /// A new purchase order from customer
    #[serde(rename = "ao")]
    AddOrder(DbOrder),
    #[serde(rename = "uo")]
    UpdateOrder(DbOrder),
    /// Add a new session entry.
    #[serde(rename = "as")]
    AddSession(DbSession),
    /// Update an existing session entry.
    UpdateSession(DbSession),
    /// Collect expired sessions.
    CollectSessions,
    /// Update an existing order status.
    /// Add a new inventory record.
    #[serde(rename = "ai")]
    AddInventoryRecord(DbInventoryRecord),
    /// Update an existing inventory record.
    #[serde(rename = "is")]
    AddStock(IncomingShipment),
    /// Increase product views counter.
    ViewProduct(i32),
    /// Check the inventory stock and make sure it corresponds to the expected value.
    /// This is useful for testing purposes to ensure that scenario is in sync with the simulation.
    #[serde(rename = "ci")]
    CheckInventory(InventoryCheck),
}
