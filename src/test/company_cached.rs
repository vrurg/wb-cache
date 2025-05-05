use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fieldx_plus::child_build;
use fieldx_plus::fx_plus;
use indicatif::ProgressBar;
use sea_orm::prelude::*;
use sea_orm::ActiveValue;
use sea_orm::DatabaseConnection;
use sea_orm::EntityTrait;
use sea_orm::QuerySelect;

use crate::cache;
use crate::traits::WBObserver;
use crate::WBCache;

use super::actor::TestActor;
use super::db::cache::CacheUpdates;
use super::db::cache::DBConnectionProvider;
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
use super::TestApp;

#[fx_plus(child(TestCompany<APP>, unwrap), sync)]
struct OrderObserver<APP>
where
    APP: TestApp, {}

#[async_trait]
impl<APP> WBObserver<OrderMgr<TestCompany<APP>>> for OrderObserver<APP>
where
    APP: TestApp,
{
    async fn on_flush(&self) -> Result<(), SimErrorAny> {
        self.parent().customer_cache()?.flush().await?;
        Ok(())
    }

    async fn on_flush_one(
        &self,
        update: &CacheUpdates<super::db::entity::order::ActiveModel>,
    ) -> Result<(), SimErrorAny> {
        match update {
            CacheUpdates::Insert(am) | CacheUpdates::Update(am) => {
                let company = self.parent();

                match am.customer_id {
                    ActiveValue::Set(customer_id) | ActiveValue::Unchanged(customer_id) => {
                        company
                            .customer_cache()?
                            .flush_one(&CustomerBy::Id(customer_id))
                            .await?;
                    }
                    _ => (),
                }
                match am.product_id {
                    ActiveValue::Set(product_id) | ActiveValue::Unchanged(product_id) => {
                        company.product_cache()?.flush_one(&product_id).await?;
                    }
                    _ => (),
                }
            }
            CacheUpdates::Delete => (),
        }
        Ok(())
    }
}

#[fx_plus(child(TestCompany<APP>, unwrap), sync)]
struct SessionObserver<APP>
where
    APP: TestApp, {}

#[async_trait]
impl<APP> WBObserver<SessionMgr<TestCompany<APP>>> for SessionObserver<APP>
where
    APP: TestApp,
{
    async fn on_flush(&self) -> Result<(), SimErrorAny> {
        self.parent().customer_cache()?.flush().await?;
        Ok(())
    }

    async fn on_flush_one(
        &self,
        update: &CacheUpdates<super::db::entity::session::ActiveModel>,
    ) -> Result<(), SimErrorAny> {
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
}

#[fx_plus(
    agent(APP, unwrap(or_else(SimErrorAny, <APP as TestApp>::app_is_gone()))),
    parent,
    fallible(off, error(SimErrorAny)),
    sync
)]
#[allow(clippy::type_complexity)]
pub struct TestCompany<APP: TestApp> {
    #[fieldx(copy, get("_current_day"), inner_mut, set("_set_current_day", private), default(0))]
    current_day: u32,

    #[fieldx(optional, private, inner_mut, get("_product_count", copy), set, builder(off))]
    product_count: u32,

    #[fieldx(optional, private, inner_mut, get("_market_capacity", copy), set, builder(off))]
    market_capacity: u32,

    #[fieldx(lazy, get("_progress", private, clone), fallible)]
    progress: Arc<Option<ProgressBar>>,

    #[fieldx(get(clone), builder(required))]
    db: Arc<DatabaseConnection>,

    #[fieldx(inner_mut, get(copy), set, default(0))]
    inv_check_no: u32,

    #[fieldx(inner_mut, get, get_mut, default(HashMap::new()))]
    updated_from: HashMap<Uuid, Order>,

    #[fieldx(lazy, fallible, get(clone))]
    customer_cache: Arc<WBCache<CustomerMgr<TestCompany<APP>>>>,

    #[fieldx(lazy, fallible, get(clone))]
    inv_rec_cache: Arc<WBCache<InventoryRecordMgr<TestCompany<APP>>>>,

    #[fieldx(lazy, fallible, get(clone))]
    order_cache: Arc<WBCache<OrderMgr<TestCompany<APP>>>>,

    #[fieldx(lazy, fallible, get(clone))]
    product_cache: Arc<WBCache<ProductMgr<TestCompany<APP>>>>,

