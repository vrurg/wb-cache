#![cfg(any(test, feature = "simulation"))]
pub mod customer;
pub mod inventory_record;
pub mod order;
pub mod product;
pub mod session;

pub use customer::Entity as Customers;
pub use customer::Manager as CustomerMgr;
pub use customer::Model as Customer;
pub use inventory_record::Entity as InventoryRecords;
pub use inventory_record::Manager as InventoryRecordMgr;
pub use inventory_record::Model as InventoryRecord;
pub use order::Entity as Orders;
pub use order::Manager as OrderMgr;
pub use order::Model as Order;
pub use product::Entity as Products;
pub use product::Manager as ProductMgr;
pub use product::Model as Product;
pub use session::Entity as Sessions;
pub use session::Manager as SessionMgr;
pub use session::Model as Session;

use crate::test::simulation::types::simerr;
use crate::test::simulation::types::SimErrorAny;

// Generate an error when a manager object accidentally holds a weak reference to
// a DB provider that has zero strong references. This scenario should not occur
// under normal circumstances, except in the case of a severe internal error.
#[inline(always)]
pub(crate) fn dbcp_gone(who: &str) -> SimErrorAny {
    simerr!("({who}) Database Provider object (parent) is gone")
}
