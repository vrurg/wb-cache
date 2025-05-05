pub mod entity;
pub mod math;
pub mod model;
pub mod reporter;
pub mod rnd_pool;
pub mod steps;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt::Display;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use std::thread;

use console::style;
use crossbeam::channel::Receiver;
use crossbeam::channel::Sender;
use entity::customer::Customer;
use entity::inventory::IncomingShipment;
use entity::inventory::InventoryCheck;
use entity::inventory::InventoryRecord;
use entity::order::Order;
use entity::product::Product;
use entity::shipment::Shipment;
use fieldx_plus::child_build;
use fieldx_plus::fx_plus;
use model::customer::CustomerModel;
use model::product::ProductModel;
use rand::Rng;
use rand_distr::Bernoulli;
use rand_distr::Distribution;
use rand_distr::Geometric;
use rand_distr::Normal;
use rand_distr::Poisson;
use reporter::Reporter;
use reporter::TaskStatus;
use rnd_pool::RndPool;
use sea_orm::prelude::Uuid;
use steps::ScriptTitle;
use steps::Step;

use super::db::entity::Order as DbOrder;
use super::db::entity::Session as DbSession;
use super::types::simerr;
use super::types::OrderStatus;
use super::types::Result;
use super::types::SimErrorAny;

thread_local! {
    static BATCH_STEPS: RefCell<Vec<Step>> = const { RefCell::new(Vec::new()) };
}

const LOGIN_EXPIRE_DAYS: i32 = 7;
const SESSION_FACTOR: usize = 10;

enum AddPostProc {
    // Backorders for the product ID.
    BackorderNew(u32),
    BackorderAgain(u32),
    /// Pending until the specified day.
    Pending(u32),
    Shipping(DbOrder),
    None,
}

#[derive(Clone, Debug)]
enum TaskData {
    /// Generate purchase orders for customers.
    Purchase(PurchaseTask),
    /// Generate order returns.
    Returns,
    /// Fulfill pending orders. Takes list of order indices in the steps array.
    Pending(Vec<usize>),
    /// Generate temporary sessions for non-login visitors.
    Sessions,
}

#[derive(Clone, Debug)]
struct PurchaseTask {
    customers:         Vec<Arc<Customer>>,
    product_interests: Arc<Vec<(u32, f64)>>,
}

#[derive(Clone, Debug)]
struct Task {
    data: TaskData,
    tx:   Sender<Result<()>>,
}

impl Task {
    fn done(&self, result: Result<()>) -> Result<()> {
        Ok(self.tx.send(result)?)
    }
}

struct TaskResult {
    rx: Receiver<Result<()>>,
}

impl TaskResult {
    fn wait(&self) -> Result<()> {
        self.rx.recv()?
    }
}

/// Generate a scenario to be executed by the Company simulator.
#[fx_plus(parent, sync, rc, builder(error(SimErrorAny), post_build, opt_in))]
pub struct ScriptWriter {
    /// For how many days the scenario will run.
    #[fieldx(default(365), builder)]
    period: u32,

    #[fieldx(lock, get(copy), set, default(0))]
    current_day: u32,

    /// How many customers we have the day 1
    #[fieldx(get(copy), default(1), builder)]
    initial_customers: u32,

    /// The maximum number of customer the company can have by the end of the simulation period.
    #[fieldx(get(copy), default(10_000), builder)]
    market_capacity: u32,

    /// Where customer base growth reaches its peak.
    #[fieldx(get(copy), default(3_000), builder)]
    inflection_point: u32,

    /// Company's "success" rate – how fast the customer base grows
    #[fieldx(get(copy), default(0.05), builder)]
    growth_rate: f64,

    #[fieldx(get(copy), default(0.15), builder)]
    min_customer_orders: f64,
    // This value is not a hard upper limit but the boundary within which 90% of randomly generated customer expected
    // daily orders will fall. The remaining 10% might be higher than this value.
    #[fieldx(get(copy), default(3.0), builder)]
    max_customer_orders: f64,

    #[fieldx(get(copy), default(10), builder)]
    product_count: u32,

    #[fieldx(get(copy), default(30), builder)]
    return_window: u32,

    /// Index in the vector is the products ID.
    #[fieldx(lock, get, get_mut)]
    products: Vec<Product>,

    /// Product return probability per day, by ID.
    #[fieldx(lazy, get)]
    return_probs: BTreeMap<u32, f64>,

    #[fieldx(lock, get, get_mut, set, default(Vec::with_capacity(100_000_000)))]
    steps: Vec<Step>,

    /// "Planned" returns, per day
    #[fieldx(inner_mut, get_mut)]
    returns: HashMap<u32, Vec<Order>>,

    /// Backorders by their indices in the steps array, per product ID. When inventory is replenished, these orders will
    /// be processed first.
    ///
    /// Since product ID is its vector index, we use a Vec here too.
    #[fieldx(lock, get, get_mut)]
    backorders: Vec<VecDeque<usize>>,

    #[fieldx(lazy, clearer, get(copy), default(0))]
    total_backordered: usize,

