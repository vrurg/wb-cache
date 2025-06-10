use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Display;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use clap::error::ErrorKind;
use clap::CommandFactory;
use clap::Parser;
use comfy_table::CellAlignment;
use fieldx::fxstruct;
use fieldx_plus::agent_build;
use fieldx_plus::fx_plus;
use garde::Validate;
use indicatif::ProgressBar;
use indicatif::ProgressStyle;
use postcard::to_io;
use sea_orm::entity::*;
use sea_orm::query::*;
use sea_orm::EntityTrait;
use sea_orm::QueryOrder;
use sea_orm_migration::MigratorTrait;
use tokio::signal;
use tokio::sync::Barrier;
use tokio::task::JoinSet;
use tokio_stream::StreamExt;
use tracing::instrument;

use super::actor::TestActor;
use super::db;
#[cfg(feature = "pg")]
use super::db::driver::pg::Pg;
#[cfg(feature = "sqlite")]
use super::db::driver::sqlite::Sqlite;
use super::db::driver::DatabaseDriver;
use super::db::entity::Customers;
use super::db::entity::InventoryRecords;
use super::db::entity::Orders;
use super::db::entity::Products;
use super::db::entity::Sessions;
use super::db::migrations::Migrator;
use super::progress::MaybeProgress;
use super::progress::POrder;
use super::progress::PStyle;
use super::progress::ProgressUI;
use super::scriptwriter::steps::Step;
use super::scriptwriter::ScriptWriter;
use super::types::simerr;
use super::types::Result;
use super::types::SimError;
use super::types::SimErrorAny;
use super::SimulationApp;

const INNER_ZIP_NAME: &str = "__script.postcard";

#[allow(unused)]
#[derive(Default, Debug, Clone, Copy)]
pub struct ActorStatus {
    per_sec: f64,
    step:    usize,
}

impl ActorStatus {
    pub fn new(per_sec: f64, step: usize) -> Self {
        Self { per_sec, step }
    }
}

#[derive(Debug, Clone, clap::Parser, Validate)]
#[fxstruct(no_new, get(copy))]
#[clap(about, version, author, name = "company")]
pub(crate) struct Cli {
    /// File name of the script.
    #[fieldx(get(clone))]
    #[garde(skip)]
    #[clap(env = "WBCACHE_SCRIPT_FILE")]
    script: Option<PathBuf>,

    #[fieldx(get(clone))]
    #[garde(skip)]
    #[clap(long, short, env = "WBCACHE_OUTPUT")]
    output: Option<PathBuf>,

    /// Silence the output
    #[clap(long, short, env = "WBCACHE_QUIET", default_value_t = false)]
    #[garde(skip)]
    quiet: bool,

    /// Simulation period in "days".
    #[clap(long, env = "WBCACHE_PERIOD", default_value_t = 365)]
    #[garde(range(min = 1))]
    period: u32,

    /// Number of products to "offer"
    #[clap(long, env = "WBCACHE_PRODUCTS", default_value_t = 10)]
    #[garde(range(min = 1))]
    products: u32,

    /// The number of customers we have on day 1.
    #[clap(long, env = "WBCACHE_INITIAL_CUSTOMERS", default_value_t = 1)]
    #[garde(range(min = 1))]
    initial_customers: u32,

    /// The maximum number of customers the company can have.
    #[clap(long, env = "WBCACHE_MARKET_CAPACITY", default_value_t = 1_000)]
    #[garde(range(min = 1))]
    market_capacity: u32,

    /// Where customer base growth reaches its peak.
    #[clap(long, env = "WBCACHE_INFLECTION_POINT", default_value_t = 400)]
    #[garde(range(min = 1), custom(Self::less_than("market-capacity", &self.market_capacity)))]
    inflection_point: u32,

    /// Company's "success" rate – how fast the customer base grows
    #[clap(long, env = "WBCACHE_GROWTH_RATE", default_value_t = 0.05)]
    #[garde(range(min = 0.0))]
    growth_rate: f32,

    /// Minimal number of orders per customer per day. Values below 1 indicate that a customer makes a purchase less
    /// than once a day.
    #[clap(long, env = "WBCACHE_MIN_CUSTOMER_ORDERS", default_value_t = 0.15)]
    #[garde(range(min = 0.0), custom(Self::less_than("max-customer-orders", &self.max_customer_orders)))]
    min_customer_orders: f32,

    /// Maximum number of orders per customer per day. This is not a hard limit but an expectation that 90% of the
    /// customers will fall within this range.  The remaining 10% may exhibit less restrained behavior.
    #[clap(long, env = "WBCACHE_MAX_CUSTOMER_ORDERS", default_value_t = 3.0)]
    #[garde(range(min = 0.0))]
    max_customer_orders: f32,

