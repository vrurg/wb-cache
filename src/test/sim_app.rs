use std::collections::HashMap;
use std::fmt::Display;
use std::io::BufWriter;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use clap::error::ErrorKind;
use clap::CommandFactory;
use clap::Parser;
use fieldx::fxstruct;
use fieldx_plus::agent_build;
use fieldx_plus::fx_plus;
use garde::Validate;
use indicatif::ProgressBar;
use indicatif::ProgressStyle;
use postcard::to_io;
use sea_orm::ConnectionTrait;
use sea_orm::DatabaseConnection;
use sea_orm_migration::MigratorTrait;
use tokio::sync::Barrier;
use tokio::task::JoinSet;

use super::actor::TestActor;
use super::db::migrations::Migrator;
use super::progress::POrder;
use super::progress::PStyle;
use super::progress::ProgressUI;
use super::scriptwriter::steps::Step;
use super::scriptwriter::ScriptWriter;
use super::types::simerr;
use super::types::Result;
use super::TestApp;

use crate::test::types::SimErrorAny;

const INNER_ZIP_NAME: &str = "__script.postcard";

#[derive(Debug, Clone, clap::Parser, Validate)]
#[fxstruct(no_new, get(copy))]
#[clap(about, version, author, name = "company")]
struct Cli {
    /// File name of the script.
    #[fieldx(get(clone))]
    #[garde(skip)]
    script: Option<PathBuf>,

    /// Path to the directory where the database is stored.
    /// If not provided, a temporary directory will be used.
    #[clap(long)]
    #[fieldx(get(copy(off)))]
    #[garde(skip)]
    db_path: Option<PathBuf>,

    /// Simulation period in "days".
    #[clap(long, default_value_t = 365)]
    #[garde(range(min = 1))]
    period: u32,

    /// Number of products to "offer"
    #[clap(long, default_value_t = 10)]
    #[garde(range(min = 1))]
    products: u32,

    /// The number of customers we have on day 1.
    #[clap(long, default_value_t = 1)]
    #[garde(range(min = 1))]
    initial_customers: u32,

    /// The maximum number of customers the company can have.
    #[clap(long, default_value_t = 10_000)]
    #[garde(range(min = 1))]
    market_capacity: u32,

    /// Where customer base growth reaches its peak.
    #[clap(long, default_value_t = 3_000)]
    #[garde(range(min = 1), custom(Self::less_than("market-capacity", &self.market_capacity)))]
    inflection_point: u32,

    /// Company's "success" rate – how fast the customer base grows
    #[clap(long, default_value_t = 0.05)]
    #[garde(range(min = 0.0))]
    growth_rate: f32,

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
    return_window: u32,

    /// Save the script to a file.
    #[clap(long, short)]
    #[garde(custom(Self::with_file(&self.script)))]
    save: bool,

    /// Load the script from a file.
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

#[fx_plus(app, rc, new(private), sync, get, fallible(off, error(SimErrorAny)))]
pub struct SimApp {
    #[fieldx(lazy, private, fallible, get(clone), default(Cli::parse()))]
    cli: Cli,

    #[fieldx(lazy, get, fallible)]
    script_writer: Arc<ScriptWriter>,

    #[fieldx(lazy, get, fallible)]
    tempdir: tempfile::TempDir,

    #[fieldx(lazy, fallible, get)]
    progress_ui: ProgressUI,
}

impl SimApp {
    fn build_cli(&self) -> Result<Cli, SimErrorAny> {
        self.cli()
    }

