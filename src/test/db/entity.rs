#![cfg(any(test, feature = "test"))]
pub mod customer;
pub mod inventory_record;
pub mod order;
pub mod product;

pub use customer::Model as Customer;
pub use inventory_record::Model as InventoryRecord;
pub use order::Model as Order;
pub use product::Model as Product;
