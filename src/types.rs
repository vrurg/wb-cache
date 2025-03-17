use crate::{entry::WBEntry, traits::WBDataController};
use std::{fmt::Debug, sync::Arc};

pub enum WBCompResult<DC>
where
    DC: WBDataController,
{
    Inserted(WBEntry<DC>),
    ReplacedWith(WBEntry<DC>),
    Removed(WBEntry<DC>),
    Unchanged(WBEntry<DC>),
    StillNone(Arc<DC::Key>),
}

impl<DC> Debug for WBCompResult<DC>
where
    DC: WBDataController,
{
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inserted(e) => fmt.debug_tuple("WBCompResult::Inserted").field(e).finish(),
            Self::Removed(e) => fmt.debug_tuple("WBCompResult::Removed").field(e).finish(),
            Self::ReplacedWith(e) => fmt.debug_tuple("WBCompResult::ReplacedWith").field(e).finish(),
            Self::Unchanged(e) => fmt.debug_tuple("WBCompResult::Unchanged").field(e).finish(),
            Self::StillNone(k) => fmt.debug_tuple("WBCompResult::StillNone").field(k).finish(),
        }
    }
}

pub enum WBPlan<DC>
where
    DC: WBDataController,
{
    Invalidate(DC::Key),
    Nop(DC::Key),
}