    /// Pending orders by their indices in the steps array, keyed by shipping day.
    /// This is a map from the day to a queue of orders.
    #[fieldx(lock, get, get_mut)]
    pending_orders: HashMap<u32, Vec<usize>>,

    /// Inventory record per product ID. Since product ID is its vector index in self.products, we use a Vec here too.
    #[fieldx(lock, get, get_mut)]
    inventory: Vec<InventoryRecord>,

    /// Map shipment day into shipments.
    #[fieldx(lock, get, get_mut)]
    shipments: BTreeMap<u32, Vec<Shipment>>,

    /// Map product ID into the number of items currently shipping.
    #[fieldx(lock, get, get_mut)]
    product_shipping: BTreeMap<u32, u32>,

    /// Map unique email into customer.
    #[fieldx(lock, get, get_mut, default(HashMap::new()))]
    customers: HashMap<String, Arc<Customer>>,

    /// Customer counts over the last few days.
    #[fieldx(lock, get, get_mut)]
    customer_counts: VecDeque<u32>,

    /// Customer history length
    #[fieldx(get(copy), default(7))]
    customer_history_length: usize,

    /// Keep track of the number of customers that are expected to be registered the next day.
    #[fieldx(inner_mut, get(copy), set)]
    next_day_customers: usize,

    #[fieldx(lazy, get(clone))]
    random_pool: Arc<RndPool>,

    #[fieldx(lazy, get)]
    customer_model: CustomerModel,

    #[fieldx(lazy, get)]
    product_price_model: Arc<ProductModel>,

    // --- Workers pool related
    #[fieldx(lazy, clearer, private, builder(off), get)]
    task_tx:       Sender<Task>,
    #[fieldx(lazy, private, builder(off), get(copy))]
    worker_count:  usize,
    #[fieldx(lock, private, get, get_mut, builder(off))]
    task_handlers: Vec<std::thread::JoinHandle<usize>>,

    #[fieldx(lazy, private, get, builder(off))]
    reporter: Arc<Reporter>,

    #[fieldx(lock, clearer, get(copy), set, builder(off))]
    track_product: usize,

    next_session_id: AtomicI64,

    // Base number to form session IDs. It has the format of <current_day> * 10^<number_of_digits_in_market_capacity>.
    #[fieldx(lazy, private, clearer, get(copy), builder(off))]
    session_base: i64,

    #[fieldx(lazy, inner_mut, get, get_mut, builder(off))]
    customer_sessions: Vec<Option<DbSession>>,
}

impl ScriptWriter {
    fn post_build(self: Arc<ScriptWriter>) -> Result<Arc<ScriptWriter>> {
        self.set_next_day_customers(self.initial_customers() as usize);

        // Do not let the model saturate too quickly. Limit the expected growth so that it reaches 75% of the market
        // capacity at 4/5 of the total modeling period. This is a shortcoming of the Richards model:
        // it is not as asymptotic as it should be. Ideally, it would never actually reach full market capacity.
        self.customer_model()
            .adjust_growth_rate(self.customer_model().market_capacity() * 0.75, self.period * 4 / 5);

        Ok(self)
    }

    pub fn create(&self) -> Result<Vec<Step>> {
        self.direct()?;
        self.reporter().stop()?;
        Ok(self.set_steps(Vec::new()))
    }

    pub fn direct(&self) -> Result<()> {
        self.reporter().start()?;

        self.add_step(Step::Title(ScriptTitle {
            period:          self.period,
            products:        self.product_count(),
            market_capacity: self.market_capacity,
        }))?;

        self.init_products()?;
        self.register_customers()?;
        self.order_shipments(true)?;

        for day in 1..=self.period {
            self.set_current_day(day);
            self.clear_session_base();
            self.add_step(Step::Day(day))?;
            let sessions_task = self.submit_task(TaskData::Sessions)?;
            self.replenish_inventory()?;
            self.fulfill_pending()?;
            self.register_customers()?;
            self.process_returns()?;
            self.place_orders()?;

            // The end of the day processing.
            self.order_shipments(false)?;
            sessions_task.wait()?;
            self.add_step(Step::CollectSessions)?;
            self.log_progress()?;
        }

        self.reporter().out("--- Simulation finished ---")?;
        self.clear_task_tx();
        while !self.task_handlers().is_empty() {
            self.task_handlers_mut().pop().unwrap().join().unwrap();
        }
        self.reporter().out("--- All threads finished ---")?;

        self.reporter().stop()?;

        Ok(())
    }

    fn maybe_init_batch_steps(&self, batch_steps: &mut Vec<Step>) {
        if batch_steps.capacity() == 0 {
            batch_steps.reserve(
                self.customer_model().market_capacity() as usize * self.max_customer_orders() as usize * 2
                    / self.worker_count(),
            );
        }
    }

    fn submit_task(&self, data: TaskData) -> Result<TaskResult> {
        let (tx, rx) = crossbeam::channel::unbounded();
        let task = Task { data, tx };
        let task_result = TaskResult { rx };
        self.task_tx().send(task)?;
        Ok(task_result)
    }