    /// The period of time we allow for a purchase to be returned.
    #[clap(long, env = "WBCACHE_RETURN_WINDOW", default_value_t = 30)]
    #[garde(skip)]
    return_window: u32,

    /// Save the script to a file.
    #[clap(long, short, env = "WBCACHE_SAVE_SCRIPT", default_value_t = false)]
    #[garde(custom(Self::with_file(&self.script)))]
    save: bool,

    /// Load the script from a file.
    #[clap(long, short, env = "WBCACHE_LOAD_SCRIPT", default_value_t = false)]
    #[garde(custom(Self::with_file(&self.script)))]
    // This field is only used when either sqlite or pg features are enabled.
    #[fieldx(get(attributes_fn(allow(unused))))]
    load: bool,

    /// Test the results of the simulation by comparing two databases.
    #[clap(long, short, env = "WBCACHE_TEST", default_value_t = false)]
    #[garde(skip)]
    // This field is only used when either sqlite or pg features are enabled.
    #[fieldx(get(attributes_fn(allow(unused))))]
    test: bool,

    #[clap(long, env = "WBCACHE_SQLITE", default_value_t = false)]
    #[fieldx(get(copy))]
    #[garde(custom(Self::feature_or_container(cfg!(feature = "sqlite"), "sqlite", &self.container)))]
    /// Use SQLite as the database backend.
    sqlite: bool,

    /// Path to the directory where the SQLite database is stored.
    /// If not provided, a temporary directory will be used.
    #[clap(long, env = "WBCACHE_SQLITE_PATH")]
    #[fieldx(get(clone))]
    #[garde(skip)]
    sqlite_path: Option<PathBuf>,

    /// Use PostgreSQL as the database backend.
    #[clap(long, env = "WBCACHE_PG", default_value_t = false)]
    #[fieldx(get(copy))]
    #[garde(custom(Self::feature_or_container(cfg!(feature = "pg"), "pg", &self.container)))]
    pg: bool,

    #[clap(long, env = "WBCACHE_PG_HOST", default_value = "localhost")]
    #[fieldx(get(clone))]
    #[garde(skip)]
    pg_host: String,

    #[clap(long, env = "WBCACHE_PG_PORT", default_value_t = 5432)]
    #[fieldx(get(copy))]
    #[garde(skip)]
    pg_port: u16,

    #[clap(long, env = "WBCACHE_PG_USER", default_value = "wbcache")]
    #[fieldx(get(clone))]
    #[garde(skip)]
    pg_user: String,

    #[clap(long, env = "WBCACHE_PG_PASSWORD", hide_env_values = true, default_value = "wbcache")]
    #[fieldx(get(clone))]
    #[garde(skip)]
    pg_password: String,

    #[clap(long, env = "WBCACHE_PG_DB_PREFIX", default_value = "wbcache_test")]
    #[fieldx(get(clone))]
    #[garde(skip)]
    pg_db_prefix: String,

    /// File to send log into
    #[clap(long, env = "WBCACHE_LOG_FILE")]
    #[fieldx(get(clone))]
    #[garde(skip)]
    log_file: Option<PathBuf>,

    /// URL of the Loki server for tracing.
    #[clap(long, env = "WBCACHE_LOKI_URL", default_value = "https://127.0.0.1:3100")]
    #[fieldx(get(clone))]
    #[garde(skip)]
    loki_url: String,

    #[fieldx(get(copy))]
    #[clap(long)]
    #[garde(skip)]
    container: bool,
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
            if !*value || file.is_some() {
                Ok(())
            }
            else {
                Err(garde::Error::new("Script file name is required"))
            }
        }
    }

    fn feature_or_container<'a>(
        feature_enabled: bool,
        feature: &'static str,
        container: &'a bool,
    ) -> impl FnOnce(&'a bool, &()) -> garde::Result {
        move |value, _| {
            if !*value || *container || feature_enabled {
                Ok(())
            }
            else {
                Err(garde::Error::new(format!(
                    "Build feature '{feature}' must be enabled or --container must be set."
                )))
            }
        }
    }
}

#[fx_plus(
    app,
    rc,
    new(private),
    sync,
    get,
    fallible(off, error(SimErrorAny)),
    builder(vis(pub))
)]
pub struct EcommerceApp {
    #[fieldx(inner_mut, clearer, builder("_cli_args"))]
    cli_args: Vec<String>,

    #[fieldx(lazy, private, fallible(error(clap::Error)), get(clone))]
    cli: Cli,

