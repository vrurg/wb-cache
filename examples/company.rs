use wb_cache::test::sim_app::SimApp;
use wb_cache::test::types::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // let script = ScriptWriter::create()?;

    // let mut out = BufWriter::new(File::create("__script.bin")?);
    // to_io(&script, &mut out)?;
    // out.into_inner()?.sync_all()?;

    SimApp::run().await.inspect_err(|err| {
        eprintln!("Application errored out: {err}");
    })
}
