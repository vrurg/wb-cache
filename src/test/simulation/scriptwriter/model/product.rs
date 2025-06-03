use anyhow::Result;
use fieldx::fxstruct;
use rand_distr::Distribution;
use rand_distr::LogNormal;
use rand_distr::SkewNormal;
use statrs::distribution::ContinuousCDF;
use statrs::distribution::LogNormal as StatLN;

use crate::test::simulation::scriptwriter::math::bisect;

/// Implementation of the LogNormal price distribution model. The model takes the following expectations:
/// - the most popular price value
/// - the most popular price range
/// - the expected share of the popular price range of all prices (normally - 2/3 of all)
///
/// The model then finds the parameters of the LogNormal distribution that best fit the given expectations and uses
/// these to sample prices for modeled products.
#[derive(Debug)]
#[fxstruct(sync, builder(post_build), rc, get(copy))]
pub struct ProductModel {
    /// The most popular price value
    #[fieldx(default(20.0))]
    top_price: f64,
    /// Lower bound of the most popular price range
    #[fieldx(default(1.0))]
    top_low:   f64,
    /// Upper bound of the most popular price range
    #[fieldx(default(100.0))]
    top_high:  f64,
    /// The expected share of the popular price range of all prices (normally - 2/3 of all)
    #[fieldx(default(2.0/3.0))]
    top_share: f64,
    #[fieldx(default(0.0001))]
    tolerance: f64,

    // Customer interest in the product
    /// The median price at which customer losts 1/2 of interest in the product, compared to the maximum.
    #[fieldx(default(300.))]
    pivot_price:        f64,
    /// The higher the value – the steeper the interest loss curve around the pivot price.
    #[fieldx(default(0.005))]
    interest_loss_rate: f64,
    /// This constant defines the point up to which customer interest remains stable with no significant loss.
    /// Empirically, for prices <120 it can be estimated as (price / (2 * π))^2. For higher values the discrepancy
    /// between the expected price and where the graph starts to fall becomes significant enough. The current default
    /// value is 162, which corresponds to the price of 80.
    #[fieldx(private, default(162.))]
    interest_stability: f64,

    #[fieldx(lock, private, set, builder(off))]
    mu:    f64,
    #[fieldx(lock, private, set, builder(off))]
    sigma: f64,
}

impl ProductModel {
    fn post_build(self) -> Self {
        self.find_sigma();
        self
    }

    fn find_sigma(&self) {
        bisect(0.01, 5.0, 0.0, self.tolerance(), |sigma| {
            self.set_sigma(sigma);
            self.probability_difference()
        })
        .expect("failed to bisect sigma parameter for price model");
    }

    fn probability_difference(&self) -> f64 {
        let sigma = self.sigma();
        let mu = self.top_price().ln() + sigma * sigma;
        self.set_mu(mu);
        let dist = StatLN::new(mu, sigma).expect("bad parameters for LogNormal distribution");
        self.top_share() - (dist.cdf(self.top_high()) - dist.cdf(self.top_low()))
    }

    pub fn next_price(&self) -> f64 {
        let dist = LogNormal::new(self.mu(), self.sigma()).expect("bad parameters for LogNormal distribution");
        let mut rng = rand::rng();
        dist.sample(&mut rng)
    }

    pub fn customer_interest(&self, price: f64) -> f64 {
        if price < 0. {
            panic!("Price cannot be negative");
        }

        // The interest loss rate is actually an arctangent over the price. This way we can get a clear range of
        // customer interest from 0 to 1. The higher the price, the lower the interest. Since arctangent itself doesn't
        // represent empirical expectations well we need to transform the price coordinate to get a better fit. The
        // final function "implements" the curve where customer interest is close to 100% up to some psychological point
        // (we set it as 80 in the defaults) and then starts to fall down. The pivot price is the point where the
        // interest loss rate is 50% of the maximum. The further decline of interest is less dramatic and it goes down
        // to ~10% at around 1000.

        let pivot = self.pivot_price();
        // This is actually a coordinate transformation of the interest loss curve.
        let price = price - pivot / ((self.interest_stability() * (price - pivot)) / (pivot + price.powi(2))).exp();

        0.5 - (self.interest_loss_rate() * price).atan() / std::f64::consts::PI
    }

    pub fn supply_distribution(
        &self,
        expected_mean: f64,
        supplier_inaccuracy: f64,
        supplier_tardiness: f64,
    ) -> Result<SkewNormal<f64>> {
        let scale = supplier_inaccuracy * expected_mean / 1.6448536;
        let shape = (std::f64::consts::PI * (supplier_tardiness - 0.5)).tan();

        SkewNormal::new(expected_mean, scale, shape)
            .map_err(|_| anyhow::anyhow!("bad parameters for SkewNormal distribution"))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_product_price() {
        let pp = ProductModel::builder()
            .top_price(15.0)
            .top_low(1.0)
            .top_high(100.0)
            .top_share(2.0 / 3.0)
            .tolerance(0.000001)
            .build()
            .unwrap();
        assert_eq!((pp.sigma() * 100000.).round(), 117844.);
        assert_eq!((pp.mu() * 100000.).round(), 409676.);
    }
}
