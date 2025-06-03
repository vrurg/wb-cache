#![cfg(all(feature = "simulation", feature = "sqlite"))]

use wb_cache::test::simulation::sim_app::EcommerceApp;

#[tokio::test]
async fn full_simulation() -> Result<(), Box<dyn std::error::Error>> {
    // Use relatively relaxed parameters for the simulation to ensure it runs quickly.
    let app = EcommerceApp::builder()
        .cli_args(vec![
            "full_simulation_test",
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
