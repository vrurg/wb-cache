use std::sync::Arc;
use std::thread;

use crossbeam::queue::SegQueue;
use crossbeam::sync::Parker;
use crossbeam::sync::Unparker;
use fake::Fake;
use fieldx_plus::fx_plus;
use rand::rngs::ThreadRng;
use rand::Rng;
use rand_distr::Distribution;
use rand_distr::Gamma;
use statrs::distribution::ContinuousCDF;
use statrs::distribution::Gamma as StatrsGamma;

use crate::test::simulation::types::simerr;
use crate::test::simulation::types::SimErrorAny;

use super::entity::customer::Customer;
use super::math::bisect;
use super::reporter::Reporter;
use super::ScriptWriter;

#[fx_plus(
    child(ScriptWriter, unwrap(or_else(SimErrorAny, no_scenario))),
    rc,
    sync,
    default(off)
)]
pub struct RndPool {
    #[fieldx(lock, get(copy), get_mut, default(500))]
    randoms_min: usize,

    #[fieldx(lock, get(copy), get_mut, default(1000))]
    randoms_full: usize,

    #[fieldx(get, get_mut, default(SegQueue::new()))]
    randoms: SegQueue<f64>,

    #[fieldx(lock, get(copy), get_mut, default(50))]
    customers_min: usize,

    #[fieldx(lock, get(copy), get_mut, set, default(100))]
    customers_full: usize,

    // Expected customer orders per day parameters.
    #[fieldx(get(copy), default(0.15))]
    min_customer_orders: f64,
    /// This value is not a hard upper limit but the boundary within which min_max_order_fraction of randomly generated
    /// customer expected daily orders will fall. (1 - min_max_order_fraction) of all customers may get a higher value.
    #[fieldx(get(copy), default(3.))]
    max_customer_orders: f64,
    #[fieldx(get(copy), default(0.95))]
    min_max_order_fraction: f64,
    #[fieldx(copy, default(2.0))]
    orders_gamma_shape: f64,

    // Erratic quotient parameters.
    /// Similarly to max_customer_orders, this value is not a hard upper limit.
    #[fieldx(get(copy), default(3.))]
    max_erratic_quotient: f64,
    /// Fraction of erratic quotient values to fall into 0..max_erratic_quotient range.
    #[fieldx(get(copy), default(0.9))]
    eq_fraction: f64,
    #[fieldx(get(copy), default(2.))]
    eq_gamma_shape: f64,

    #[fieldx(copy, default(0.00001))]
    tolerance: f64,

    #[fieldx(lock, get, get_mut, default(SegQueue::new()))]
    customers: SegQueue<Customer>,

    #[fieldx(lock, get(copy), get_mut, default(0))]
    customer_id: i32,

    // #[fieldx(lazy, builder(off))]
    // tx: Sender<bool>,
    #[fieldx(lazy, get)]
    generator_unparker: Unparker,

    #[fieldx(lazy)]
    orders_gamma: Gamma<f64>,

    #[fieldx(lazy)]
    erratic_quotient_gamma: Gamma<f64>,

    #[fieldx(lazy)]
    reporter: Arc<Reporter>,

    #[fieldx(lock, private, get(copy), set, builder(off))]
    shutdown: bool,
}

macro_rules! next_method {
    ( $( $method:ident, $name:ident, $type:ty );+ $(;)? ) => {
        $( paste::paste! {
            #[inline(always)]
            pub fn [<next_ $method>](&self) -> $type {
                if self.$name().len() < self.[<$name _min>]() {
                    // Refill a half-empty buffer.
                    self.generator_unparker().unpark();
                    // self.tx().send(true).unwrap();
                }
                loop {
                    if let Some(c) = self.$name().pop() {
                        return c;
                    }
                    else {
                        self.generator_unparker().unpark();
                    }
                }
            }
        } )+
    };
}

impl RndPool {
    next_method! {
        rand, randoms, f64;
        customer, customers, Customer;
    }

    // fn build_tx(&self) -> Sender<bool> {
    //     let (tx, rx) = std::sync::mpsc::channel();
    //     self.start(rx);
    //     tx
    // }