    #[fieldx(lazy, lock, get, clearer, fallible)]
    script_writer: Arc<ScriptWriter>,

    // This field is only used when either sqlite or pg features are enabled.
    #[fieldx(lazy, private, get(attributes_fn(allow(unused))), fallible)]
    tempdir: tempfile::TempDir,

    #[fieldx(lazy, fallible, get(clone), clearer)]
    progress_ui: Arc<ProgressUI>,

    // This field is only used when either sqlite or pg features are enabled.
    #[fieldx(
        lock,
        private,
        get(copy, attributes_fn(allow(unused))),
        set("_set_plain_status"),
        builder(off)
    )]
    plain_status: ActorStatus,

    // This field is only used when either sqlite or pg features are enabled.
    #[fieldx(
        lock,
        private,
        get(copy, attributes_fn(allow(unused))),
        set("_set_cached_status"),
        builder(off)
    )]
    cached_status: ActorStatus,
}

impl EcommerceApp {
    fn build_cli(&self) -> Result<Cli, clap::Error> {
        Ok(if let Some(custom_args) = self.clear_cli_args() {
            Cli::try_parse_from(custom_args.into_iter())?
        }
        else {
            Cli::try_parse()?
        })
    }

    fn build_script_writer(&self) -> Result<Arc<ScriptWriter>> {
        let cli = self.cli()?;
        Ok(ScriptWriter::builder()
            .quiet(cli.quiet())
            .period(cli.period() as i32)
            .product_count(cli.products() as i32)
            .initial_customers(cli.initial_customers())
            .market_capacity(cli.market_capacity())
            .inflection_point(cli.inflection_point())
            .growth_rate(cli.growth_rate() as f64)
            .min_customer_orders(cli.min_customer_orders() as f64)
            .max_customer_orders(cli.max_customer_orders() as f64)
            .return_window(cli.return_window() as i32)
            .build()?)
    }

    fn build_tempdir(&self) -> Result<tempfile::TempDir, SimErrorAny> {
        Ok(tempfile::Builder::new().prefix("wb-cache-simulation").tempdir()?)
    }

    fn build_progress_ui(&self) -> Result<Arc<ProgressUI>, SimErrorAny> {
        Ok(ProgressUI::builder().quiet(self.cli()?.quiet()).build()?)
    }

    fn validate(&self) -> Result<(), SimErrorAny> {
        if let Err(err) = self.cli()?.validate() {
            let mut cmd = Cli::command();
            let err = cmd.error(ErrorKind::InvalidValue, err);

            err.exit();
        }

        Ok(())
    }

    async fn db_prepare<D: DatabaseDriver>(&self, dbd: &D) -> Result<()> {
        dbd.configure().await?;
        let db = dbd.connection();
        Migrator::down(&db, None).await?;
        Migrator::up(&db, None).await?;
        Ok(())
    }

    async fn compare_tables<E>(
        &self,
        table: &str,
        key: E::Column,
        name1: &str,
        db1: Arc<impl DatabaseDriver>,
        name2: &str,
        db2: Arc<impl DatabaseDriver>,
    ) -> Result<(), SimErrorAny>
    where
        E: EntityTrait,
        E::Model: FromQueryResult + Sized + Send + Sync + PartialEq + Debug,
    {
        let conn1 = db1.connection();
        let conn2 = db2.connection();

        let mut paginator1 = E::find().order_by_asc(key).paginate(&conn1, 1000).into_stream();
        let mut paginator2 = E::find().order_by_asc(key).paginate(&conn2, 1000).into_stream();

        loop {
            let page1 = paginator1.next().await;
            let page2 = paginator2.next().await;

            if page1.is_none() && page2.is_none() {
                break;
            }

            if page1.is_none() {
                return Err(simerr!("Table '{table}': {name2} has more records than {name1}"));
            }
            if page2.is_none() {
                return Err(simerr!("Table '{table}': {name1} has more records than {name2}"));
            }

            let page1 = page1.unwrap()?;
            let page2 = page2.unwrap()?;

            if page1.len() != page2.len() {
                return Err(simerr!(
                    "Table '{table}': {name1} has {} records, {name2} has {} records",
                    page1.len(),
                    page2.len()
                ));
            }

            for (record1, record2) in page1.iter().zip(page2.iter()) {
                if record1 != record2 {
                    return Err(simerr!(
                        "Table '{table}': Records do not match: {name1} = {:?}, {name2} = {:?}",
                        record1,
                        record2
                    ));
                }
            }
        }

        Ok(())
    }

