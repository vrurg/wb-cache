use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use fieldx_plus::fx_plus;
use indicatif::ProgressBar;
use sea_orm::prelude::*;
use sea_orm::ActiveValue::Set;
use sea_orm::IntoActiveModel;

use super::actor::TestActor;
use super::db::cache::DBProvider;
use super::db::driver::DatabaseDriver;
use super::db::prelude::*;
use super::progress::MaybeProgress;
use super::progress::PStyle;
use super::scriptwriter::steps::ScriptTitle;
use super::types::simerr;
use super::types::OrderStatus;
use super::types::SimError;
use super::types::SimErrorAny;
use super::SimulationApp;

const WATCH_PRODUCT_ID: Option<i32> = None; // Some(1);

macro_rules! watch_product {
    ($record:expr, $( $msg:tt )+ ) => {
        if WATCH_PRODUCT_ID.is_some_and(|id| id < 0 || id == $record.product_id) {
            eprintln!( $( $msg )+ );
        }
    };
}

#[derive(Debug)]
#[fx_plus(
    agent(APP, unwrap(or_else(SimErrorAny, <APP as SimulationApp>::app_is_gone()))),
    parent,
    fallible(off, error(SimErrorAny)),
    default(off),
    sync
)]
pub struct TestCompany<APP: SimulationApp, D: DatabaseDriver> {
    #[fieldx(copy, get("_current_day"), inner_mut, set("_set_current_day", private), default(0))]
    current_day: i32,

    #[fieldx(lazy, get("_progress", private, clone), fallible)]
    #[allow(clippy::type_complexity)]
    progress: Arc<Option<ProgressBar>>,

    #[fieldx(get(clone), builder(required))]
    db: Arc<D>,

    #[fieldx(inner_mut, get(copy), set, default(0))]
    inv_check_no: u32,

    #[fieldx(inner_mut, get, get_mut, default(HashMap::new()))]
    updated_from: HashMap<Uuid, Order>,

    #[fieldx(inner_mut, set, get(copy), builder(off), default(Instant::now()))]
    started: Instant,
}

impl<APP: SimulationApp, D: DatabaseDriver> TestCompany<APP, D> {
    fn build_progress(&self) -> Result<Arc<Option<ProgressBar>>, SimErrorAny> {
        let app = self.app()?;
        let progress = app.acquire_progress(PStyle::Main, None)?;

        Ok(Arc::new(progress))
    }

