use std::sync::Arc;

use fieldx::fxstruct;
use rand_distr::Distribution;
use rand_distr::SkewNormal;

use crate::test::db::entity::Product as DbProduct;
use crate::test::scriptwriter::model::product::ProductModel;

#[derive(Clone, Debug)]
#[fxstruct(sync, no_new, builder, get(copy))]
pub struct Product {
    /// Unique product ID
    id:                   u32,
    /// Product name
    #[fieldx(get(copy(off)))]
    name:                 String,
    /// Product price
    price:                f64,
    /// The expected daily customer interest for the product, expressed as a fraction of 1.0 where 1.0 means every
    /// customer wants it. This metric quantifies the product's popularity. Note that the product's price also
    /// influences final sales.
    daily_quotient:       f64,
    /// Expected return rate of the product, %/period.
    expected_return_rate: f64,
    /// Expected terms of shipment arrival from the supplier, in days. The final value for each shipment is sampled
    /// using SkewedNormal distribution with the parameters derived from the following three values. stock_supplies_in
    /// is the mean or location parameter.
    stock_supplies_in:    f64,
    /// Specifies the variability of the shipment terms. The value is a percentage of the stock_supplies_in value. The
    /// higher the value, the more uncertain the shipment terms are; values above 20% are not accepted. This value
    /// translates into the SkewNormal distribution scale parameter by the formula:
    /// scale = supplier_inaccuracy * stock_supplies_in / 1.6448536
    supplier_inaccuracy:  f64,
    /// Specifies the tendency of a supplier to delay the shipment. Positive values indicate a tendency to delay and
    /// measured as a percentage of the late shipments. Negative values indicate a tendency to deliver faster. Reasonable
    /// range for the value is between -0.9 and 0.7. Whereas the former is simply optimistic, the latter is rather
    /// the maximum reasonably acceptable in real life.
    /// The value translates into SkewedNormal distribution shape parameter by the formula:
    /// shape = tan(PI * (supplier_tardiness - 0.5))
    supplier_tardiness:   f64,

    #[fieldx(get(clone), serde(off))]
    product_model: Arc<ProductModel>,

    #[fieldx(lazy, private, get(copy(off)), builder(off), serde(off))]
    supply_distribution: SkewNormal<f64>,

    /// The expected number of items of this product sold to a single customer per day.  This value is used in the
    /// sampling process to simulate how many items of this product a customer buys in a single order.
    #[fieldx(lazy, get(copy), builder(off))]
    daily_estimate: f64,
}

impl Product {
    fn build_supply_distribution(&self) -> SkewNormal<f64> {
        self.product_model()
            .supply_distribution(
                self.stock_supplies_in(),
                self.supplier_inaccuracy(),
                self.supplier_tardiness(),
            )
            .unwrap()
    }

    fn build_daily_estimate(&self) -> f64 {
        self.product_model().customer_interest(self.price()) * self.daily_quotient()
    }

    pub fn supplies_in(&self) -> f64 {
        self.supply_distribution().sample(&mut rand::rng())
    }
}

impl From<Product> for DbProduct {
    fn from(product: Product) -> Self {
        DbProduct {
            id:    product.id,
            name:  product.name,
            price: product.price,
        }
    }
}
