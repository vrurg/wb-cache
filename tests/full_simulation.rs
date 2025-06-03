#![cfg(feature = "simulation")]

use wb_cache::test::simulation::sim_app::EcommerceApp;

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn full_simulation_sqlite() -> Result<(), Box<dyn std::error::Error>> {
    // Use relatively relaxed parameters for the simulation to ensure it runs quickly.
    let app = EcommerceApp::builder()
        .cli_args(vec![
            "full_simulation_sqlite_test",
            "--quiet",
            "--sqlite",
            "--test",
            "--market-capacity=100",
            "--inflection-point=40",
            "--period=50",
        ])
        .build()?;

    assert!(app.execute().await.is_ok(), "Simulation failed");

    Ok(())
}

#[cfg(feature = "pg")]
#[tokio::test]
async fn full_simulation_pg() -> Result<(), Box<dyn std::error::Error>> {
    // Use relatively relaxed parameters for the simulation to ensure it runs quickly.

    use std::env;
    assert!(
        env::var("WBCACHE_PG_HOST").is_ok(),
        "WBCACHE_PG_HOST must be set for PostgreSQL tests"
    );
    assert!(
        env::var("WBCACHE_PG_USER").is_ok(),
        "WBCACHE_PG_USER must be set for PostgreSQL tests"
    );
    assert!(
        env::var("WBCACHE_PG_PASSWORD").is_ok(),
        "WBCACHE_PG_PASSWORD must be set for PostgreSQL tests"
    );
    let app = EcommerceApp::builder()
        .cli_args(vec![
            "full_simulation_pg_test",
            "--quiet",
            "--pg",
            "--test",
            "--market-capacity=100",
            "--inflection-point=40",
            "--period=30",
        ])
        .build()?;

    assert!(app.execute().await.is_ok(), "Simulation failed");

    Ok(())
}