    async fn update_inventory(&self, db: &DatabaseConnection, order: &Order) -> Result<(), SimError> {
        match order.status {
            OrderStatus::Backordered | OrderStatus::Refunded | OrderStatus::Shipped | OrderStatus::Recheck => (),
            _ => {
                watch_product!(
                    order,
                    "Updating inventory from order {}, status {:?}, product {}: {}",
                    order.id,
                    order.status,
                    order.product_id,
                    order.quantity
                );
                if self.updated_from().contains_key(&order.id) {
                    return Err(simerr!(
                        "Inventory already updated from order {:?}",
                        self.updated_from().get(&order.id)
                    )
                    .into());
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
impl<APP, D> TestActor for TestCompany<APP, D>
where
    APP: SimulationApp,
    D: DatabaseDriver,
{
    fn prelude(&self) -> Result<(), SimError> {
        self.set_started(Instant::now());
        self.progress()?.maybe_set_prefix("Plain ");
        Ok(())
    }

    fn progress(&self) -> Result<Arc<Option<ProgressBar>>, SimError> {
        Ok(self._progress()?)
    }

    async fn set_current_day(&self, day: i32) -> Result<(), SimError> {
        self._set_current_day(day);
        Ok(())
    }

    fn current_day(&self) -> i32 {
        self._current_day()
    }

    fn set_title(&self, _title: &ScriptTitle) -> Result<(), SimError> {
        Ok(())
    }

    async fn add_customer(&self, db: &DatabaseConnection, customer: &Customer) -> Result<(), SimError> {
        let mut customer = customer.clone();
        customer.registered_on = self.current_day();
        Customers::insert(customer.into_active_model()).exec(db).await?;
        Ok(())
    }

    async fn add_product(&self, db: &DatabaseConnection, product: &Product) -> Result<(), SimError> {
        Products::insert(product.clone().into_active_model()).exec(db).await?;
        Ok(())
    }

    async fn add_inventory_record(
        &self,
        db: &DatabaseConnection,
        inventory_record: &InventoryRecord,
    ) -> Result<(), SimError> {
        InventoryRecords::insert(inventory_record.clone().into_active_model())
            .exec(db)
            .await?;
        Ok(())
    }

    async fn add_order(&self, db: &DatabaseConnection, order: &Order) -> Result<(), SimError> {
        watch_product!(
            order,
            "Adding order {} on product {}: {}, {:?}",
            order.id,
            order.product_id,
            order.quantity,
            order.status
        );
        self.update_inventory(db, order).await?;

        let mut order = order.clone();
        order.purchased_on = self.current_day();
        order.into_active_model().insert(db).await?;

        Ok(())
    }

    async fn update_order(&self, db: &DatabaseConnection, order: &Order) -> Result<(), SimError> {
        watch_product!(
            order,
            "Updating order {} on product {}: {}, {:?}",
            order.id,
            order.product_id,
            order.quantity,
            order.status
        );
        self.update_inventory(db, order).await?;

        let mut order_update = super::db::entity::order::ActiveModel::new();
        order_update.id = Set(order.id);
        order_update.status = Set(order.status);
        order_update.update(db).await?;

        Ok(())
    }

    async fn update_inventory_record(
        &self,
        db: &DatabaseConnection,
        product_id: i32,
        quantity: i64,
    ) -> Result<(), SimError> {
        let Some(inventory_record) = InventoryRecords::find_by_id(product_id).one(db).await? else {
            return Err(simerr!(
                "Can't update non-existing inventory record for product ID: {}",
                product_id
            )
            .into());
        };

        let new_stock = inventory_record.stock + quantity;
        if new_stock < 0 {
            return Err(simerr!(
                "Not enough stock for product ID {}: need {}, but only {} remaining",
                product_id,
                -quantity,
                inventory_record.stock
            )
            .into());
        }
        watch_product!(
            inventory_record,
            "Updating inventory record for product ID {}: {} -> {}",
            product_id,
            inventory_record.stock,
            new_stock
        );
        let mut inventory_record = inventory_record.into_active_model();
        inventory_record.product_id = Set(product_id);
        inventory_record.stock = Set(new_stock);
        inventory_record.update(db).await?;

        Ok(())
    }

    async fn check_inventory(
        &self,
        db: &DatabaseConnection,
        product_id: i32,
        stock: i64,
        comment: &str,
    ) -> Result<(), SimError> {
        let inventory_record = InventoryRecords::find_by_id(product_id).one(db).await?;

        self.inv_rec_compare(&inventory_record, product_id, stock, comment)?;

        let inv_check_no = self.inv_check_no() + 1;
        self.set_inv_check_no(inv_check_no);

        Ok(())
    }

    async fn add_session(&self, db: &DatabaseConnection, session: &Session) -> Result<(), SimError> {
        let session = session.clone().into_active_model();
        Sessions::insert(session).exec(db).await?;
        Ok(())
    }

    async fn update_product_view_count(&self, db: &DatabaseConnection, product_id: i32) -> Result<(), SimError> {
        Products::update_many()
            .col_expr(
                super::db::entity::product::Column::Views,
                sea_orm::sea_query::SimpleExpr::Custom("views + 1".to_string()),
            )
            .filter(super::db::entity::product::Column::Id.eq(product_id))
            .exec(db)
            .await?;
        Ok(())
    }

    async fn update_session(&self, db: &DatabaseConnection, session: &Session) -> Result<(), SimError> {
        let mut session_update = super::db::entity::session::ActiveModel::new();
        session_update.id = Set(session.id);
        session_update.customer_id = Set(session.customer_id);
        session_update.expires_on = Set(session.expires_on);
        session_update.update(db).await?;

        Ok(())
    }

    async fn collect_sessions(&self, db: &DatabaseConnection) -> Result<(), SimError> {
        let res = Sessions::delete_many()
            .filter(super::db::entity::session::Column::ExpiresOn.lte(self.current_day()))
            .exec(db)
            .await?;

        if res.rows_affected == 0 {
            self.progress()?.maybe_set_message("");
        } else {
            self.progress()?
                .maybe_set_message(format!("Collected {} sessions", res.rows_affected));
        }

        Ok(())
    }

    async fn step_complete(&self, _db: &DatabaseConnection, step_num: usize) -> Result<(), SimError> {
        let elapsed = self.started().elapsed().as_secs_f64();
        self.app()?.set_plain_per_sec(step_num as f64 / elapsed);
        Ok(())
    }
}

#[async_trait]
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