    // Implement the most straightforward test by comparing all records in all
    // tables in both databases.
    async fn test_db<D: DatabaseDriver>(&self, db_plain: Arc<D>, db_cached: Arc<D>) -> Result<(), SimErrorAny> {
        self.compare_tables::<Customers>(
            "customers",
            db::entity::customer::Column::Id,
            "plain",
            db_plain.clone(),
            "cached",
            db_cached.clone(),
        )
        .await?;

        self.compare_tables::<InventoryRecords>(
            "inventory_records",
            db::entity::inventory_record::Column::ProductId,
            "plain",
            db_plain.clone(),
            "cached",
            db_cached.clone(),
        )
        .await?;

        self.compare_tables::<Products>(
            "products",
            db::entity::product::Column::Id,
            "plain",
            db_plain.clone(),
            "cached",
            db_cached.clone(),
        )
        .await?;

        self.compare_tables::<Orders>(
            "orders",
            db::entity::order::Column::Id,
            "plain",
            db_plain.clone(),
            "cached",
            db_cached.clone(),
        )
        .await?;

        self.compare_tables::<Sessions>(
            "sessions",
            db::entity::session::Column::Id,
            "plain",
            db_plain.clone(),
            "cached",
            db_cached.clone(),
        )
        .await?;

        Ok(())
    }