    fn task_purchases(&self, task: &PurchaseTask) -> Result<()> {
        let mut rng = rand::rng();
        let thread = thread::current();
        let thread_id = thread.name().unwrap_or("unknown");
        let return_probs = self.return_probs().clone();

        BATCH_STEPS.with_borrow_mut(|batch_steps| -> Result<()> {
            self.maybe_init_batch_steps(batch_steps);

            let init_capacity = batch_steps.capacity();
            let mut crecords = Vec::with_capacity(task.customers.len());
            let mut expected_steps = 0;
            let mut planned_returns: BTreeMap<u32, Vec<Order>> = BTreeMap::new();
            let return_window = self.return_window() as f32;
            let current_day = self.current_day();

            for customer in task.customers.iter() {
                let customer_purchases = customer.daily_purchases()?;
                expected_steps += customer_purchases as usize;
                crecords.push((customer, customer_purchases));

                // If the customer is likely to purchase today, then we need to refresh their session.
                // Otherwise, we simulate a scenario where the customer visits to check if they are interested.
                // This is based on their expected daily purchases, so customers with an expectation of 1 or more
                // are more likely to visit every day.
                if customer_purchases > 0.0 || Bernoulli::new(1.0 - (-customer_purchases).exp()).unwrap().sample(&mut rng) {
                    self.customer_login(customer.id())?;
                }
            }

            let mut total_purchases = 0;
            // let mut batch_steps = Vec::with_capacity(init_capacity);

            for (customer, customer_purchases) in crecords {
                let customer_id = customer.id();

                // self.reporter()
                //     .out(format!("Customer {}: {} purchases", customer.id(), customer_purchases))?;

                if customer_purchases == 0.0 {
                    // eprint!("  No purchases for customer {}", customer.id());
                    continue;
                }

                let mut purchases = 0;

                for (product_id, product_interest) in task.product_interests.iter() {

                    // The customer may want to see the product page before ordering it.
                    if rng.random_range(0.0..1.0) <= self.products()[*product_id as usize].view_probability() {
                        batch_steps.push(Step::ViewProduct(*product_id));
                    }

                    let product_items = Poisson::new(customer_purchases * product_interest)
                        .unwrap()
                        .sample(&mut rng)
                        .round() as u32;

                    if product_items == 0 {
                        // eprint!("  No purchases for customer {}", customer.id());
                        continue;
                    }

                    let order = Order {
                        id:          Uuid::new_v4(),
                        customer_id,
                        product_id:  *product_id,
                        quantity:    product_items,
                        status:      OrderStatus::New,
                    };

                    if rng.random_range(0.0..1.0) < *return_probs.get(product_id).unwrap() {
                        let on_day =
                            rng.random_range(0.0..return_window).round() as u32 + 1 + current_day;
                        if on_day < self.period {
                            planned_returns
                                .entry(on_day)
                                .or_default()
                                .push(order.clone());
                        }
                    }

                    self.adjust_capacity(batch_steps, 100);
                    batch_steps.push(Step::AddOrder(order.into()));

                    purchases += product_items;
                }

                total_purchases += purchases;
            }

            if batch_steps.capacity() > init_capacity {
                self.reporter().out(format!(
                    "Thread {}: Purchase task steps capacity increased from {} to {} (expected steps: {}, total steps: {}, batch steps: {})",
                    thread_id,
                    init_capacity,
                    batch_steps.capacity(),
                    expected_steps,
                    total_purchases,
                    batch_steps.len()
                ))?;
            }

            self.add_steps(batch_steps.drain(0..batch_steps.len()))?;

            let mut returns = self.returns_mut();
            for (day, orders) in planned_returns {
                returns.entry(day).or_default().extend(orders);
            }

            Ok(())
        })
    }

    fn task_returns(&self) -> Result<()> {
        BATCH_STEPS.with_borrow_mut(|batch_steps| -> Result<()> {
            self.maybe_init_batch_steps(batch_steps);
            let mut returns = self.returns_mut();

            // Process prepared returns first.
            if let Some(today_returns) = returns.remove(&self.current_day()) {
                // self.reporter().out(format!(
                //     "Processing {} returns for day {}",
                //     today_returns.len(),
                //     self.current_day()
                // ))?;

                for mut order in today_returns {
                    order.status = OrderStatus::Refunded;
                    batch_steps.push(Step::UpdateOrder(order.into()));
                }
            }

            self.add_steps(batch_steps.drain(0..batch_steps.len()))?;

            Ok(())
        })
    }

    fn task_pending(&self, indicies: &[usize]) -> Result<()> {
        BATCH_STEPS.with_borrow_mut(|batch_steps| -> Result<()> {
            self.maybe_init_batch_steps(batch_steps);

            for step_idx in indicies.iter().copied() {
                let mut order = self.order_by_idx(step_idx)?;

                order.status = OrderStatus::Shipped;
                batch_steps.push(Step::UpdateOrder(order));
            }

            self.add_steps(batch_steps.drain(0..batch_steps.len()))?;

            Ok(())
        })
    }

    fn next_session_id(&self) -> i64 {
        self.next_session_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + self.session_base()
    }

