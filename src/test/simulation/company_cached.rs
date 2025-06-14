use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use fieldx_plus::child_build;
use fieldx_plus::fx_plus;
use indicatif::ProgressBar;
use sea_orm::prelude::*;
use sea_orm::ActiveValue;
use sea_orm::DatabaseConnection;
use sea_orm::EntityTrait;
use sea_orm::QuerySelect;
use tracing::debug;
use tracing::instrument;

use crate::cache;
use crate::test::simulation::sim_app::ActorStatus;
use crate::traits::Observer;
use crate::update_iterator::UpdateIterator;
use crate::Cache;

use super::actor::TestActor;
use super::db::cache::CacheUpdates;
use super::db::cache::DBProvider;
use super::db::driver::DatabaseDriver;
use super::db::entity::customer::CustomerBy;
use super::db::entity::session;
use super::db::entity::*;
use super::progress::MaybeProgress;
use super::progress::PStyle;
use super::scriptwriter::steps::ScriptTitle;
use super::types::simerr;
use super::types::OrderStatus;
use super::types::SimError;
use super::types::SimErrorAny;
use super::SimulationApp;

type CustomerCache<APP, D> = Arc<Cache<CustomerMgr<TestCompany<APP, D>>>>;
type InvRecCache<APP, D> = Arc<Cache<InventoryRecordMgr<TestCompany<APP, D>>>>;
type OrderCache<APP, D> = Arc<Cache<OrderMgr<TestCompany<APP, D>>>>;
type ProductCache<APP, D> = Arc<Cache<ProductMgr<TestCompany<APP, D>>>>;
type SessionCache<APP, D> = Arc<Cache<SessionMgr<TestCompany<APP, D>>>>;

#[fx_plus(child(TestCompany<APP, D>, unwrap), sync)]
struct OrderObserver<APP, D>
where
    APP: SimulationApp,
    D: DatabaseDriver, {}

#[async_trait]
impl<APP, D> Observer<OrderMgr<TestCompany<APP, D>>> for OrderObserver<APP, D>
where
    APP: SimulationApp,
    D: DatabaseDriver,
{
    async fn on_flush(&self, _updates: Arc<UpdateIterator<OrderMgr<TestCompany<APP, D>>>>) -> Result<(), SimErrorAny> {
        let parent = self.parent();

        // parent
        //     .app()?
        //     .report_debug(format!("OrderObserver::on_flush: {}", _updates.len()));
        debug!("OrderObserver::on_flush: {}", _updates.len());

        parent.customer_cache()?.flush_raw().await?;
        Ok(())
    }

    async fn on_flush_one(
        &self,
        key: &Uuid,
        update: &CacheUpdates<super::db::entity::order::ActiveModel>,
    ) -> Result<(), Arc<SimErrorAny>> {
        // self.parent()
        //     .app()?
        //     .report_debug(format!("OrderObserver::on_flush_one: {}", key));
        debug!("OrderObserver::on_flush_one: {}", key);

        match update {
            CacheUpdates::Insert(am) | CacheUpdates::Update(am) => {
                let company = self.parent();

                match am.customer_id {
                    ActiveValue::Set(_customer_id) | ActiveValue::Unchanged(_customer_id) => {
                        company.customer_cache()?.flush().await?;
                    }
                    _ => (),
                }
                match am.product_id {
                    ActiveValue::Set(_product_id) | ActiveValue::Unchanged(_product_id) => {
                        company.product_cache()?.flush().await?;
                    }
                    _ => (),
                }
            }
            CacheUpdates::Delete => (),
        }
        Ok(())
    }

    async fn on_monitor_error(&self, error: &Arc<SimErrorAny>) {
        self.parent()
            .app()
            .unwrap()
            .report_error(format!("OrderObserver::on_monitor_error: {error:?}"));
    }

    async fn on_debug(&self, message: &str) {
        debug!("[orders] {}", message);
        // self.parent()
        //     .app()
        //     .unwrap()
        //     .report_debug(format!("[orders] {}", message));
    }
}

#[derive(Debug)]
#[fx_plus(child(TestCompany<APP,D>, unwrap), sync)]
struct SessionObserver<APP, D>
where
    APP: SimulationApp,
    D: DatabaseDriver, {}