    #[instrument(level = "trace", skip(self, db_plain, db_cached, screenplay))]
    async fn execute_script<D: DatabaseDriver>(
        &self,
        db_plain: Arc<D>,
        db_cached: Arc<D>,
        screenplay: Arc<Vec<Step>>,
    ) -> Result<(), SimErrorAny> {
        let driver_name = db_plain.name();
        let cli = self.cli()?;

        let barrier = Arc::new(Barrier::new(2));

        let message_progress = self.progress_ui()?.acquire_progress(PStyle::Message, None);
        message_progress.maybe_set_prefix("Rate");

        let mut tasks = JoinSet::<Result<(&'static str, Duration), SimError>>::new();

        let myself = self.myself().unwrap();
        let total_steps = screenplay.len();
        let progress_ui = self.progress_ui()?;
        let progress_task = tokio::spawn(async move {
            let user_attended = progress_ui.user_attended();
            let mut interval = tokio::time::interval(Duration::from_millis(if user_attended { 100 } else { 1000 }));

            loop {
                interval.tick().await;

                let plain_status = myself.plain_status();
                let cached_status = myself.cached_status();

                let rate = if plain_status.per_sec > 0.0 {
                    cached_status.per_sec / plain_status.per_sec
                }
                else {
                    0.0
                };

                let mut message_parts = Vec::new();

                message_parts.push(format!(
                    "{rate:.2}x | Average: cached {:.2}/s, plain {:.2}/s",
                    cached_status.per_sec, plain_status.per_sec,
                ));

                if !user_attended {
                    message_parts.push(format!(
                        "Steps: cached {:.2}%, plain {:.2}%",
                        (cached_status.step as f32 / total_steps as f32) * 100.0,
                        (plain_status.step as f32 / total_steps as f32) * 100.0
                    ));
                }

                let message_line = message_parts.join(" | ");
                if user_attended {
                    message_progress.maybe_set_message(message_line);
                    message_progress.maybe_inc(1);
                }
                else if !progress_ui.quiet() {
                    progress_ui.report_info(message_line);
                }
            }
        });

        // Spawn the plain actor
        let myself = self.myself().unwrap();
        let s1 = screenplay.clone();
        let b1 = barrier.clone();
        let db_plain_async = db_plain.clone();
        tasks.spawn(async move {
            myself.db_prepare(&*db_plain_async).await?;
            b1.wait().await;
            let started = Instant::now();
            let plain_actor = agent_build!(
                myself, crate::test::simulation::company_plain::TestCompany<Self, D> {
                    db: db_plain_async
                }
            )?;
            plain_actor.act(&s1).await.inspect_err(|err| {
                err.context("Plain actor");
            })?;
            Ok(("plain", Instant::now().duration_since(started)))
        });

        // Spawn the cached actor
        let s2 = screenplay.clone();
        let b2 = barrier.clone();
        let myself = self.myself().unwrap();
        let db_cached_async = db_cached.clone();
        tasks.spawn(async move {
            myself.db_prepare(&*db_cached_async).await?;
            b2.wait().await;
            let started = Instant::now();
            let cached_actor = agent_build!(
                myself, crate::test::simulation::company_cached::TestCompany<Self, D> {
                    db: db_cached_async
                }
            )?;
            cached_actor.act(&s2).await.inspect_err(|err| {
                err.context("Cached actor");
            })?;
            myself.report_debug("Cached actor completed.");
            Ok(("cached", Instant::now().duration_since(started)))
        });

        let mut all_success = true;
        let mut outcomes = HashMap::new();

        while let Some(res) = tasks.join_next().await {
            match res {
                Ok(Ok((label, duration))) => {
                    self.report_info(format!("{} actor completed in {:.2}s", label, duration.as_secs_f64()));
                    outcomes.insert(label.to_string(), duration);
                }
                Ok(Err(err)) => {
                    all_success = false;
                    self.report_error(err.to_string_with_backtrace("An error occurred during actor execution"));
                    tasks.abort_all();
                }
                Err(err) => {
                    all_success = false;
                    let err = SimErrorAny::from(err);
                    self.report_error(err.to_string_with_backtrace("Actor errorred out"));
                    tasks.abort_all();
                }
            }
            self.report_info(format!("Tasks left: {}", tasks.len()));
        }

        progress_task.abort();

        if all_success {
            let plain = outcomes.get("plain").unwrap();
            let cached = outcomes.get("cached").unwrap();

            let mut table = comfy_table::Table::new();
            table
                .load_preset(comfy_table::presets::ASCII_FULL_CONDENSED)
                .set_header(["", "Plain", "Cached"]);

            table
                .add_row([
                    "Duration (s)".to_string(),
                    format!("{:.2}", plain.as_secs_f64()),
                    format!("{:.2}", cached.as_secs_f64()),
                ])
                .add_row([
                    "Rate".to_string(),
                    format!("{:.2}x", plain.as_secs_f64() / cached.as_secs_f64()),
                    "1.00x".to_string(),
                ]);

            for col in 1..=2 {
                if let Some(column) = table.column_mut(col) {
                    column.set_cell_alignment(CellAlignment::Right);
                }
            }

            let summary = format!(
                "*** {} ***\nWith driver: {}\n{}\n",
                chrono::Local::now().naive_local(),
                driver_name,
                table
            );

            if let Some(stats_output) = cli.output() {
                let mut file = BufWriter::new(std::fs::File::options().create(true).append(true).open(&stats_output)?);
                write!(file, "\n{summary}")?;
            }

            let summary = summary.trim_end();
            for summary_line in summary.lines() {
                self.report_info(summary_line.to_string());
            }
        }

        if cli.test() {
            self.test_db(db_plain, db_cached).await?;
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
        self.report_info(format!("Script file '{}' loaded successfully.", script_file.display()));
        let script: Vec<Step> = postcard::from_bytes(&buf)?;
        self.report_info("Script extracted successfully.");

        Ok(script)
    }

    #[cfg(feature = "sqlite")]
    fn db_dir(&self) -> Result<PathBuf, SimErrorAny> {
        self.cli()?
            .sqlite_path()
            .as_ref()
            .cloned()
            .map_or_else(|| self.tempdir().map(|t| t.path().to_path_buf()), Ok)
    }

    #[cfg(any(feature = "pg", feature = "sqlite"))]
    #[instrument(level = "trace", skip(script, self))]
    async fn execute_per_db(&self, script: Vec<Step>) -> Result<(), SimErrorAny> {
        let cli = self.cli()?;
        let script = Arc::new(script);
        let mut performed = false;
        let mut drivers = Vec::new();

        #[cfg(feature = "sqlite")]
        if cli.sqlite() {
            let db_plain = Sqlite::connect(&self.db_dir()?, "test_company_plain.db").await?;
            let db_cached = Sqlite::connect(&self.db_dir()?, "test_company_cached.db").await?;
            performed = true;
            self.execute_script(db_plain, db_cached, script.clone()).await?;
        }
        else {
            drivers.push("--sqlite");
        }

        #[cfg(not(feature = "sqlite"))]
        if cli.sqlite() {
            return Err(simerr!(
                "SQLite support is not enabled. Use `cargo run --feature sqlite ...` to enable it or try with --container."
            ));
        }

        #[cfg(feature = "pg")]
        if cli.pg() {
            let db_plain = Pg::builder()
                .host(cli.pg_host())
                .port(cli.pg_port())
                .user(cli.pg_user())
                .password(cli.pg_password())
                .database(format!("{}_plain", cli.pg_db_prefix()))
                .build()?;
            db_plain.connect().await?;
            let db_cached = Pg::builder()
                .host(cli.pg_host())
                .port(cli.pg_port())
                .user(cli.pg_user())
                .password(cli.pg_password())
                .database(format!("{}_cached", cli.pg_db_prefix()))
                .build()?;
            db_cached.connect().await?;
            performed = true;
            self.execute_script(db_plain, db_cached, script.clone()).await?;
        }
        else {
            drivers.push("--pg");
        }

        #[cfg(not(feature = "pg"))]
        if cli.pg() {
            return Err(simerr!(
                "PostgreSQL support is not enabled. Use `cargo run --feature pg ...` to enable it or try with --container."
            ));
        }

        if !performed {
            // `drivers`` can't be empty here because if neither DB feature is enabled, compilation will fail. If the
            // code compiles, then at least one DB was attempted above, failed, and was pushed to the list.
            if drivers.len() < 2 {
                return Err(simerr!("No database driver selected. Use {} option.", drivers[0]));
            }
            else {
                return Err(simerr!(
                    "No database driver selected. Use one of {} to select a driver.",
                    drivers.join(", ")
                ));
            }
        }

        Ok(())
    }

    #[cfg(all(feature = "tracing", feature = "tracing-otlp"))]
    fn resource() -> opentelemetry_sdk::Resource {
        use opentelemetry::KeyValue;
        use opentelemetry_semantic_conventions::attribute::DEPLOYMENT_ENVIRONMENT_NAME;
        use opentelemetry_semantic_conventions::attribute::SERVICE_VERSION;
        use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
        use opentelemetry_semantic_conventions::SCHEMA_URL;

        opentelemetry_sdk::Resource::builder()
            .with_service_name(env!("CARGO_PKG_NAME"))
            .with_schema_url(
                [
                    KeyValue::new(SERVICE_NAME, "wb_cache::company"),
                    KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
                    KeyValue::new(DEPLOYMENT_ENVIRONMENT_NAME, "develop"),
                ],
                SCHEMA_URL,
            )
            .build()
    }

    #[cfg(all(feature = "tracing", feature = "tracing-otlp"))]
    fn init_meter_provider(&self) -> Result<opentelemetry_sdk::metrics::SdkMeterProvider, SimErrorAny> {
        use opentelemetry::global;
        use opentelemetry_sdk::metrics::MeterProviderBuilder;
        use opentelemetry_sdk::metrics::PeriodicReader;

        let exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_temporality(opentelemetry_sdk::metrics::Temporality::default())
            .build()
            .unwrap();

        let reader = PeriodicReader::builder(exporter)
            .with_interval(std::time::Duration::from_secs(30))
            .build();

        // For debugging in development
        // let stdout_reader = PeriodicReader::builder(opentelemetry_stdout::MetricExporter::default()).build();

        let meter_provider = MeterProviderBuilder::default()
            .with_resource(Self::resource())
            .with_reader(reader)
            // .with_reader(stdout_reader)
            .build();

        global::set_meter_provider(meter_provider.clone());

        Ok(meter_provider)
    }

    #[cfg(all(feature = "tracing", feature = "tracing-otlp"))]
    fn init_tracer_provider(&self) -> Result<opentelemetry_sdk::trace::SdkTracerProvider, SimErrorAny> {
        use opentelemetry_sdk::trace::RandomIdGenerator;
        use opentelemetry_sdk::trace::Sampler;
        use opentelemetry_sdk::trace::SdkTracerProvider;

        let exporter = opentelemetry_otlp::SpanExporter::builder().with_tonic().build()?;

        Ok(SdkTracerProvider::builder()
            // Customize sampling strategy
            .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(1.0))))
            // If export trace to AWS X-Ray, you can use XrayIdGenerator
            .with_id_generator(RandomIdGenerator::default())
            .with_resource(Self::resource())
            .with_batch_exporter(exporter)
            .build())
    }

    #[cfg(all(feature = "tracing", feature = "tracing-otlp"))]
    #[allow(clippy::type_complexity)]
    fn setup_tracing_otlp<R>(
        &self,
        registry: R,
    ) -> Result<
        tracing_subscriber::layer::Layered<
            tracing_opentelemetry::MetricsLayer<
                tracing_subscriber::layer::Layered<
                    tracing_opentelemetry::OpenTelemetryLayer<R, opentelemetry_sdk::trace::Tracer>,
                    R,
                >,
            >,
            tracing_subscriber::layer::Layered<
                tracing_opentelemetry::OpenTelemetryLayer<R, opentelemetry_sdk::trace::Tracer>,
                R,
            >,
        >,
        SimErrorAny,
    >
    where
        R: tracing_subscriber::layer::SubscriberExt + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        use opentelemetry::trace::TracerProvider;
        use tracing_opentelemetry::MetricsLayer;
        use tracing_opentelemetry::OpenTelemetryLayer;
        use tracing_subscriber::layer::SubscriberExt;

        let meter_provider = self.init_meter_provider()?;
        let otlp_exporter = opentelemetry_otlp::SpanExporter::builder().with_tonic().build()?;
        let _ = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_simple_exporter(otlp_exporter)
            .build();

        let tracer_provider = self.init_tracer_provider()?;
        let tracer = tracer_provider.tracer("wb_cache::company");

        Ok(registry
            .with(OpenTelemetryLayer::new(tracer))
            .with(MetricsLayer::new(meter_provider.clone())))
    }

    #[cfg(all(feature = "tracing", feature = "tracing-loki"))]
    fn setup_tracing_loki<R>(
        &self,
        registry: R,
    ) -> Result<tracing_subscriber::layer::Layered<tracing_loki::Layer, R>, SimErrorAny>
    where
        R: tracing_subscriber::layer::SubscriberExt + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        use std::process;

        let url = self.cli()?.loki_url();

        let (loki, loki_task) = tracing_loki::builder()
            .label("app", "wb_cache::company")?
            .extra_field("pid", format!("{}", process::id()))?
            .build_url(url)?;

        tokio::spawn(loki_task);

        Ok(registry.with(loki))
    }

    #[cfg(all(feature = "tracing", feature = "tracing-file"))]
    #[allow(clippy::type_complexity)]
    fn setup_tracing_file<R>(
        &self,
        registry: R,
    ) -> Result<
        tracing_subscriber::layer::Layered<
            tracing_subscriber::fmt::Layer<
                R,
                tracing_subscriber::fmt::format::DefaultFields,
                tracing_subscriber::fmt::format::Format,
                ::std::sync::Mutex<Box<dyn std::io::Write + Send + 'static>>,
            >,
            R,
        >,
        SimErrorAny,
    >
    where
        R: tracing_subscriber::layer::SubscriberExt + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        use std::io;
        use std::sync::Mutex;
        use tracing_subscriber::fmt::format::FmtSpan;

        let cli = self.cli()?;

        let dest_writer = Mutex::new(if let Some(log_file) = cli.log_file() {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(log_file)?;
            Box::new(file) as Box<dyn io::Write + Send>
        }
        else {
            Box::new(io::stdout()) as Box<dyn io::Write + Send>
        });

        Ok(registry.with(
            tracing_subscriber::fmt::layer()
                .with_writer(dest_writer)
                .with_span_events(FmtSpan::FULL),
        ))
    }

    #[cfg(feature = "tracing")]
    fn setup_tracing(&self) -> Result<(), SimErrorAny> {
        use tracing::info;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let filter = tracing_subscriber::EnvFilter::from_default_env();

        let tracing_registry = tracing_subscriber::registry();
        let tracing_registry = tracing_registry.with(filter);

        #[cfg(all(feature = "tracing", feature = "tracing-otlp"))]
        let tracing_registry = self.setup_tracing_otlp(tracing_registry)?;

        #[cfg(all(feature = "tracing", feature = "tracing-loki"))]
        let tracing_registry = self.setup_tracing_loki(tracing_registry)?;

        #[cfg(all(feature = "tracing", feature = "tracing-file"))]
        let tracing_registry = self.setup_tracing_file(tracing_registry)?;

        tracing_registry.try_init()?;

        info!("Tracing initialized");

        Ok(())
    }

    pub async fn execute(&self) -> Result<(), SimErrorAny> {
        let cli = match self.cli() {
            Ok(cli) => cli,
            Err(err) => match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                    let mut cmd = Cli::command();
                    // let mut cmd = cmd.color(clap::ColorChoice::Always);
                    cmd.print_help().unwrap();
                    return Ok(());
                }
                _ => {
                    return Err(err.into());
                }
            },
        };

        self.validate()?;

        if cli.container() {
            return self.run_container().await;
        }

        #[cfg(feature = "tracing")]
        self.setup_tracing()?;

        if cli.save() {
            let myself = self.myself().unwrap();
            return tokio::task::spawn_blocking(move || myself.save_script()).await?;
        }

        #[cfg(any(feature = "pg", feature = "sqlite"))]
        {
            let script = if cli.load() {
                self.load_script()?
            }
            else {
                let s = self.script_writer()?.create()?;
                self.clear_script_writer();
                s
            };

            self.execute_per_db(script).await?;
        }

        Ok(())
    }

    pub async fn run_container(&self) -> Result<(), SimErrorAny> {
        let cli = self.cli()?;

        let mut cmd = tokio::process::Command::new("docker");

        let mut profiles = Vec::new();

        if let Some(script) = cli.script() {
            cmd.env("WBCACHE_SCRIPT", script);
        }

        if let Some(output) = cli.output() {
            cmd.env("WBCACHE_OUTPUT", output);
        }

        cmd.env("WBCACHE_QUIET", cli.quiet().to_string())
            .env("WBCACHE_PERIOD", cli.period().to_string())
            .env("WBCACHE_PRODUCTS", cli.products().to_string())
            .env("WBCACHE_INITIAL_CUSTOMERS", cli.initial_customers().to_string())
            .env("WBCACHE_MARKET_CAPACITY", cli.market_capacity().to_string())
            .env("WBCACHE_INFLECTION_POINT", cli.inflection_point().to_string())
            .env("WBCACHE_GROWTH_RATE", cli.growth_rate().to_string())
            .env("WBCACHE_MIN_CUSTOMER_ORDERS", cli.min_customer_orders().to_string())
            .env("WBCACHE_MAX_CUSTOMER_ORDERS", cli.max_customer_orders().to_string())
            .env("WBCACHE_RETURN_WINDOW", cli.return_window().to_string())
            .env("WBCACHE_SAVE", cli.save().to_string())
            .env("WBCACHE_LOAD", cli.load().to_string())
            .env("WBCACHE_TEST", cli.test().to_string())
            .env("WBCACHE_SQLITE", cli.sqlite().to_string())
            .env("WBCACHE_PG", cli.pg().to_string())
            .env("WBCACHE_PG_HOST", cli.pg_host())
            .env("WBCACHE_PG_PORT", cli.pg_port().to_string())
            .env("WBCACHE_PG_USER", cli.pg_user())
            .env("WBCACHE_PG_PASSWORD", cli.pg_password())
            .env("WBCACHE_PG_DB_PREFIX", cli.pg_db_prefix())
            .env("WBCACHE_LOKI_URL", cli.loki_url());

        if cli.sqlite() {
            profiles.push("sqlite");
        }

        if let Some(sqlite_path) = cli.sqlite_path() {
            cmd.env("WBCACHE_SQLITE_PATH", sqlite_path);
        }

        if cli.pg() {
            profiles.push("pg");
        }

        if let Some(log_file) = cli.log_file() {
            cmd.env("WBCACHE_LOG_FILE", log_file);
        }

        cmd.arg("compose");

        for profile in profiles {
            cmd.arg("--profile").arg(profile);
        }

        cmd.arg("up").arg("--build");

        let mut child = cmd.kill_on_drop(true).spawn()?;

        let mut kill_count = 0;

        while kill_count < 3 {
            tokio::select!(
                status = child.wait() => {
                    if let Err(e) = status {
                        let err = SimErrorAny::from(e);
                        err.context("Failed to wait for the container process");
                        return Err(err);
                    }
                    break;
                },
                _ = signal::ctrl_c() => {
                    println!("Received Ctrl+C, stopping the container...");
                    let _ = child.kill().await;
                    kill_count += 1;
                }
            );
        }

        Ok(())
    }

    pub async fn run() -> Result<(), SimErrorAny> {
        EcommerceApp::__fieldx_new().execute().await
    }
}