    fn task_sessions(&self) -> Result<()> {
        // Non-login sessions are those where a user visited the site but did not register or log in.  In such cases, a
        // new session is always created; however, it is short-lived with an expiration set to the next day.  The
        // expected number of temporary sessions depends on site popularity, which is measured by the number of existing
        // customers.  Anticipated traffic is customer_count * 10, roughly equivalent to a 10% conversion rate from
        // advertising—an unrealistically good value.
        // Sessions are added one by one to intersperse them with other steps.

        let day = self.current_day() as i32;
        let anticipated_sessions = (self.customers().len() * SESSION_FACTOR) as f64;
        let sigma = anticipated_sessions * 0.2;

        let todays_sessions = Normal::new(anticipated_sessions, sigma)
            .map_err(|err| simerr!("Normal distribution: {}", err))?
            .sample(&mut rand::rng())
            .round() as u32;

        for _ in 0..todays_sessions {
            self.add_step(Step::AddSession(DbSession {
                id:          self.next_session_id(),
                customer_id: None,
                expires_on:  day + 3,
            }))?;
        }

        Ok(())
    }

    fn task_worker(&self, thread_id: usize, rx: Receiver<Task>) -> Result<()> {
        let thread_name = thread::current()
            .name()
            .ok_or(simerr!("Worker thread must have a name"))?
            .to_owned();

        loop {
            let task = {
                let Ok(task) = rx.recv()
                else {
                    break;
                };
                task
            };

            self.reporter().set_task_status(thread_id, TaskStatus::Busy);

            task.done(
                match task.data {
                    TaskData::Purchase(ref p_task) => self.task_purchases(p_task),
                    TaskData::Returns => self.task_returns(),
                    TaskData::Pending(ref p_task) => self.task_pending(p_task),
                    TaskData::Sessions => self.task_sessions(),
                }
                .inspect_err(|err| {
                    err.context(format!("task worker {thread_name}"));
                }),
            )?;

            self.reporter().set_task_status(thread_id, TaskStatus::Idle);
        }

        Ok(())
    }

    fn log_progress(&self) -> Result<()> {
        let reporter = self.reporter();
        reporter.set_scenario_lines(self.steps().len());
        let total_backordered = self.total_backordered();
        reporter.set_backorders(total_backordered);
        let total_pending = self.pending_orders().values().map(|queue| queue.len()).sum();
        reporter.set_pending_orders(total_pending);
        if total_backordered > 100_000 {
            // Categorize by product ID and sort by the amount.
            let backorders = self.backorders();
            let mut bo = backorders.iter().enumerate().collect::<Vec<_>>();
            bo.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
            let product_id = bo[0].0;
            self.set_track_product(product_id);
            let product = &self.products()[product_id];
            reporter.out(format!(
                "Top backordered product {}: {} orders:\n  Daily quotient: {:.2}, daily estimate: {:.2}, expected return rate: {:.2}\n  Stock supplies in: {:.2}, supplier inaccuracy: {:.2}, supplier tardiness: {:.2}",
                product.id(),
                bo[0].1.len(),
                product.daily_quotient(),
                product.daily_estimate(),
                product.expected_return_rate(),
                product.stock_supplies_in(),
                product.supplier_inaccuracy(),
                product.supplier_tardiness(),
            ))?;
        }
        else {
            self.clear_track_product();
        }

        let mut check_steps = vec![];
        for inv_rec in self.inventory().iter() {
            check_steps.push(Step::CheckInventory(InventoryCheck::new(
                inv_rec.product_id(),
                inv_rec.stock(),
                "daily",
            )));
        }
        self.add_steps(check_steps)?;

        // if total_pending > 50_000 {
        //     reporter.out(format!(
        //         "Pending orders: {}",
        //         self.pending_orders()
        //             .iter()
        //             .map(|(day, queue)| format!("{}: {}", day, queue.len()))
        //             .collect::<Vec<_>>()
        //             .join(", ")
        //     ))?;
        // }
        self.reporter().refresh_report()
    }

    #[inline(always)]
    fn adjust_capacity<T>(&self, steps: &mut Vec<T>, delta: usize) {
        let step_count = steps.len();
        if (step_count + delta) >= steps.capacity() {
            // At least double the capacity.
            steps.reserve(step_count);
            // eprintln!("Step capacity increased from {} to {}", step_count, steps.capacity());
        }
    }

    fn process_order(&self, mut step: Step) -> Result<(Step, AddPostProc)> {
        let order = match step {
            Step::AddOrder(ref mut order) | Step::UpdateOrder(ref mut order) => order,
            _ => return Err(simerr!("Non-order step submitted for order processing: {:?}", step)),
        };

        let mut post_proc = AddPostProc::None;

        if order.status == OrderStatus::New || order.status == OrderStatus::Recheck {
            let new_order = order.status == OrderStatus::New;
            let mut inventory = self.inventory_mut();
            let Some(inv_rec) = inventory.get_mut(order.product_id as usize)
            else {
                return Err(simerr!("Product {} not found in inventory", order.product_id));
            };

            if inv_rec.stock() < order.quantity {
                // Not enough stock, mark the order as backordered.
                order.status = OrderStatus::Backordered;
                if new_order {
                    post_proc = AddPostProc::BackorderNew(order.product_id);
                }
                else {
                    post_proc = AddPostProc::BackorderAgain(order.product_id);
                }
            }
            else {
                order.status = OrderStatus::Pending;
                if inv_rec.handling_days() > 0 {
                    // If this inventory entry requires extra processing then the order goes into the pending queue.
                    post_proc = AddPostProc::Pending(self.current_day() + inv_rec.handling_days() as u32);
                    // self.pending_orders_mut()
                    //     .entry(self.current_day() + inv_rec.handling_days() as i32)
                    //     .or_default()
                    //     .push_back(order.clone());
                }
                else {
                    // Same-day shipping: a pending step is still needed because the warehouse must process the request
                    // before dispatch, so a post-processing step is added to proceed with the shipment.
                    let mut shipping_order = order.clone();
                    shipping_order.status = OrderStatus::Shipped;
                    post_proc = AddPostProc::Shipping(shipping_order);
                }
                // Either way, the stock gets reduced.
                *inv_rec.stock_mut() -= order.quantity;
            }
        }

        Ok((step, post_proc))
    }

