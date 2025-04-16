use fieldx::fxstruct;

use crate::test::scriptwriter::math::bisect;

#[fxstruct(sync, no_new, builder(post_build), get(copy))]
pub struct CustomerModel {
    /// The initial number of customers.
    initial_customers: f64,
    /// The maximum number of customers the company can have.
    market_capacity:   f64,
    /// Where customer base growth reaches its peak.
    inflection_point:  f64,
    /// Company's "success" rate – how fast the customer base grows
    #[fieldx(lock, set)]
    growth_rate:       f64,
    /// Precision of the bisection method
    #[fieldx(default(0.0001))]
    tolerance:         f64,
    #[fieldx(private, set, builder(off))]
    v:                 f64,
    #[fieldx(private, set, builder(off))]
    q:                 f64,
}

impl CustomerModel {
    pub fn new(initial_customers: f64) -> Self {
        Self::builder().initial_customers(initial_customers).build().unwrap()
    }

    fn post_build(mut self) -> Self {
        self.set_v(self.calc_v());
        self.set_q(self.calc_q());
        self
    }

    fn calc_v(&self) -> f64 {
        // Use bisection method to find v
        let v0 = 0.0;
        let v1 = 10.0;
        let expected = self.inflection_point / self.market_capacity;

        #[inline(always)]
        fn coeff(v: f64) -> f64 {
            (v / (1.0 + v)).powf(1.0 / v)
        }

        bisect(v0, v1, expected, self.tolerance, coeff).expect("failed to bisect Richards model asymmetry parameter v")
    }

    fn calc_q(&self) -> f64 {
        (self.market_capacity / self.initial_customers).powf(self.v()) - 1.0
    }

    pub fn adjust_growth_rate(&self, expected_customers: f64, day: u32) {
        let gr0 = 0.0;
        let gr1 = 0.1;
        let day = day;

        self.set_growth_rate(gr1);

        self.set_growth_rate(
            bisect(gr0, gr1, expected_customers, self.tolerance, |gr| {
                self.set_growth_rate(gr);
                self.expected_customers(day)
            })
            .expect("failed to bisect growth rate"),
        );
    }

    /// Richards growth function
    /// https://en.wikipedia.org/wiki/Generalised_logistic_function
    pub fn expected_customers(&self, t: u32) -> f64 {
        let t = t as f64;
        self.market_capacity / (1.0 + self.q() * (-self.growth_rate() * t).exp()).powf(1.0 / self.v)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_v_param() {
        let richards = CustomerModel::builder()
            .initial_customers(1.)
            .market_capacity(1_000_000.)
            .inflection_point(200_000.)
            .tolerance(0.0001)
            .build()
            .unwrap();
        let v = richards.v();
        assert_eq!((v * 10000.0).round(), 6058.);
        richards.adjust_growth_rate(500_000., 365);
        assert_eq!((richards.growth_rate() * 10000.0).round(), 0247.);
    }
}