#[async_trait]
impl<APP, D> Observer<SessionMgr<TestCompany<APP, D>>> for SessionObserver<APP, D>
where
    APP: SimulationApp,
    D: DatabaseDriver,
{
    async fn on_flush(&self, updates: Arc<UpdateIterator<SessionMgr<TestCompany<APP, D>>>>) -> Result<(), SimErrorAny> {
        // self.parent()
        //     .app()?
        //     .report_debug(format!("SessionObserver::on_flush: {}", updates.len()));
        debug!("SessionObserver::on_flush: {}", updates.len());

        let mut customer_ids: HashSet<CustomerBy> = HashSet::new();
        while let Some(update) = updates.next() {
            match update.update() {
                CacheUpdates::Insert(am) | CacheUpdates::Update(am) => match am.customer_id {
                    ActiveValue::Set(Some(customer_id)) | ActiveValue::Unchanged(Some(customer_id)) => {
                        customer_ids.insert(CustomerBy::Id(customer_id));
                    }
                    _ => (),
                },
                CacheUpdates::Delete => (),
            }
        }
        self.parent()
            .customer_cache()?
            .flush_many_raw(customer_ids.into_iter().collect::<Vec<_>>())
            .await?;
        Ok(())
    }

    async fn on_flush_one(
        &self,
        key: &i64,
        update: &CacheUpdates<super::db::entity::session::ActiveModel>,
    ) -> Result<(), Arc<SimErrorAny>> {
        // self.parent()
        //     .app()?
        //     .report_debug(format!("SessionObserver::on_flush_one: {}", key));
        debug!("SessionObserver::on_flush_one: {}", key);

        match update {
            CacheUpdates::Insert(am) | CacheUpdates::Update(am) => match am.customer_id {
                ActiveValue::Set(Some(customer_id)) | ActiveValue::Unchanged(Some(customer_id)) => {
                    self.parent()
                        .customer_cache()?
                        .flush_one(&CustomerBy::Id(customer_id))
                        .await?;
                }
                _ => (),
            },
            CacheUpdates::Delete => (),
        }
        Ok(())
    }

    async fn on_monitor_error(&self, error: &Arc<SimErrorAny>) {
        self.parent()
            .app()
            .unwrap()
            .report_error(format!("SessionObserver::on_monitor_error: {error:?}"));
    }

    async fn on_debug(&self, message: &str) {
        debug!("[sessions] {}", message);
        // self.parent()
        //     .app()
        //     .unwrap()
        //     .report_debug(format!("[sessions] {}", message));
    }
}

#[fx_plus(
    agent(APP, unwrap(or_else(SimErrorAny, <APP as SimulationApp>::app_is_gone()))),
    parent,
    fallible(off, error(SimErrorAny)),
    sync,
    default(off)
)]
#[allow(clippy::type_complexity)]
pub struct TestCompany<APP: SimulationApp, D: DatabaseDriver> {
    #[fieldx(copy, get("_current_day"), inner_mut, set("_set_current_day", private), default(0))]
    current_day: i32,

    #[fieldx(optional, private, inner_mut, get("_product_count", copy), set, builder(off))]
    product_count: i32,

    #[fieldx(optional, private, inner_mut, get("_market_capacity", copy), set, builder(off))]
    market_capacity: u32,

    #[fieldx(lazy, get("_progress", private, clone), fallible)]
    progress: Arc<Option<ProgressBar>>,

    #[fieldx(get(clone), builder(required))]
    db: Arc<D>,

    #[fieldx(inner_mut, get(copy), set, default(0))]
    inv_check_no: u32,

    #[fieldx(inner_mut, get, get_mut, default(HashMap::new()))]
    updated_from: HashMap<Uuid, Order>,

    #[fieldx(lazy, fallible, get(clone))]
    customer_cache: CustomerCache<APP, D>,

    #[fieldx(lazy, fallible, get(clone))]
    inv_rec_cache: InvRecCache<APP, D>,

    #[fieldx(lazy, fallible, get(clone))]
    order_cache: OrderCache<APP, D>,

    #[fieldx(lazy, fallible, get(clone))]
    product_cache: ProductCache<APP, D>,

    #[fieldx(lazy, fallible, get(clone))]
    session_cache: SessionCache<APP, D>,

    #[fieldx(inner_mut, set, get(copy), builder(off), default(Instant::now()))]
    started: Instant,
}

impl<APP, D> Debug for TestCompany<APP, D>
where
    APP: SimulationApp,
    D: DatabaseDriver,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestCompany")
            .field("current_day", &self.current_day())
            .field("product_count", &self.product_count())
            .finish()
    }
}