    #[inline]
    fn add_step(&self, step: Step) -> Result<()> {
        self.add_steps([step])
    }

    fn add_steps<S: IntoIterator<Item = Step>>(&self, new_steps: S) -> Result<()> {
        let mut steps = self.steps_mut();
        self.adjust_capacity(&mut *steps, 10_000);

        // List of backordered orders as a tuple of product ID and the position in the steps vector.
        let mut backordered_new: Vec<(u32, usize)> = Vec::new();
        let mut backordered_again: Vec<(u32, usize)> = Vec::new();
        // List of pending orders as a tuple of a day to ship and the position in the steps vector.
        let mut pending_orders: Vec<(u32, usize)> = Vec::new();

        for mut step in new_steps.into_iter() {
            let mut post_proc = AddPostProc::None;
            match step {
                Step::AddOrder(_) | Step::UpdateOrder(_) => {
                    (step, post_proc) = self.process_order(step)?;
                }
                _ => (),
            }

            steps.push(step);
            let step_idx = steps.len() - 1;

            match post_proc {
                AddPostProc::Pending(day) => {
                    pending_orders.push((day, step_idx));
                }
                AddPostProc::BackorderNew(product_id) => {
                    backordered_new.push((product_id, step_idx));
                }
                AddPostProc::BackorderAgain(product_id) => {
                    backordered_again.push((product_id, step_idx));
                }
                AddPostProc::Shipping(shipping_order) => {
                    // let inv_rec = inventory.get_mut(shipping_order.product_id as usize).unwrap();
                    // steps.push(Step::CheckInventory(InventoryCheck::new(
                    //     shipping_order.product_id,
                    //     inv_rec.stock(),
                    //     "same day shipping",
                    // )));
                    steps.push(Step::UpdateOrder(shipping_order));
                }
                AddPostProc::None => (),
            }
        }

        if !pending_orders.is_empty() {
            let mut pendings = self.pending_orders_mut();
            for (day, step_idx) in pending_orders {
                pendings.entry(day).or_default().push(step_idx);
            }
        }

        if !(backordered_new.is_empty() && backordered_again.is_empty()) {
            let mut backorders = self.backorders_mut();
            for (product_id, step_idx) in backordered_again {
                backorders[product_id as usize].push_front(step_idx);
            }
            for (product_id, step_idx) in backordered_new {
                backorders[product_id as usize].push_back(step_idx);
            }
        }

        self.reporter().set_scenario_lines(steps.len());
        self.reporter().set_scenario_capacity(steps.capacity());

        Ok(())
    }

    /// Register our products and initialize related structures: backorders and ivnentory.
    fn init_products(&self) -> Result<()> {
        let mut products = self.products_mut();
        let random_pool = self.random_pool();
        let price_model = self.product_price_model();
        let product_count = self.product_count() as usize;
        let geometric = Geometric::new(0.7).unwrap();
        let mut rng = rand::rng();

        products.reserve(product_count);
        self.backorders_mut().resize(product_count, VecDeque::new());

        for id in 0..self.product_count() {
            let price = price_model.next_price();
            let daily_quotient = random_pool.next_rand();
            // Limit the return rate to 30% to avoid too much volatility
            let expected_return_rate = random_pool.next_rand() * 0.3;
            // Let's be optimistic and assume that expected shipment terms are between 3 and 21 days. 3 is OK, but 21 in
            // the real world sometimes is an optimistic expectation. Though, with a 50% chance, it arrives later,
            // making things more realistic.
            let ship_terms = 3.0 + random_pool.next_rand() * 18.0;
            // Make it random in the range of 5% to 20%.
            let supplier_inaccuracy = random_pool.next_rand() * 0.15 + 0.05;
            // Make it random in the range of -0.9 to 0.7.
            let supplier_tardiness = random_pool.next_rand() * 1.6 - 0.9;
            let product = Product::builder()
                .id(id)
                .name(format!("Product {id}"))
                .price(price)
                .daily_quotient(daily_quotient)
                .expected_return_rate(expected_return_rate)
                .stock_supplies_in(ship_terms)
                .supplier_inaccuracy(supplier_inaccuracy)
                .supplier_tardiness(supplier_tardiness)
                .product_model(self.product_price_model().clone())
                .build()
                .unwrap();
            let step = Step::AddProduct(product.clone().into());
            self.add_step(step)?;

            products.push(product);

            // The distribution gives 1.. values, so we need to subtract 1 to get the range of 0...
            let handling_days = geometric.sample(&mut rng).min(255) as u8;
            let inv_rec = InventoryRecord::new(id, 0, handling_days);
            self.inventory_mut().push(inv_rec.clone());
            self.add_step(Step::AddInventoryRecord(inv_rec.into()))?;
        }

        Ok(())
    }

