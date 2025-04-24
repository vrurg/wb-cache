use std::borrow::Cow;

use indicatif::MultiProgress;
use indicatif::ProgressBar;

#[allow(dead_code)]
pub trait MaybeProgress {
    fn maybe_inc(&self, n: u64);
    fn maybe_enable_steady_tick(&self, dur: std::time::Duration);
    fn maybe_finish(&self);
    fn maybe_finish_with_message(&self, msg: impl Into<Cow<'static, str>>);
    fn maybe_set_message(&self, msg: impl Into<Cow<'static, str>>);
    fn maybe_set_position(&self, pos: u64);
}

impl MaybeProgress for Option<ProgressBar> {
    fn maybe_inc(&self, n: u64) {
        if let Some(pb) = self {
            pb.inc(n);
        }
    }

    fn maybe_enable_steady_tick(&self, dur: std::time::Duration) {
        if let Some(pb) = self {
            pb.enable_steady_tick(dur);
        }
    }

    fn maybe_finish(&self) {
        if let Some(pb) = self {
            pb.finish();
        }
    }

    fn maybe_finish_with_message(&self, msg: impl Into<Cow<'static, str>>) {
        if let Some(pb) = self {
            pb.finish_with_message(msg);
        }
    }

    fn maybe_set_message(&self, msg: impl Into<Cow<'static, str>>) {
        if let Some(pb) = self {
            pb.set_message(msg);
        }
        else {
            println!("{}", msg.into());
        }
    }

    fn maybe_set_position(&self, pos: u64) {
        if let Some(pb) = self {
            pb.set_position(pos);
        }
    }
}

pub trait MaybeMultiProgress {
    fn maybe_add(&self, pb: ProgressBar) -> Option<ProgressBar>;
    #[allow(dead_code)]
    fn maybe_insert_before(&self, before: Option<&ProgressBar>, pb: ProgressBar) -> Option<ProgressBar>;
    #[allow(dead_code)]
    fn maybe_insert_after(&self, after: Option<&ProgressBar>, pb: ProgressBar) -> Option<ProgressBar>;
    fn maybe_insert_from_back(&self, index: usize, pb: ProgressBar) -> Option<ProgressBar>;
    fn maybe_remove(&self, pb: Option<ProgressBar>);
}

impl MaybeMultiProgress for Option<MultiProgress> {
    fn maybe_add(&self, pb: ProgressBar) -> Option<ProgressBar> {
        self.as_ref().map(|mp| mp.add(pb))
    }

    fn maybe_insert_before(&self, before: Option<&ProgressBar>, pb: ProgressBar) -> Option<ProgressBar> {
        self.as_ref().map(|mp| {
            if let Some(before) = before {
                mp.insert_before(before, pb)
            }
            else {
                mp.add(pb)
            }
        })
    }

    fn maybe_insert_after(&self, after: Option<&ProgressBar>, pb: ProgressBar) -> Option<ProgressBar> {
        if let Some(mp) = self {
            if let Some(after) = after {
                Some(mp.insert_after(after, pb))
            }
            else {
                Some(mp.add(pb))
            }
        }
        else {
            None
        }
    }

    fn maybe_insert_from_back(&self, index: usize, pb: ProgressBar) -> Option<ProgressBar> {
        self.as_ref().map(|mp| mp.insert_from_back(index, pb))
    }

    fn maybe_remove(&self, pb: Option<ProgressBar>) {
        if let Some(mp) = self {
            if let Some(pb) = &pb {
                mp.remove(pb);
            }
        }
    }
}