    #[fieldx(lazy, fallible, get(clone))]
    session_cache: Arc<WBCache<SessionMgr<TestCompany<APP>>>>,
}

impl<APP: TestApp> TestCompany<APP> {
    fn build_progress(&self) -> Result<Arc<Option<ProgressBar>>, SimErrorAny> {
        let app = self.app()?;
        let progress = app.acquire_progress(PStyle::Main, None)?;

        Ok(Arc::new(progress))
    }

    fn build_customer_cache(&self) -> Result<Arc<WBCache<CustomerMgr<TestCompany<APP>>>>, SimErrorAny> {
        let customer_cache = child_build!(self, CustomerMgr<TestCompany<APP>>)?;
        Ok(WBCache::builder()
            .name("customers")
            .data_controller(customer_cache)
            .max_updates((self.market_capacity()? as u64 / 10).min(100))
            .max_capacity(self.market_capacity()? as u64)
            .build()?)
    }

    fn build_inv_rec_cache(&self) -> Result<Arc<WBCache<InventoryRecordMgr<TestCompany<APP>>>>, SimErrorAny> {
        let inv_rec_cache = child_build!(self, InventoryRecordMgr<TestCompany<APP>>)?;
        Ok(WBCache::builder()
            .name("inventory records")
            .data_controller(inv_rec_cache)
            .max_updates(self.product_count()? as u64)
            .max_capacity(self.product_count()? as u64)
            .build()?)
    }

    fn build_order_cache(&self) -> Result<Arc<WBCache<OrderMgr<TestCompany<APP>>>>, SimErrorAny> {
        let order_cache = child_build!(self, OrderMgr<TestCompany<APP>>)?;
        let order_observer = child_build!(self, OrderObserver<APP>)?;
        // Cache size is set based on the expectation that we may need to re-process one order per day per customer.
        // Re-process means handling a refund or shipping a backordered one.
        Ok(WBCache::builder()
            .name("orders")
            .data_controller(order_cache)
            .max_updates(self.market_capacity()? as u64)
            .max_capacity(self.market_capacity()? as u64 * 30)
            .flush_interval(Duration::from_secs(60))
            .observer(order_observer)
            .build()?)
    }

    fn build_product_cache(&self) -> Result<Arc<WBCache<ProductMgr<TestCompany<APP>>>>, SimErrorAny> {
        let product_cache = child_build!(self, ProductMgr<TestCompany<APP>>)?;
        Ok(WBCache::builder()
            .name("products")
            .data_controller(product_cache)
            .max_updates(self.product_count()? as u64)
            .max_capacity(self.product_count()? as u64)
            .build()?)
    }

    fn build_session_cache(&self) -> Result<Arc<WBCache<SessionMgr<TestCompany<APP>>>>, SimErrorAny> {
        let session_cache = child_build!(self, SessionMgr<TestCompany<APP>>)?;
        let session_observer = child_build!(self, SessionObserver<APP>)?;
        Ok(WBCache::builder()
            .name("sessions")
            .data_controller(session_cache)
            .max_updates(self.market_capacity()? as u64 / 2)
            .max_capacity(self.market_capacity()? as u64 * 10)
            .observer(session_observer)
            .build()?)
    }

    fn product_count(&self) -> Result<u32, SimErrorAny> {
        self._product_count().ok_or_else(|| simerr!("Product count is not set"))
    }