    fn register_customers(&self) -> Result<()> {
        let new_customers = self.next_day_customers();

        for _ in 0..new_customers {
            let customer = self.random_pool().next_customer();
            self.add_step(Step::AddCustomer(customer.clone().into()))?;
            if self.customers().contains_key(customer.email()) {
                panic!("Customer with email {} already exists", customer.email());
            }
            self.customers_mut()
                .insert(customer.email().clone(), Arc::new(customer));
        }

        if self.current_day() < self.period {
            let next_cust = self.customer_model().expected_customers(self.current_day() + 1).round() as usize
                - self.customers().len();
            self.set_next_day_customers(next_cust);

            // Shift the customer counts history window.
            let mut customer_counts = self.customer_counts_mut();
            while customer_counts.len() >= self.customer_history_length() {
                customer_counts.pop_front();
            }
            customer_counts.push_back(self.customers().len() as u32);
        }

        Ok(())
    }

    fn customer_login(&self, customer_id: u32) -> Result<()> {
        let sess_idx = customer_id as usize - 1;
        if let Some(existing_session) = &mut self.customer_sessions_mut()[sess_idx] {
            if existing_session.expires_on >= self.current_day() as i32 {
                // Session is still valid, no need to create a new one but extend the expiration date.
                existing_session.expires_on = self.current_day() as i32 + LOGIN_EXPIRE_DAYS;
                self.add_step(Step::UpdateSession(existing_session.clone()))?;
                return Ok(());
            }
        }

        // A new session.
        let session = DbSession {
            id:          self.next_session_id(),
            customer_id: Some(customer_id),
            expires_on:  self.current_day() as i32 + LOGIN_EXPIRE_DAYS,
        };
        self.customer_sessions_mut()[customer_id as usize - 1] = Some(session.clone());
        self.add_step(Step::AddSession(session))?;
        Ok(())
    }

    fn order_shipments(&self, initial: bool) -> Result<()> {
        self.clear_total_backordered();

        let customer_count = self.customers().len() as f64;
        let products = self.products();
        let inventory = self.inventory();
        let mut shipments = self.shipments_mut();
        let anticipated_growth = {
            let base = self
                .customer_counts()
                .front()
                .map(|rec| *rec as f64)
                .unwrap_or(self.customer_model().initial_customers());
            self.customers().len() as f64 / base
        };

        'inv_record: for product in products.iter() {
            let product_id = product.id();

            // A more realistic model would take into account the supplier's tardiness and inaccuracies, but we want to
            // test stock outages as well. Still, we attempt to account for the expected customer base growth, albeit
            // very simplistically, by multiplying the current number of customers by a constant factor.
            let estimate_sales = self.total_backordered() as u32
                + (product.daily_estimate() * customer_count * anticipated_growth * product.supplies_in()).round()
                    as u32;
            let being_shipped = *self.product_shipping_mut().entry(product_id).or_insert(0);

            if let Some(tracked_product) = self.track_product() {
                if tracked_product == product_id as usize {
                    self.reporter().out(format!(
                        "Product {}: being shipped: {}, estimate sales: {}, inventory: {}",
                        product_id,
                        being_shipped,
                        estimate_sales,
                        inventory.get(tracked_product).map_or(0, |rec| rec.stock())
                    ))?;
                }
            }

            if being_shipped > estimate_sales || estimate_sales == 0 {
                continue 'inv_record;
            }

            let mut batch_size = estimate_sales - being_shipped;

            let arrives_on = if initial {
                1
            }
            else {
                if let Some(inv_rec) = inventory.get(product_id as usize) {
                    // If we still have enough stock, we don't need to order anything.
                    if inv_rec.stock() > batch_size {
                        continue 'inv_record;
                    }
                    batch_size -= inv_rec.stock();
                }
                // We ceil-round the value because even the small
                // fraction of a day counts towards the entire day.
                self.current_day() + product.supplies_in().ceil() as u32
            };

            if batch_size > 0 {
                let shipment = Shipment::builder()
                    .product_id(product_id)
                    .batch_size(batch_size)
                    .arrives_on(arrives_on)
                    .build()
                    .unwrap();

                if let Some(tracked_product) = self.track_product() {
                    if tracked_product == product_id as usize {
                        self.reporter().out(format!(
                            "Order shipment for product {}: {} units, arrives in {} days",
                            product_id,
                            shipment.batch_size(),
                            shipment.arrives_on() - self.current_day()
                        ))?;
                    }
                }

                *self.product_shipping_mut().get_mut(&product_id).unwrap() += shipment.batch_size();
                shipments.entry(arrives_on).or_default().push(shipment);
            }
        }