    fn build_generator_unparker(&self) -> Unparker {
        let parker = Parker::new();
        let unparker = parker.unparker().clone();
        self.start(parker);
        unparker
    }

    fn solve_gamma_scale(&self, shape: f64, min_scale: f64, max_scale: f64, fraction: f64, upper_bound: f64) -> f64 {
        bisect(min_scale, max_scale, fraction, self.tolerance, |scale| {
            let gamma = StatrsGamma::new(shape, scale).unwrap();
            let min = gamma.cdf(0.0);
            let max = gamma.cdf(upper_bound);
            max - min
        })
        .expect("failed to bisect gamma distribution scale")
    }

    fn build_orders_gamma(&self) -> Gamma<f64> {
        Gamma::new(
            self.orders_gamma_shape,
            self.solve_gamma_scale(
                self.orders_gamma_shape,
                self.tolerance,
                100.0,
                self.min_max_order_fraction(),
                self.max_customer_orders() - self.min_customer_orders(),
            ),
        )
        .unwrap()
    }

    fn build_erratic_quotient_gamma(&self) -> Gamma<f64> {
        Gamma::new(
            self.eq_gamma_shape,
            self.solve_gamma_scale(
                self.eq_gamma_shape,
                self.tolerance,
                100.0,
                self.eq_fraction(),
                self.max_erratic_quotient() - 1.0,
            ),
        )
        .unwrap()
    }

    fn build_reporter(&self) -> Arc<Reporter> {
        self.parent().unwrap().reporter().clone()
    }

    pub fn start(&self, parker: Parker) {
        let me = self.myself().unwrap();
        thread::Builder::new()
            .name("rnd_pool".into())
            .spawn(move || loop {
                let mut rng = rand::rng();
                while !me.shutdown() {
                    me.replenish(&mut rng);
                    parker.park();
                }
            })
            .unwrap();
    }

    fn no_scenario(&self) -> SimErrorAny {
        simerr!("Scenario object is gone!")
    }

    fn replenish(&self, rng: &mut ThreadRng) {
        let mut incomplete = true;
        self.reporter()
            .set_rnd_pool_task_status(super::reporter::TaskStatus::Busy);

        if self.randoms().is_empty() {
            let _ = self.reporter().out(format!(
                "Doubling randoms pool size from {}/{}",
                self.randoms_min(),
                self.randoms_full()
            ));
            *self.randoms_full_mut() *= 2;
            *self.randoms_min_mut() *= 2;
        }

        if self.customers().is_empty() {
            let _ = self.reporter().out(format!(
                "Doubling customers pool size from {}/{}",
                self.customers_min(),
                self.customers_full()
            ));
            *self.customers_full_mut() *= 2;
            *self.customers_min_mut() *= 2;
        }

        while incomplete {
            incomplete = false;
            if self.randoms().len() < self.randoms_full() {
                self.randoms().push(rng.random_range(0.0..=1.0));
                incomplete = true;
            }

            if self.customers().len() < self.customers_full() {
                let customer = self.new_customer(rng);
                self.customers().push(customer);
                incomplete = true;
            }
        }
        self.reporter()
            .set_rnd_pool_task_status(super::reporter::TaskStatus::Idle);
    }

    fn new_customer(&self, rng: &mut ThreadRng) -> Customer {
        *self.customer_id_mut() += 1;
        let email = format!(
            "{}{}@{}.{}",
            fake::faker::lorem::en::Word().fake::<String>(),
            self.customer_id(),
            fake::faker::lorem::en::Word().fake::<String>(),
            fake::faker::internet::en::DomainSuffix().fake::<String>(),
        );
        Customer::builder()
            .id(self.customer_id())
            .first_name(fake::faker::name::en::FirstName().fake())
            .last_name(fake::faker::name::en::LastName().fake())
            .email(email.clone())
            .erratic_quotient(self.erratic_quotient_gamma().sample(rng))
            .orders_per_day(self.orders_gamma().sample(rng) + self.min_customer_orders())
            .build()
            .unwrap()
    }
}

impl Drop for RndPool {
    fn drop(&mut self) {
        self.set_shutdown(true);
        self.generator_unparker().unpark();
    }
}
