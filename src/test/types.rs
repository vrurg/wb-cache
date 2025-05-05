use std::fmt::Debug;
use std::fmt::Display;
use std::sync::Arc;

use parking_lot::RwLock;
use sea_orm::DbErr;
use sea_orm::DeriveActiveEnum;
use sea_orm::EnumIter;
use serde::Deserialize;
use serde::Serialize;
use std::mem;

use super::scriptwriter::steps::Step;

macro_rules! simerr {
    ($fmt:literal $(, $( $tt:tt )+)?) => {
        $crate::test::types::SimErrorAny::with_anyhow(anyhow::anyhow!($fmt $(, $( $tt )+)?))
    };
}

pub(crate) use simerr;

pub enum SimError {
    Any(SimErrorAny),
    Sim(Arc<SimErrorAny>),
}

impl SimError {
    pub fn new<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Any(SimErrorAny::new(err))
    }

    pub fn context<C>(&self, context: C)
    where
        C: Display + Send + Sync + 'static,
    {
        match self {
            Self::Any(err) => {
                err.context(context);
            }
            Self::Sim(err) => err.context(context),
        }
    }

    pub fn report_with_backtrace<S: Display>(&self, msg: S) -> String {
        match self {
            Self::Any(err) => err.report_with_backtrace(msg),
            Self::Sim(err) => err.report_with_backtrace(msg),
        }
    }
}

impl Debug for SimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any(err) => Debug::fmt(err, f),
            Self::Sim(err) => Debug::fmt(err, f),
        }
    }
}

impl Display for SimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any(err) => Display::fmt(err, f),
            Self::Sim(err) => Display::fmt(err, f),
        }
    }
}

impl From<anyhow::Error> for SimError {
    fn from(err: anyhow::Error) -> Self {
        Self::Any(SimErrorAny(RwLock::new(err)))
    }
}

impl From<Arc<SimErrorAny>> for SimError {
    fn from(err: Arc<SimErrorAny>) -> Self {
        Self::Sim(err)
    }
}

impl From<SimErrorAny> for SimError {
    fn from(err: SimErrorAny) -> Self {
        Self::Any(err)
    }
}

impl From<DbErr> for SimError {
    fn from(err: DbErr) -> Self {
        Self::Any(SimErrorAny(RwLock::new(anyhow::Error::from(err))))
    }
}

impl From<SimError> for SimErrorAny {
    fn from(err: SimError) -> Self {
        match err {
            SimError::Any(e) => e,
            SimError::Sim(e) => simerr!("{}", e),
        }
    }
}

pub type Result<T, ERR = SimErrorAny> = std::result::Result<T, ERR>;

#[derive(Debug)]
pub struct SimErrorAny(RwLock<anyhow::Error>);

impl<E> From<E> for SimErrorAny
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(err: E) -> Self {
        Self(RwLock::new(anyhow::Error::new(err)))
    }
}

impl SimErrorAny {
    pub fn new<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self(RwLock::new(anyhow::Error::new(err)))
    }

    pub fn with_anyhow(err: anyhow::Error) -> Self {
        Self(RwLock::new(err))
    }

    pub fn context<C>(&self, context: C)
    where
        C: Display + Send + Sync + 'static,
    {
        let mut error = self.0.write();
        let err = mem::replace(&mut *error, anyhow::Error::msg("temp"));
        let err = err.context(context);
        *error = err;
    }

    pub fn report_with_backtrace<S: Display>(&self, msg: S) -> String {
        format!("{msg}\n{}", self.0.read().backtrace())
    }
}

impl Display for SimErrorAny {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let error = self.0.read();
        Display::fmt(&*error, f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "order_status")]
pub enum OrderStatus {
    #[sea_orm(string_value = "New")]
    #[serde(rename = "n")]
    New,
    #[sea_orm(string_value = "Backordered")]
    #[serde(rename = "b")]
    Backordered,
    // Re-check if we can proceed with a backordered order.
    #[sea_orm(string_value = "Recheck")]
    #[serde(rename = "c")]
    Recheck,
    #[sea_orm(string_value = "Pending")]
    #[serde(rename = "p")]
    Pending,
    #[sea_orm(string_value = "Shipped")]
    #[serde(rename = "s")]
    Shipped,
    #[sea_orm(string_value = "Refunded")]
    #[serde(rename = "r")]
    Refunded,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    #[serde(rename = "l")]
    pub length: usize,
    #[serde(rename = "s")]
    pub steps:  Vec<Step>,
}
