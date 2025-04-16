#![cfg(any(test, feature = "test"))]

use fieldx_plus::fx_plus;
pub mod db;
pub mod scriptwriter;
pub mod types;

#[fx_plus(app, sync, get)]
pub struct TestCompany {
    scenario: scriptwriter::ScriptWriter,
}

impl TestCompany {}
