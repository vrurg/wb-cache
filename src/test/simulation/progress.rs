pub mod traits;

use std::fmt::Debug;
use std::fmt::Display;
use std::fmt::Write;
use std::time::Instant;

use console::Style;
use fieldx::fxstruct;
use indicatif::style::ProgressTracker;
use indicatif::MultiProgress;
use indicatif::ProgressBar;
use indicatif::ProgressState;
use indicatif::ProgressStyle;
pub use traits::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MsgType {
    Debug,
    Info,
    Warn,
    Error,
}

impl Display for MsgType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MsgType::Debug => write!(f, "DEBUG"),
            MsgType::Info => write!(f, "INFO"),
            MsgType::Warn => write!(f, "WARN"),
            MsgType::Error => write!(f, "ERROR"),
        }
    }
}

pub enum POrder<'a> {
    Before(Option<&'a ProgressBar>),
    After(Option<&'a ProgressBar>),
}

pub enum PStyle {
    Main,
    Minor,
    Message,
    Custom(ProgressStyle),
}

impl Debug for PStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PStyle::Main => write!(f, "Main"),
            PStyle::Minor => write!(f, "Minor"),
            PStyle::Message => write!(f, "Message"),
            PStyle::Custom(_) => write!(f, "Custom"),
        }
    }
}

struct PerSecFmt;

impl ProgressTracker for PerSecFmt {
    fn clone_box(&self) -> Box<dyn ProgressTracker> {
        Box::new(PerSecFmt)
    }

    fn tick(&mut self, _state: &ProgressState, _now: Instant) {}

    fn reset(&mut self, _state: &ProgressState, _now: Instant) {}

    fn write(&self, state: &ProgressState, w: &mut dyn Write) {
        // Writes, for example, "12.34/s"
        write!(w, "{:.2}/s", state.per_sec()).unwrap();
    }
}

#[fxstruct(new(off), sync, fallible(off, error(SimError)), builder)]
pub struct ProgressUI {
    #[fieldx(get(copy), default(false))]
    quiet: bool,

    #[fieldx(lazy, get, builder(off))]
    multi_progress: Option<MultiProgress>,

    #[fieldx(lazy, get(copy))]
    user_attended: bool,

    #[fieldx(lazy, private, get(clone))]
    progress_style_main: ProgressStyle,

    #[fieldx(lazy, private, get(clone))]
    progress_style_minor: ProgressStyle,

    #[fieldx(lazy, private, get(clone))]
    progress_style_message: ProgressStyle,
}

impl ProgressUI {
    fn build_user_attended(&self) -> bool {
        !self.quiet && console::user_attended()
    }

    fn build_multi_progress(&self) -> Option<MultiProgress> {
        if self.user_attended() {
            Some(MultiProgress::new())
        } else {
            None
        }
    }

    #[inline(always)]
    fn build_progress_style_main(&self) -> ProgressStyle {
        ProgressStyle::default_bar()
            .template("{prefix}: [{elapsed_precise:.cyan}] [{eta:>4}] {percent_precise:>7}% {bar:30.cyan.on_240} {pos:>2.cyan}/{len:>2.cyan} {per_sec_short} {msg:.cyan}")
            .expect("Main progress style")
            .with_key("per_sec_short", PerSecFmt)
            .progress_chars("█▉▊▋▌▍▎▏ ")
    }

    #[inline(always)]
    fn build_progress_style_minor(&self) -> ProgressStyle {
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:5.250.on_240} {pos:>2}/{len:>2} ({prefix}) {msg}")
            .expect("Minor progress style")
            .progress_chars("█▉▊▋▌▍▎▏ ")
    }

    #[inline(always)]
    fn build_progress_style_message(&self) -> ProgressStyle {
        ProgressStyle::default_bar()
            .template("{prefix}: {msg}")
            .expect("Message progress style")
    }

    pub fn message_style(&self, msg_type: MsgType) -> Style {
        match msg_type {
            MsgType::Debug => Style::new().magenta().for_stderr(),
            MsgType::Info => Style::new(),
            MsgType::Warn => Style::new().yellow().for_stderr(),
            MsgType::Error => Style::new().red().for_stderr(),
        }
    }

    fn _println(&self, msg_type: MsgType, msg: String) {
        if self.quiet {
            return;
        }

        let prefix = self.message_style(msg_type).apply_to(format!("[{msg_type}]"));
        if matches!(msg_type, MsgType::Info) {
            println!("{prefix} {msg}");
        } else {
            eprintln!("{prefix} {msg}");
        }
    }

    pub fn print_message<S: ToString>(&self, msg_type: MsgType, msg: S) {
        let msg = msg.to_string();
        if let Some(mp) = self.multi_progress().as_ref() {
            mp.suspend(|| {
                self._println(msg_type, msg);
            })
        } else {
            self._println(msg_type, msg);
        }
    }

    pub fn report_error<S: ToString>(&self, msg: S) {
        self.print_message(MsgType::Error, msg);
    }

    pub fn report_warn<S: ToString>(&self, msg: S) {
        self.print_message(MsgType::Warn, msg);
    }

    pub fn report_info<S: ToString>(&self, msg: S) {
        self.print_message(MsgType::Info, msg);
    }

    pub fn report_debug<S: ToString>(&self, msg: S) {
        self.print_message(MsgType::Debug, msg);
    }

    pub fn progress_style(&self, style: PStyle) -> ProgressStyle {
        match style {
            PStyle::Main => self.progress_style_main(),
            PStyle::Minor => self.progress_style_minor(),
            PStyle::Message => self.progress_style_message(),
            PStyle::Custom(custom) => custom,
        }
    }

    pub fn acquire_progress(&self, style: PStyle, order: Option<POrder>) -> Option<ProgressBar> {
        let mp = self.multi_progress();

        if mp.is_none() {
            return None;
        }

        let style = self.progress_style(style);

        if let Some(order) = order {
            match order {
                POrder::Before(before) => {
                    mp.maybe_insert_before(before, ProgressBar::new_spinner().with_style(style.clone()))
                }
                POrder::After(after) => {
                    mp.maybe_insert_after(after, ProgressBar::new_spinner().with_style(style.clone()))
                }
            }
        } else {
            mp.maybe_add(ProgressBar::new(0).with_style(style.clone()))
        }
    }

    pub fn remove(&self, pb: Option<ProgressBar>) {
        self.multi_progress().maybe_remove(pb);
    }

    pub fn finish(&self) {
        // self.multi_progress.clear();
    }
}

impl Drop for ProgressUI {
    fn drop(&mut self) {
        self.finish();
    }
}
