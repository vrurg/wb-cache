#![cfg(any(test, feature = "test"))]
pub mod customer;
pub mod inventory_record;
pub mod order;
pub mod product;
pub mod session;

use sea_orm_migration::prelude::*;

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(customer::Migration),
            Box::new(inventory_record::Migration),
            Box::new(order::Migration),
            Box::new(product::Migration),
            Box::new(session::Migration),
        ]
    }
}
