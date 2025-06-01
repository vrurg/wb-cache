use anyhow::Result;
use fieldx::fxstruct;
use rand::Rng;
use statrs::distribution::ContinuousCDF;
use statrs::distribution::Normal;

pub fn bisect(v0: f64, v1: f64, expected: f64, tolerance: f64, f: impl Fn(f64) -> f64) -> Result<f64> {
    let mut v0 = v0;
    let mut v1 = v1;

    for _ in 0..1000 {
        let v2 = (v0 + v1) / 2.0;
        let f2 = f(v2);
        let diff = f2 - expected;

        if diff.abs() < tolerance {
            return Ok(v2);
        }
        if f2 < expected {
            v0 = v2;
        }
        else {
            v1 = v2;
        }

        if v0 == v1 {
            break;
        }
    }

    Err(anyhow::anyhow!("Bisection failed to converge"))
}

/// Skew normal distribution truncated to [a, b]
#[fxstruct(no_new, builder(post_build), get(copy))]
pub struct TruncatedSkewNormal {
    location: f64,
    scale:    f64,
    shape:    f64,
    lower:    f64,
    upper:    f64,

    #[fieldx(private, get(off), builder(off))]
    cdf_a: f64,
    #[fieldx(private, get(off), builder(off))]
    cdf_b: f64,
}

impl TruncatedSkewNormal {
    fn post_build(mut self) -> Self {
        self.cdf_a = self.skew_cdf(self.lower);
        self.cdf_b = self.skew_cdf(self.upper);
        self
    }

    fn skew_cdf(&self, x: f64) -> f64 {
        let z = (x - self.location) / self.scale;
        let normal = Normal::new(0.0, 1.0).unwrap();
        let phi = normal.cdf(z);
        let psi = normal.cdf(self.shape * z);
        2.0 * phi * psi
    }

    fn skew_ppf(&self, p: f64) -> f64 {
        // Invert CDF via binary search
        let mut lo = self.lower;
        let mut hi = self.upper;
        for _ in 0..100 {
            let mid = (lo + hi) / 2.0;
            let cdf = self.skew_cdf(mid);
            if cdf < p {
                lo = mid;
            }
            else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    }

    pub fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> f64 {
        let u = rng.random_range(self.cdf_a..self.cdf_b);
        self.skew_ppf(u)
    }
}