impl<APP: SimulationApp, D: DatabaseDriver> TestCompany<APP, D> {
    fn build_progress(&self) -> Result<Arc<Option<ProgressBar>>, SimErrorAny> {
        let app = self.app()?;
        let progress = app.acquire_progress(PStyle::Main, None)?;

        Ok(Arc::new(progress))
    }

    fn build_customer_cache(&self) -> Result<CustomerCache<APP, D>, SimErrorAny> {
        let customer_cache = child_build!(self, CustomerMgr<TestCompany<APP, D>>)?;
        Ok(Cache::builder()
            .name("customers")
            .data_controller(customer_cache)
            .max_updates(self.market_capacity()? as u64)
            .max_capacity(self.market_capacity()? as u64)
            .flush_interval(Duration::from_secs(600))
            .build()?)
    }

    fn build_inv_rec_cache(&self) -> Result<InvRecCache<APP, D>, SimErrorAny> {
        let inv_rec_cache = child_build!(self, InventoryRecordMgr<TestCompany<APP, D>>)?;
        Ok(Cache::builder()
            .name("inventory records")
            .data_controller(inv_rec_cache)
            .max_updates(self.product_count()? as u64)
            .max_capacity(self.product_count()? as u64)
            .flush_interval(Duration::from_secs(600))
            .build()?)
    }

    fn build_order_cache(&self) -> Result<OrderCache<APP, D>, SimErrorAny> {
        let order_cache = child_build!(self, OrderMgr<TestCompany<APP, D>>)?;
        let order_observer = child_build!(self, OrderObserver<APP,D>)?;
        // Cache size is set based on the expectation that we may need to re-process one order per day per customer.
        // Re-process means handling a refund or shipping a backordered one.
        Ok(Cache::builder()
            .name("orders")
            .data_controller(order_cache)
            .max_updates(self.market_capacity()? as u64 * 100)
            .max_capacity((self.market_capacity()? as u64 * 1000).max(1_000_000))
            .flush_interval(Duration::from_secs(600))
            .observer(order_observer)
            .build()?)
    }

    fn build_product_cache(&self) -> Result<ProductCache<APP, D>, SimErrorAny> {
        let product_cache = child_build!(self, ProductMgr<TestCompany<APP, D>>)?;
        Ok(Cache::builder()
            .name("products")
            .data_controller(product_cache)
            .max_updates(self.product_count()? as u64)
            .max_capacity(self.product_count()? as u64)
            .flush_interval(Duration::from_secs(60))
            .build()?)
    }

    fn build_session_cache(&self) -> Result<SessionCache<APP, D>, SimErrorAny> {
        let session_cache = child_build!(self, SessionMgr<TestCompany<APP, D>>)?;
        let session_observer = child_build!(self, SessionObserver<APP,D>)?;
        Ok(Cache::builder()
            .name("sessions")
            .data_controller(session_cache)
            .max_updates((self.market_capacity()? as u64 * 100).max(100_000))
            .max_capacity((self.market_capacity()? as u64 * 1000).max(1_000_000))
            .flush_interval(Duration::from_secs(600))
            .observer(session_observer)
            .build()?)
    }

    fn product_count(&self) -> Result<i32, SimErrorAny> {
        self._product_count().ok_or_else(|| simerr!("Product count is not set"))
    }

    fn market_capacity(&self) -> Result<u32, SimErrorAny> {
        self._market_capacity()
            .ok_or_else(|| simerr!("Market capacity is not set"))
    }

