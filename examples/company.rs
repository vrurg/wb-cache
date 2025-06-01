use wb_cache::test::simulation::sim_app::EcommerceApp;
use wb_cache::test::simulation::types::Result;

#[tokio::main]
async fn main() -> Result<()> {
    EcommerceApp::run().await.inspect_err(|err| {
        err.report_with_backtrace(format!("Application errored out: {err}"));
    })
}