        Ok(())
    }

    // Take all shipments for the current day, replenish the inventory and remove the shipments from the list.
    fn replenish_inventory(&self) -> Result<()> {
        let mut all_shipments = self.shipments_mut();
        let products = self.products();
        let mut inv_steps = Vec::new();

        // self.report(
        //     "I ".to_owned()
        //         + &self
        //             .inventory()
        //             .iter()
        //             .enumerate()
        //             .map(|(id, rec)| format!("{}: {:>8}", id, rec.stock()))
        //             .collect::<Vec<_>>()
        //             .join("|"),
        // )?;

        // self.reporter().out(
        //     "B ".to_owned()
        //         + &self
        //             .backorders()
        //             .iter()
        //             .enumerate()
        //             .map(|(id, orders)| format!("{}: {:>8}", id, orders.len()))
        //             .collect::<Vec<_>>()
        //             .join("|"),
        // )?;

        if let Some(shipments) = all_shipments.remove(&self.current_day()) {
            let mut inventory = self.inventory_mut();
            let mut affected_products = vec![false; self.product_count() as usize];
            for shipment in shipments {
                let product = products.get(shipment.product_id() as usize).unwrap();
                let product_id = product.id() as usize;
                let batch_size = shipment.batch_size();
                let inv_rec = inventory.get_mut(product_id).unwrap();

                affected_products[product_id] = true;

                // inv_steps.push(Step::CheckInventory(InventoryCheck::new(
                //     shipment.product_id(),
                //     inv_rec.stock(),
                //     "pre-incoming shipment",
                // )));

                let new_stock = inv_rec.stock() + batch_size;
                *inv_rec.stock_mut() = new_stock;

                if let Some(tracked_product) = self.track_product() {
                    if tracked_product == product_id {
                        self.reporter().out(format!(
                            "Arrived shipment for product {product_id}: {batch_size} units, new stock: {new_stock}",
                        ))?;
                    }
                }

                // The executor doesn't need to see the ordered shipment. We only inform it to replenish the inventory.
                // It wouldn't also be involved with processing backorders – only execute order changes where it will
                // actually decrease inventory stock.
                inv_steps.push(Step::AddStock(IncomingShipment {
                    product_id: shipment.product_id(),
                    batch_size: shipment.batch_size(),
                }));

                // inv_steps.push(Step::CheckInventory(InventoryCheck::new(
                //     shipment.product_id(),
                //     new_stock,
                //     "post-incoming shipment",
                // )));

                // self.report(format!(
                //     "[day {}] Arrived shipment for product {}: {} units",
                //     self.current_day(),
                //     product_id,
                //     batch_size
                // ))?;

                *self.product_shipping_mut().get_mut(&product.id()).unwrap() -= shipment.batch_size();

                // // Put what's left of the arrived batch after fulfilling backorders into the inventory.
                // // Tell the executor to match the inventory with the expected value. This is useful for testing purposes
                // // to ensure that scenario is in sync with the simulation.
                // inv_steps.push(self.new_step(StepAction::CheckInventory {
                //     product_id: product_id as u32,
                //     stock:      new_stock,
                // }));
            }

            for (product_id, updated) in affected_products.iter().enumerate() {
                if *updated {
                    let mut backorders = self.backorders_mut();

                    if let Some(orders) = backorders.get_mut(product_id) {
                        // if orders.len() > 0 {
                        //     self.report(format!(
                        //         "Processing {} backorders for product {}",
                        //         orders.len(),
                        //         product_id
                        //     ))?;
                        // }
                        let inv_rec = &inventory[product_id];

                        while !orders.is_empty() {
                            let mut order = self.order_by_idx(*orders.front().unwrap())?;

                            if order.product_id != product_id as u32 {
                                return Err(simerr!(
                                    "Backorder {} product ID {} does not match the shipment's product ID {}",
                                    order.id,
                                    order.product_id,
                                    product_id
                                ));
                            }

                            if order.quantity <= inv_rec.stock() {
                                // The order can be fulfilled, remove it from the backorder queue.
                                let _ = orders.pop_front().unwrap();
                                order.status = OrderStatus::Recheck;
                                // Submit backordered order for processing.
                                inv_steps.push(Step::UpdateOrder(order));
                            }
                            else {
                                // Not enough stock to fulfill the order, leave it in the backorder queue.
                                break;
                            }
                        }
                    }
                }
            }
        }

        // self.report(
        //     "I ".to_owned()
        //         + &self
        //             .inventory()
        //             .iter()
        //             .enumerate()
        //             .map(|(id, rec)| format!("{}: {:>8}", id, rec.stock()))
        //             .collect::<Vec<_>>()
        //             .join("|"),
        // )?;

        self.add_steps(inv_steps)
    }

    fn fulfill_pending(&self) -> Result<()> {
        let mut pending_orders = self.pending_orders_mut();

        let Some(indicies) = pending_orders.remove(&self.current_day())
        else {
            return Ok(());
        };

        // Use batches of at least 1000 orders to minimize task submission overhead. However, do no more than
        // worker_count() tasks at once, as that ensures optimal CPU utilization.
        let order_count = indicies.len();
        let batch_count = (order_count / 1000 + 1).min(self.worker_count());
        let batch_size = order_count / batch_count;

        let mut tasks = vec![];

        for idx_chunk in indicies.chunks(batch_size) {
            tasks.push(self.submit_task(TaskData::Pending(idx_chunk.to_vec()))?);
        }

        for task in tasks {
            task.wait()?;
        }

        Ok(())
    }

    fn place_orders(&self) -> Result<()> {
        let customers = self.customers();
        let products = self.products();

        // First, calculate the current expected sale rate per product.
        let total_sale_rate: f64 = products.iter().map(|p| p.daily_estimate()).sum();
        let prod_sale_rate: Arc<Vec<(_, _)>> = Arc::new(
            products
                .iter()
                .map(|p| (p.id(), p.daily_estimate() / total_sale_rate))
                .collect(),
        );

        let mut task_results = Vec::new();

        let cust_count = customers.len();
        let task_count = self.worker_count();
        let batch_size = (cust_count / task_count) + (cust_count % task_count > 0) as usize;

        for cust_chunk in customers.values().cloned().collect::<Vec<_>>().chunks(batch_size) {
            let cust_chunk = cust_chunk.to_vec();

            let task = PurchaseTask {
                customers:         cust_chunk,
                product_interests: prod_sale_rate.clone(),
            };

            task_results.push(self.submit_task(TaskData::Purchase(task))?);
        }

        let mut succeed = true;
        for tr in task_results {
            if let Err(err) = tr.wait() {
                self.reporter()
                    .out(style(format!("Failed to process task: {err}")).red().bright())?;
                succeed = false;
            }
        }

        if succeed {
            Ok(())
        }
        else {
            Err(simerr!("Order processing was not successful"))
        }
    }

    fn process_returns(&self) -> Result<()> {
        let task_result = self.submit_task(TaskData::Returns)?;

        task_result.wait()
    }

    #[allow(unused)]
    fn report<S: Display>(&self, msg: S) -> Result<()> {
        self.reporter().out(msg.to_string())
    }

    fn order_by_idx(&self, idx: usize) -> Result<DbOrder> {
        let steps = self.steps();
        let Some(step) = steps.get(idx)
        else {
            return Err(simerr!("Step index {} not found", idx));
        };
        Ok(match step {
            Step::AddOrder(ref order) => order,
            Step::UpdateOrder(ref order) => order,
            _ => Err(simerr!("Script step at index {} is not an order", idx))?,
        }
        .clone())
    }

    fn build_worker_count(&self) -> usize {
        // Leave one core for the main thread, if possible.
        let threads = (num_cpus::get()).max(1);
        self.reporter().set_task_count(threads);
        threads
    }

    fn build_task_tx(&self) -> Sender<Task> {
        // Leave one core for the main thread, if possible.
        let threads = self.worker_count();
        let (tx, rx) = crossbeam::channel::unbounded();
        for thread_id in 0..threads {
            let rx = rx.clone();
            let myself = self.myself().unwrap();
            let task_handler = thread::Builder::new()
                .name(format!("task_worker_{thread_id}"))
                .spawn(move || {
                    let mut retry = 1;
                    while let Err(err) = myself.task_worker(thread_id, rx.clone()) {
                        eprintln!("Task worker thread {thread_id} failed [{retry}]: {err:?}");
                        retry += 1;
                        if retry > 3 {
                            eprintln!("Task worker thread {thread_id} failed too many times, exiting");
                            break;
                        }
                    }
                    thread_id
                })
                .unwrap();
            self.task_handlers_mut().push(task_handler);
        }

        tx
    }

    fn build_reporter(&self) -> Arc<Reporter> {
        child_build!(self, Reporter).unwrap()
    }

    fn build_random_pool(&self) -> Arc<RndPool> {
        child_build!(
            self,
            RndPool {
                min_customer_orders: self.min_customer_orders,
                max_customer_orders: self.max_customer_orders,
                customers_full:      100,
            }
        )
        .unwrap()
    }

    fn build_return_probs(&self) -> BTreeMap<u32, f64> {
        let mut return_probs = BTreeMap::new();
        let return_window = self.return_window() as f64;
        for product in self.products().iter() {
            return_probs.insert(product.id(), product.expected_return_rate() / return_window);
        }
        return_probs
    }

    fn build_total_backordered(&self) -> usize {
        self.backorders().iter().map(|bo| bo.len()).sum()
    }

    fn build_customer_model(&self) -> CustomerModel {
        CustomerModel::builder()
            .initial_customers(self.initial_customers() as f64)
            .market_capacity(self.market_capacity() as f64)
            .inflection_point(self.inflection_point() as f64)
            .growth_rate(self.growth_rate())
            .build()
            .unwrap()
    }

    fn build_product_price_model(&self) -> Arc<ProductModel> {
        ProductModel::builder().build().unwrap()
    }

    fn build_customer_sessions(&self) -> Vec<Option<DbSession>> {
        vec![None; self.market_capacity() as usize]
    }

    fn build_session_base(&self) -> i64 {
        self.current_day() as i64 * 10i64.pow((self.market_capacity() as i64 * SESSION_FACTOR as i64).ilog10() + 1)
    }
}
