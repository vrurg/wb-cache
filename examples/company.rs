use wb_cache::test::sim_app::SimApp;
use wb_cache::test::types::Result;

#[tokio::main]
async fn main() -> Result<()> {
    SimApp::run().await.inspect_err(|err| {
        err.report_with_backtrace(format!("Application errored out: {err}"));
    })
}