    fn build_script_writer(&self) -> Result<Arc<ScriptWriter>, SimErrorAny> {
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

    fn build_tempdir(&self) -> Result<tempfile::TempDir, SimErrorAny> {
        Ok(tempfile::Builder::new().prefix("wb-cache-simulation").tempdir()?)
    }

    fn build_progress_ui(&self) -> Result<ProgressUI, SimErrorAny> {
        Ok(ProgressUI::builder().build()?)
    }

    fn validate(&self) -> Result<(), SimErrorAny> {
        if let Err(err) = self.cli()?.validate() {
            let mut cmd = Cli::command();
            let err = cmd.error(ErrorKind::InvalidValue, err);

            err.exit();
        }

        Ok(())
    }

    async fn new_sqlite_db(&self, db_file: &str) -> Result<Arc<DatabaseConnection>> {
        let db_path = self.db_dir()?.join(db_file);

        let schema = format!("sqlite://{}?mode=rwc", db_path.display());
        let db = sea_orm::Database::connect(&schema)
            .await
            .inspect_err(|e| eprintln!("Error connecting to database {schema}: {e}"))?;

        Migrator::down(&db, None).await?;
        Migrator::up(&db, None).await?;

        db.execute_unprepared("PRAGMA journal_mode=WAL;").await?;
        db.execute_unprepared("PRAGMA cache=64000;").await?;
        db.execute_unprepared("PRAGMA synchronous=NORMAL;").await?;

        Ok(Arc::new(db))
    }

    async fn execute_script(&self, screenplay: Vec<Step>) -> Result<(), SimErrorAny> {
        let barrier = Arc::new(Barrier::new(2));

        let mut actors = JoinSet::<Result<(&'static str, Duration), SimErrorAny>>::new();
        let myself = self.myself().unwrap();
        let screenplay = Arc::new(screenplay);
        let s1 = screenplay.clone();

        let b1 = barrier.clone();
        actors.spawn(async move {
            b1.wait().await;
            let started = Instant::now();
            let plain_actor = agent_build!(
                myself, crate::test::company_plain::TestCompany<Self> =>
                    db: myself.new_sqlite_db("test_company_plain.db").await?;
            )?;
            plain_actor.act(&s1).await.map_err(|err| {
                let err = simerr!("{}", err);
                err.context("plain");
                err
            })?;
            Ok(("plain", Instant::now().duration_since(started)))
        });

        let s2 = screenplay.clone();
        let b2 = barrier.clone();
        let myself = self.myself().unwrap();
        actors.spawn(async move {
            b2.wait().await;
            let started = Instant::now();
            let cached_actor = agent_build!(
                myself, crate::test::company_cached::TestCompany<Self> =>
                    db: myself.new_sqlite_db("test_company_cached.db").await?;
            )?;
            cached_actor.act(&s2).await.map_err(|e| {
                let err = simerr!("{}", e);
                err.context("cached");
                err
            })?;
            Ok(("cached", Instant::now().duration_since(started)))
        });

        let results = actors.join_all().await;
        let mut all_success = true;
        let mut outcomes = HashMap::new();
        for result in results {
            match result {
                Ok((label, duration)) => {
                    outcomes.insert(label.to_string(), duration);
                }
                Err(err) => {
                    eprintln!("Actor errored out: {err:#}");
                    all_success = false;
                }
            }
        }

        if all_success {
            let plain = outcomes.get("plain").unwrap();
            let cached = outcomes.get("cached").unwrap();
            println!("\n{:>11} | {:>11}", "plain", "cached");
            println!("{:>10.2}s | {:>10.2}s", plain.as_secs_f64(), cached.as_secs_f64());
            println!("{:>10.2}x | {:>10.2}x", plain.as_secs_f64() / cached.as_secs_f64(), 1.0);
        }

        Ok(())
    }

    fn save_script(&self) -> Result<(), SimErrorAny> {
        let script = self.script_writer()?.create()?;
        let script_file = self.cli()?.script().unwrap();

        let out = std::fs::File::create(&script_file)?;
        let mut zip = zip::ZipWriter::new(out);
        zip.start_file(INNER_ZIP_NAME, zip::write::SimpleFileOptions::default())?;
        let pb = ProgressBar::no_length()
            .with_message(format!("Saving script to {}", script_file.display()))
            .with_style(ProgressStyle::default_spinner().template("[{binary_bytes:.yellow}] {msg}")?);

        let mut zip = BufWriter::with_capacity(128 * 1024, zip);
        to_io(&script, pb.wrap_write(&mut zip))?;
        pb.finish_with_message("Script saved successfully.");
        zip.into_inner()?.finish()?;

        Ok(())
    }

    fn load_script(&self) -> Result<Vec<Step>, SimErrorAny> {
        let script_file = self.cli()?.script().unwrap();
        let file = std::fs::File::open(&script_file)?;
        let mut zip = zip::ZipArchive::new(file)?;
        let zip_file = zip.by_name(INNER_ZIP_NAME)?;

        let size = zip_file.size();
        let mut buf = vec![0; size as usize];

        let pb = ProgressBar::new(size)
            .with_message(format!("Loading script from {}", script_file.display()))
            .with_style(ProgressStyle::default_spinner().template("[{binary_bytes:.yellow}] {msg}")?);

        pb.wrap_read(zip_file).read_exact(&mut buf[..size as usize])?;
        pb.set_message("Script file loaded successfully.");
        let script: Vec<Step> = postcard::from_bytes(&buf)?;
        pb.finish_with_message("Script extracted successfully.");

        Ok(script)
    }

    pub async fn run() -> Result<(), SimErrorAny> {
        let app = SimApp::__fieldx_new();
        app.validate()?;
        let cli = app.cli()?;

        if cli.save() {
            return tokio::task::spawn_blocking(move || app.save_script()).await?;
        }

        let script = if cli.load() {
            app.load_script()?
        }
        else {
            app.script_writer()?.create()?
        };

        app.execute_script(script).await
    }
}

impl TestApp for SimApp {
    fn db_dir(&self) -> Result<PathBuf, SimErrorAny> {
        self.cli()?
            .db_path()
            .as_ref()
            .cloned()
            .map_or_else(|| self.tempdir().map(|t| t.path().to_path_buf()), Ok)
    }

    fn acquire_progress<'a>(
        &'a self,
        style: PStyle,
        order: Option<POrder<'a>>,
    ) -> Result<Option<ProgressBar>, SimErrorAny> {
        Ok(self.progress_ui()?.acquire_progress(style, order))
    }
}