    fn market_capacity(&self) -> Result<u32, SimErrorAny> {
        self._market_capacity()
            .ok_or_else(|| simerr!("Market capacity is not set"))
    }

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
impl<APP> TestActor for TestCompany<APP>
where
    APP: TestApp,
{
    fn prelude(&self) -> Result<(), SimError> {
        self.progress()?.maybe_set_prefix("cached");
        Ok(())
    }

    async fn set_current_day(&self, day: u32) -> Result<(), SimError> {
        if day == 1 {
            self.customer_cache()?.flush().await?;
            self.product_cache()?.flush().await?;
            self.inv_rec_cache()?.flush().await?;
        }
        self._set_current_day(day);
        Ok(())
    }

    fn current_day(&self) -> u32 {
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

    async fn add_customer(&self, _db: &DatabaseConnection, customer: &Customer) -> Result<(), SimError> {
        self.customer_cache()?.insert(customer.clone()).await?;
        Ok(())
    }

    async fn add_product(&self, _db: &DatabaseConnection, product: &Product) -> Result<(), SimError> {
        self.product_cache()?.insert(product.clone()).await?;
        Ok(())
    }

    async fn add_inventory_record(
        &self,
        _db: &DatabaseConnection,
        inventory_record: &InventoryRecord,
    ) -> Result<(), SimError> {
        self.product_cache()?.flush().await?;
        self.inv_rec_cache()?.insert(inventory_record.clone()).await?;
        Ok(())
    }

    async fn add_order(&self, db: &DatabaseConnection, order: &Order) -> Result<(), SimError> {
        self.update_inventory(db, order).await?;

        self.order_cache()?.insert(order.clone()).await?;
        Ok(())
    }

    async fn add_session(&self, _db: &DatabaseConnection, session: &Session) -> Result<(), SimError> {
        self.session_cache()?.insert(session.clone()).await?;
        Ok(())
    }

    async fn check_inventory(
        &self,
        _db: &DatabaseConnection,
        product_id: u32,
        stock: u32,
        comment: &str,
    ) -> Result<(), SimError> {
        let inventory_record = self.inv_rec_cache()?.get(&product_id).await?;

        self.inv_rec_compare(&inventory_record, product_id, stock, comment)?;

        let inv_check_no = self.inv_check_no() + 1;
        self.set_inv_check_no(inv_check_no);

        Ok(())
    }

    async fn update_inventory_record(
        &self,
        _db: &DatabaseConnection,
        product_id: u32,
        quantity: i64,
    ) -> Result<(), SimError> {
        self.inv_rec_cache()?
            .entry(product_id)
            .await?
            .and_try_compute_with(async |entry| {
                if let Some(entry) = entry {
                    let mut inventory_record = entry.into_value();
                    let new_stock = inventory_record.stock as i64 + quantity;
                    if new_stock < 0 {
                        return Err(simerr!(
                            "Not enough stock for product ID {}: need {}, but only {} remaining",
                            product_id,
                            -quantity,
                            inventory_record.stock
                        ));
                    }
                    inventory_record.stock = new_stock as u32;
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

    async fn update_order(&self, db: &DatabaseConnection, order_update: &Order) -> Result<(), SimError> {
        self.order_cache()?
            .entry(order_update.id)
            .await?
            .and_try_compute_with(async |entry| {
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

    async fn update_product_view_count(&self, _db: &DatabaseConnection, product_id: u32) -> Result<(), SimError> {
        self.product_cache()?
            .entry(product_id)
            .await?
            .and_try_compute_with(async |entry| {
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

    async fn update_session(&self, _db: &DatabaseConnection, session_update: &Session) -> Result<(), SimError> {
        self.session_cache()?
            .entry(session_update.id)
            .await?
            .and_try_compute_with(async |entry| {
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

    async fn collect_sessions(&self, db: &DatabaseConnection) -> Result<(), SimError> {
        // It is safe to drop expired sessions that have no user ID set because they cannot become valid.  For sessions
        // with a user ID, extra caution is required; do not directly delete those that are currently in the cache.
        self.collect_session_stubs(db).await?;

        let user_sessions = Sessions::find()
            .select_only()
            .column(session::Column::Id)
            .filter(session::Column::CustomerId.is_not_null())
            .into_tuple::<i64>()
            .all(db)
            .await?;

        for session_id in user_sessions {
            self.session_cache()?
                .entry(session_id)
                .await?
                .and_try_compute_with(async |entry| {
                    if let Some(entry) = entry {
                        let session = entry.into_value();
                        if (session.expires_on as u32) < self.current_day() {
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
                        // The scenario of a bug in the WBCache implementation is not considered here.
                        Ok(cache::Op::Nop)
                    }
                })
                .await?;
        }

        Ok(())
    }

    async fn curtain_call(&self, _db: &DatabaseConnection) -> Result<(), SimError> {
        self.customer_cache()?.close().await;
        self.product_cache()?.close().await;
        self.inv_rec_cache()?.close().await;
        self.order_cache()?.close().await;
        self.session_cache()?.close().await;
        Ok(())
    }
}

#[async_trait]
impl<APP> DBConnectionProvider for TestCompany<APP>
where
    APP: TestApp,
{
    async fn db_connection(&self) -> Result<Arc<DatabaseConnection>, SimErrorAny> {
        Ok(self.db())
    }
}