impl EcommerceAppBuilder {
    pub fn cli_args<S: ToString>(self, args: Vec<S>) -> Self {
        self._cli_args(args.into_iter().map(|s| s.to_string()).collect())
    }
}

impl Debug for EcommerceApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SimApp {{ ... }}")
    }
}

impl SimulationApp for EcommerceApp {
    fn acquire_progress<'a>(
        &'a self,
        style: PStyle,
        order: Option<POrder<'a>>,
    ) -> Result<Option<ProgressBar>, SimErrorAny> {
        Ok(self.progress_ui()?.acquire_progress(style, order))
    }

    fn set_cached_status(&self, status: ActorStatus) {
        self._set_cached_status(status);
    }

    fn set_plain_status(&self, status: ActorStatus) {
        self._set_plain_status(status);
    }

    fn report_info<S: ToString>(&self, msg: S) {
        self.progress_ui().unwrap().report_info(msg.to_string());
    }

    fn report_debug<S: ToString>(&self, msg: S) {
        self.progress_ui().unwrap().report_debug(msg.to_string());
    }

    fn report_warn<S: ToString>(&self, msg: S) {
        self.progress_ui().unwrap().report_warn(msg.to_string());
    }

    fn report_error<S: ToString>(&self, msg: S) {
        self.progress_ui().unwrap().report_error(msg.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parsing() {
        let args = vec!["cmd", "--quiet", "--test", "--products", "5", "--period", "30"];
        let cli = Cli::try_parse_from(args).expect("Failed to parse CLI arguments");
        assert_eq!(cli.products(), 5);
        assert_eq!(cli.period(), 30);
        assert!(cli.quiet());
        assert!(cli.test());
    }
}
