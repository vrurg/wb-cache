use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use fieldx_plus::Agent;
use indicatif::ProgressBar;
use sea_orm::prelude::*;
use tokio::time::Instant;
use tracing::debug;
use tracing::instrument;

use super::db::cache::DBProvider;
use super::db::driver::DatabaseDriver;
use super::db::entity::InventoryRecord;
use super::db::prelude::*;
use super::progress::traits::MaybeProgress;
use super::scriptwriter::steps::ScriptTitle;
use super::scriptwriter::steps::Step;
use super::types::simerr;
use super::types::SimError;
use super::types::SimErrorAny;
use super::SimulationApp;

#[async_trait]
pub trait TestActor<APP>: DBProvider + Agent<RcApp = Result<Arc<APP>, SimErrorAny>> + Debug
where
    APP: SimulationApp + Send + Sync + 'static,
{
    fn progress(&self) -> Result<Arc<Option<ProgressBar>>, SimError>;
    fn current_day(&self) -> i32;
    fn set_title(&self, title: &ScriptTitle) -> Result<(), SimError>;
    fn prelude(&self) -> Result<(), SimError>;

    async fn set_current_day(&self, day: i32) -> Result<(), SimError>;
    async fn add_customer(&self, db: &DatabaseConnection, customer: &Customer) -> Result<(), SimError>;
    async fn add_inventory_record(
        &self,
        db: &DatabaseConnection,
        inventory_record: &InventoryRecord,
    ) -> Result<(), SimError>;
    async fn add_order(&self, db: &DatabaseConnection, order: &Order) -> Result<(), SimError>;
    async fn add_product(&self, db: &DatabaseConnection, product: &Product) -> Result<(), SimError>;
    async fn add_session(&self, db: &DatabaseConnection, session: &Session) -> Result<(), SimError>;
    async fn check_inventory(
        &self,
        db: &DatabaseConnection,
        product_id: i32,
        stock: i64,
        comment: &str,
    ) -> Result<(), SimError>;
    async fn update_inventory_record(
        &self,
        db: &DatabaseConnection,
        product_id: i32,
        quantity: i64,
    ) -> Result<(), SimError>;
    async fn collect_sessions(&self, db: &DatabaseConnection) -> Result<(), SimError>;
    async fn update_order(&self, db: &DatabaseConnection, order: &Order) -> Result<(), SimError>;
    async fn update_product_view_count(&self, db: &DatabaseConnection, product_id: i32) -> Result<(), SimError>;
    async fn update_session(&self, db: &DatabaseConnection, session: &Session) -> Result<(), SimError>;
    async fn step_complete(&self, db: &DatabaseConnection, step_num: usize) -> Result<(), SimError>;

    fn inv_rec_compare(
        &self,
        inventory_record: &Option<InventoryRecord>,
        product_id: i32,
        stock: i64,
        comment: &str,
    ) -> Result<(), SimError> {
        if let Some(inventory_record) = inventory_record {
            if inventory_record.stock != stock {
                return Err(simerr!(
                    "Inventory check '{}' failed for product ID {}: expected {}, but found {}",
                    comment,
                    product_id,
                    stock,
                    inventory_record.stock
                )
                .into());
            }
        }
        else {
            return Err(simerr!("Inventory record not found for product ID {}", product_id).into());
        }

        Ok(())
    }

    #[instrument(level = "trace", skip(screenplay))]
    async fn act(&self, screenplay: &[Step]) -> Result<(), SimError> {
        self.prelude()?;

        let db = self.db_connection()?;
        let progress = self.progress()?;

        progress.maybe_set_length(screenplay.len() as u64);

        let mut checkpoint = Instant::now();
        let mut err = Ok(());
        let mut post_steps = 0;

        if !matches!(screenplay.first(), Some(Step::Title { .. })) {
            Err(simerr!("Screenplay must start with a title"))?;
        }

        for (step_num, step) in screenplay.iter().enumerate() {
            if err.is_err() {
                if post_steps < 10 {
                    post_steps += 1;
                    continue;
                }
                return err;
            }
            if Instant::now().duration_since(checkpoint).as_secs() > 5 {
                checkpoint = Instant::now();
                self.db_driver()?.checkpoint().await?;
            }

            let step_name = format!("{step}");

            debug!("Executing step {step_name}: {step:?}");

            err = match step {
                Step::Title(title) => self.set_title(title),
                Step::Day(day) => {
                    self.set_current_day(*day).await?;
                    Ok(())
                }
                Step::AddProduct(product) => self.add_product(&db, product).await,
                Step::AddInventoryRecord(inventory_record) => self.add_inventory_record(&db, inventory_record).await,
                Step::AddCustomer(customer) => self.add_customer(&db, customer).await,
                Step::AddOrder(order) => self.add_order(&db, order).await,
                Step::AddStock(shipment) => {
                    self.update_inventory_record(&db, shipment.product_id, shipment.batch_size as i64)
                        .await
                }
                Step::UpdateOrder(order) => self.update_order(&db, order).await,
                Step::CheckInventory(checkpoint) => {
                    self.check_inventory(&db, checkpoint.product_id, checkpoint.stock, &checkpoint.comment)
                        .await
                }
                Step::AddSession(session) => self.add_session(&db, session).await,
                Step::UpdateSession(session) => self.update_session(&db, session).await,
                Step::ViewProduct(product_id) => self.update_product_view_count(&db, *product_id).await,
                Step::CollectSessions => self.collect_sessions(&db).await,
            }
            .inspect_err(|err| {
                err.context(format!("step {step_name}"));
            });

            progress.maybe_inc(1);
            self.step_complete(&db, step_num).await?;
        }

        self.curtain_call().await?;

        Ok(())
    }

    #[instrument(level = "trace", skip(self))]
    async fn curtain_call(&self) -> Result<(), SimError> {
        Ok(())
    }

    /// Collect sessions that are older than the current day and have no customer ID
    #[instrument(level = "trace", skip(self, db))]
    async fn collect_session_stubs(&self, db: &DatabaseConnection) -> Result<(), SimError> {
        let res = Sessions::delete_many()
            .filter(
                super::db::entity::session::Column::ExpiresOn
                    .lte(self.current_day())
                    .and(super::db::entity::session::Column::CustomerId.is_null()),
            )
            .exec(db)
            .await?;

        if res.rows_affected == 0 {
            self.progress()?.maybe_set_message("");
        }
        else {
            self.progress()?
                .maybe_set_message(format!("Collected {} sessions", res.rows_affected));
        }

        Ok(())
    }
}
