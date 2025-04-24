use std::fmt::Display;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::error::ErrorKind;
use clap::CommandFactory;
use clap::Parser;
use fieldx::fxstruct;
use fieldx_plus::fx_plus;
use garde::Validate;
use indicatif::ProgressBar;
use indicatif::ProgressStyle;
use postcard::to_io;
use wb_cache::test::scriptwriter::steps::Step;
use wb_cache::test::scriptwriter::ScriptWriter;
use wb_cache::test::TestApp;

#[derive(Debug, Clone, clap::Parser, Validate)]
#[fxstruct(no_new, get(copy))]
#[clap(about, version, author, name = "company")]
struct Cli {
    /// File name of the script.
    #[fieldx(get(clone))]
    #[garde(skip)]
    script:              Option<PathBuf>,
    /// Simulation period in "days".
    #[clap(long, default_value_t = 365)]
    #[garde(range(min = 1))]
    period:              u32,
    /// Number of products to "offer"
    #[clap(long, default_value_t = 10)]
    #[garde(range(min = 1))]
    products:            u32,
    /// The number of customers we have on day 1.
    #[clap(long, default_value_t = 1)]
    #[garde(range(min = 1))]
    initial_customers:   u32,
    /// The maximum number of customers the company can have.
    #[clap(long, default_value_t = 10_000)]
    #[garde(range(min = 1))]
    market_capacity:     u32,
    /// Where customer base growth reaches its peak.
    #[clap(long, default_value_t = 3_000)]
    #[garde(range(min = 1), custom(Self::less_than("market-capacity", &self.market_capacity)))]
    inflection_point:    u32,
    /// Company's "success" rate – how fast the customer base grows
    #[clap(long, default_value_t = 0.05)]
    #[garde(range(min = 0.0))]
    growth_rate:         f32,
    /// Minimal number of orders per customer per day. Values below 1 indicate that a customer makes a purchase less
    /// than once a day.
    #[clap(long, default_value_t = 0.15)]
    #[garde(range(min = 0.0), custom(Self::less_than("max-customer-orders", &self.max_customer_orders)))]
    min_customer_orders: f32,
    /// Maximum number of orders per customer per day. This is not a hard limit but an expectation that 90% of the
    /// customers will fall within this range.  The remaining 10% may exhibit less restrained behavior.
    #[clap(long, default_value_t = 3.0)]
    #[garde(range(min = 0.0))]
    max_customer_orders: f32,
    /// The period of time we allow for a purchase to be returned.
    #[clap(long, default_value_t = 30)]
    #[garde(skip)]
    return_window:       u32,

    /// Save the script to a file.
    #[clap(long, short)]
    #[garde(custom(Self::with_file(&self.script)))]
    save: bool,

    #[clap(long, short)]
    #[garde(custom(Self::with_file(&self.script)))]
    load: bool,
}

impl Cli {
    fn less_than<'a, T: PartialOrd + Display>(
        max_name: &'static str,
        max: &'a T,
    ) -> impl FnOnce(&'a T, &()) -> garde::Result {
        move |value, _| {
            if value > max {
                Err(garde::Error::new(format!(
                    "{} is more than {max_name} ({})",
                    *value, *max
                )))
            }
            else {
                Ok(())
            }
        }
    }

    fn with_file<'a>(file: &'a Option<PathBuf>) -> impl FnOnce(&'a bool, &()) -> garde::Result {
        move |value, _| {
            if *value && file.is_none() {
                Err(garde::Error::new("Script file name is required"))
            }
            else {
                Ok(())
            }
        }
    }
}

#[fx_plus(app, new(private), sync, get, fallible(off, error(anyhow::Error)))]
struct SimApp {
    #[fieldx(lazy, fallible, get(clone), default(Cli::parse()))]
    cli: Cli,

    #[fieldx(lazy, get, fallible)]
    script_writer: Arc<ScriptWriter>,

    #[fieldx(lazy, get, fallible)]
    tempdir: tempfile::TempDir,
}

impl SimApp {
    fn build_cli(&self) -> Result<Cli> {
        self.cli()
    }

    fn build_script_writer(&self) -> Result<Arc<ScriptWriter>> {
        let cli = self.cli()?;
        ScriptWriter::builder()
            .period(cli.period())
            .product_count(cli.products())
            .initial_customers(cli.initial_customers())
            .market_capacity(cli.market_capacity())
            .inflection_point(cli.inflection_point())
            .growth_rate(cli.growth_rate() as f64)
            .min_customer_orders(cli.min_customer_orders() as f64)
            .max_customer_orders(cli.max_customer_orders() as f64)
            .return_window(cli.return_window())
            .build()
    }

    fn build_tempdir(&self) -> Result<tempfile::TempDir> {
        Ok(tempfile::Builder::new().prefix("wb-cache-simulation").tempdir()?)
    }

    fn validate(&self) -> Result<()> {
        if let Err(err) = self.cli()?.validate() {
            let mut cmd = Cli::command();
            let err = cmd.error(ErrorKind::InvalidValue, err);

            err.exit();
        }

        Ok(())
    }

    async fn execute_script(&self, script: Vec<Step>) -> Result<()> {
        Ok(())
    }

    fn save_script(&self) -> Result<()> {
        let script = self.script_writer()?.create()?;
        let script_file = self.cli()?.script().unwrap();

        let out = std::fs::File::create(&script_file)?;
        let mut zip = zip::ZipWriter::new(out);
        zip.start_file("__script.postcard", zip::write::SimpleFileOptions::default())?;
        let pb = ProgressBar::no_length()
            .with_message(format!("Saving script to {}", script_file.display()))
            .with_style(ProgressStyle::default_spinner().template("[{binary_bytes:.yellow}] {msg}")?);

        let mut zip = BufWriter::with_capacity(128 * 1024, zip);
        to_io(&script, pb.wrap_write(&mut zip))?;
        pb.finish_with_message("Script saved successfully.");
        zip.into_inner()?.finish()?;

        Ok(())
    }

    async fn run() -> Result<()> {
        let app = SimApp::__fieldx_new();
        app.validate()?;
        let cli = app.cli()?;

        if cli.save() {
            tokio::task::spawn_blocking(move || app.save_script()).await??;
        }

        Ok(())
    }
}

impl TestApp for SimApp {
    fn tempdir(&self) -> Result<PathBuf> {
        self.tempdir().map(|t| t.path().to_path_buf())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // let script = ScriptWriter::create()?;

    // let mut out = BufWriter::new(File::create("__script.bin")?);
    // to_io(&script, &mut out)?;
    // out.into_inner()?.sync_all()?;

    SimApp::run().await?;

    Ok(())
}
