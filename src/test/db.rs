#![cfg(any(test, feature = "test"))]
pub mod entity;
pub mod migrations;

pub mod prelude {
    pub use super::entity::*;
}
