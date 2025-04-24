use anyhow::anyhow;
use anyhow::Result;
use fieldx::fxstruct;
use rand_distr::Distribution;
use rand_distr::Gamma;
use rand_distr::Poisson;

use crate::test::db::entity::Customer as DbCustomer;

#[derive(Clone, Debug)]
#[fxstruct(no_new, builder(post_build), get)]
pub struct Customer {
    #[fieldx(copy)]
    pub id:            u32,
    pub first_name:    String,
    pub last_name:     String,
    pub email:         String,
    #[fieldx(copy, default(-1))]
    pub registered_on: i32,

    #[fieldx(copy)]
    erratic_quotient: f64,
    #[fieldx(copy)]
    orders_per_day:   f64,

    /// The Gamma for Negative Binomial distribution used to sample the number of items of this product sold to a single
    /// customer per day.
    #[fieldx(optional, builder(off), set(private), get("_nb_gamma", as_ref))]
    nb_gamma: Gamma<f64>,
}

impl Customer {
    fn post_build(mut self) -> Self {
        let eq = self.erratic_quotient();
        let shape = self.orders_per_day() / eq;
        self.set_nb_gamma(Gamma::new(shape, eq).unwrap());

        self
    }

    #[inline(always)]
    pub fn nb_gamma(&self) -> &Gamma<f64> {
        self._nb_gamma().unwrap()
    }

    pub fn daily_purchases(&self) -> Result<f64> {
        let mut rng = &mut rand::rng();
        let lambda = self.nb_gamma().sample(&mut rng);
        if lambda == 0. {
            return Ok(0.0);
        }
        Ok(Poisson::new(lambda)
            .map_err(|err| {
                anyhow!("Poisson(lambda={lambda}): {}", err).context(format!(
                    "Customer EQ: {}; orders per day: {}",
                    self.erratic_quotient(),
                    self.orders_per_day()
                ))
            })?
            .sample(&mut rng) as f64)
    }
}

impl From<Customer> for DbCustomer {
    fn from(customer: Customer) -> Self {
        DbCustomer {
            id:            customer.id,
            first_name:    customer.first_name,
            last_name:     customer.last_name,
            email:         customer.email,
            registered_on: customer.registered_on,
        }
    }
}