    #[instrument(level = "trace", skip(db))]
    async fn update_inventory(&self, db: &DatabaseConnection, order: &Order) -> Result<(), SimError> {
        match order.status {
            OrderStatus::Backordered | OrderStatus::Refunded | OrderStatus::Shipped | OrderStatus::Recheck => (),
            _ => {
                if self.updated_from().contains_key(&order.id) {
                    // Prevent accidental double update
                    Err(simerr!(
                        "Inventory already updated from order {:?}",
                        self.updated_from().get(&order.id)
                    ))?;
                }
                self.updated_from_mut().insert(order.id, order.clone());
                self.update_inventory_record(db, order.product_id, -(order.quantity as i64))
                    .await?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl<APP, D> TestActor<APP> for TestCompany<APP, D>
where
    APP: SimulationApp,
    D: DatabaseDriver,
{
    fn prelude(&self) -> Result<(), SimError> {
        self.set_started(Instant::now());
        self.progress()?.maybe_set_prefix("Cached");
        Ok(())
    }

    async fn set_current_day(&self, day: i32) -> Result<(), SimError> {
        if day == 1 {
            self.customer_cache()?.flush().await?;
            self.product_cache()?.flush().await?;
            self.inv_rec_cache()?.flush().await?;
        }
        self._set_current_day(day);
        self.session_cache()?.soft_flush().await?;
        self.order_cache()?.soft_flush().await?;
        Ok(())
    }

    #[inline(always)]
    fn current_day(&self) -> i32 {
        self._current_day()
    }

    fn progress(&self) -> Result<Arc<Option<ProgressBar>>, SimError> {
        Ok(self._progress()?)
    }

    fn set_title(&self, title: &ScriptTitle) -> Result<(), SimError> {
        self.set_product_count(title.products);
        self.set_market_capacity(title.market_capacity);
        Ok(())
    }

    #[instrument(level = "trace", skip(self, _db))]
    async fn add_customer(&self, _db: &DatabaseConnection, customer: &Customer) -> Result<(), SimError> {
        let mut customer = customer.clone();
        customer.registered_on = self.current_day();
        self.customer_cache()?.insert(customer.clone()).await?;
        Ok(())
    }

    #[instrument(level = "trace", skip(self, _db))]
    async fn add_product(&self, _db: &DatabaseConnection, product: &Product) -> Result<(), SimError> {
        self.product_cache()?.insert(product.clone()).await?;
        Ok(())
    }

    #[instrument(level = "trace", skip(self, _db))]
    async fn add_inventory_record(
        &self,
        _db: &DatabaseConnection,
        inventory_record: &InventoryRecord,
    ) -> Result<(), SimError> {
        self.product_cache()?.flush().await?;
        self.inv_rec_cache()?.insert(inventory_record.clone()).await?;
        Ok(())
    }

    #[instrument(level = "trace", skip(self, db))]
    async fn add_order(&self, db: &DatabaseConnection, order: &Order) -> Result<(), SimError> {
        debug!(
            "Adding order {} on product {}: {}, {:?}",
            order.id, order.product_id, order.quantity, order.status
        );
        self.update_inventory(db, order).await?;

        let mut order = order.clone();
        order.purchased_on = self.current_day();
        self.order_cache()?.insert(order.clone()).await?;

        Ok(())
    }

    #[instrument(level = "trace", skip(self, _db))]
    async fn add_session(&self, _db: &DatabaseConnection, session: &Session) -> Result<(), SimError> {
        debug!("Adding session {} for customer {:?}", session.id, session.customer_id);
        self.session_cache()?.insert(session.clone()).await?;
        Ok(())
    }

    #[instrument(level = "trace", skip(self, _db))]
    async fn check_inventory(
        &self,
        _db: &DatabaseConnection,
        product_id: i32,
        stock: i64,
        comment: &str,
    ) -> Result<(), SimError> {
        let inventory_record = self.inv_rec_cache()?.get(&product_id).await?;

        self.inv_rec_compare(&inventory_record, product_id, stock, comment)?;

        let inv_check_no = self.inv_check_no() + 1;
        self.set_inv_check_no(inv_check_no);

        Ok(())
    }

    #[instrument(level = "trace", skip(self, _db))]
    async fn update_inventory_record(
        &self,
        _db: &DatabaseConnection,
        product_id: i32,
        quantity: i64,
    ) -> Result<(), SimError> {
        self.inv_rec_cache()?
            .entry(product_id)
            .await?
            .and_try_compute_with(|entry| async {
                if let Some(entry) = entry {
                    let mut inventory_record = entry.into_value();
                    let new_stock = inventory_record.stock + quantity;
                    if new_stock < 0 {
                        return Err(simerr!(
                            "Not enough stock for product ID {}: need {}, but only {} remaining",
                            product_id,
                            -quantity,
                            inventory_record.stock
                        ));
                    }
                    inventory_record.stock = new_stock;
                    Ok(cache::Op::Put(inventory_record))
                }
                else {
                    Err(simerr!(
                        "Can't update non-existing inventory record for product ID: {}",
                        product_id
                    ))
                }
            })
            .await?;

        Ok(())
    }

    #[instrument(level = "trace", skip(self, db))]
    async fn update_order(&self, db: &DatabaseConnection, order_update: &Order) -> Result<(), SimError> {
        debug!(
            "Updating order {} on product {}: {}, {:?}",
            order_update.id, order_update.product_id, order_update.quantity, order_update.status
        );
        self.order_cache()?
            .entry(order_update.id)
            .await?
            .and_try_compute_with(|entry| async {
                if let Some(entry) = entry {
                    self.update_inventory(db, order_update).await?;
                    let mut order: Order = entry.into_value();
                    order.status = order_update.status;
                    Ok(cache::Op::Put(order))
                }
                else {
                    Err(simerr!("Can't update non-existing order for ID: {}", order_update.id))
                }
            })
            .await?;

        Ok(())
    }

    #[instrument(level = "trace", skip(self, _db))]
    async fn update_product_view_count(&self, _db: &DatabaseConnection, product_id: i32) -> Result<(), SimError> {
        self.product_cache()?
            .entry(product_id)
            .await?
            .and_try_compute_with(|entry| async {
                if let Some(entry) = entry {
                    let mut product = entry.into_value();
                    product.views += 1;
                    Ok(cache::Op::Put(product))
                }
                else {
                    Err(simerr!("Can't update non-existing product for ID: {}", product_id))
                }
            })
            .await?;

        Ok(())
    }

    #[instrument(level = "trace", skip(self, _db))]
    async fn update_session(&self, _db: &DatabaseConnection, session_update: &Session) -> Result<(), SimError> {
        debug!(
            "Updating session {} for customer {:?}",
            session_update.id, session_update.customer_id
        );
        self.session_cache()?
            .entry(session_update.id)
            .await?
            .and_try_compute_with(|entry| async {
                if let Some(entry) = entry {
                    let mut session = entry.into_value();
                    session.customer_id = session_update.customer_id;
                    session.expires_on = session_update.expires_on;
                    Ok(cache::Op::Put(session))
                }
                else {
                    Err(simerr!(
                        "Can't update non-existing session for ID: {}",
                        session_update.id
                    ))
                }
            })
            .await?;

        Ok(())
    }

    #[instrument(level = "trace", skip(self, db))]
    async fn collect_sessions(&self, db: &DatabaseConnection) -> Result<(), SimError> {
        // It is safe to drop expired sessions that have no user ID set because they cannot become valid.  For sessions
        // with a user ID, extra caution is required; do not directly delete those that are currently in the cache.
        self.collect_session_stubs(db).await?;

        let session_cache = self.session_cache()?;
        let user_sessions = Sessions::find()
            .select_only()
            .column(session::Column::Id)
            .filter(session::Column::CustomerId.is_not_null())
            .into_tuple::<i64>()
            .all(db)
            .await?;

        for session_id in user_sessions {
            session_cache
                .entry(session_id)
                .await?
                .and_try_compute_with(|entry| async {
                    if let Some(entry) = entry {
                        let session = entry.into_value();
                        if session.expires_on <= self.current_day() {
                            // Session expired
                            Ok(cache::Op::Remove)
                        }
                        else {
                            // Session is still valid, do nothing
                            Ok(cache::Op::Nop)
                        }
                    }
                    else {
                        // If the session ID was found in the database but not in the cache, it indicates that it was
                        // previously deleted but hadn't been flushed yet.
                        // The scenario of a bug in the Cache implementation is not considered here.
                        Ok(cache::Op::Nop)
                    }
                })
                .await?;
        }

        Ok(())
    }

    async fn step_complete(&self, _db: &DatabaseConnection, step_num: usize) -> Result<(), SimError> {
        let elapsed = self.started().elapsed().as_secs_f64();
        self.app()?
            .set_cached_status(ActorStatus::new(step_num as f64 / elapsed, step_num));
        Ok(())
    }

    #[instrument(level = "trace", skip(self))]
    async fn curtain_call(&self) -> Result<(), SimError> {
        self.app()?.report_info("Shutting down caches...");

        self.customer_cache()?.close().await?;
        self.product_cache()?.close().await?;
        self.inv_rec_cache()?.close().await?;
        self.order_cache()?.close().await?;
        self.session_cache()?.close().await?;

        Ok(())
    }
}

impl<APP, D> DBProvider for TestCompany<APP, D>
where
    APP: SimulationApp,
    D: DatabaseDriver,
{
    fn db_driver(&self) -> Result<Arc<impl DatabaseDriver>, SimErrorAny> {
        Ok(self.db())
    }

    fn db_connection(&self) -> Result<DatabaseConnection, SimErrorAny> {
        Ok(self.db().connection())
    }
}
