use std::cell::RefCell;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use console::style;
use console::Term;
use fieldx_plus::fx_plus;
use num_format::utils::DecimalStr;
use num_format::utils::InfinityStr;
use num_format::utils::MinusSignStr;
use num_format::utils::NanStr;
use num_format::utils::PlusSignStr;
use num_format::utils::SeparatorStr;
use num_format::Format;
use num_format::Locale;
use num_format::SystemLocale;
use num_format::ToFormattedString;

use crate::test::simulation::types::simerr;
use crate::test::simulation::types::Result;
use crate::test::simulation::types::SimErrorAny;

use super::ScriptWriter;

const TICKER_CHARS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

// ScriptWriter reporter trait
pub trait SwReporter: Send + Sync + 'static {
    fn out(&self, msg: &str) -> Result<()>;
    fn refresh_report(&self) -> Result<()>;
    fn set_backorders(&self, backorders: usize);
    fn set_pending_orders(&self, orders: usize);
    fn set_rnd_pool_task_status(&self, status: TaskStatus);
    fn set_scenario_capacity(&self, capacity: usize);
    fn set_scenario_lines(&self, lines: usize);
    fn set_task_count(&self, count: usize);
    fn set_task_status(&self, task_id: usize, status: TaskStatus);
    fn start(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
}

#[derive(Clone)]
pub enum Reporter {
    Formatted(Arc<FormattedReporter>),
    Quiet,
}

impl SwReporter for Reporter {
    fn out(&self, msg: &str) -> Result<()> {
        match self {
            Reporter::Formatted(reporter) => reporter.out(msg),
            Reporter::Quiet => Ok(()),
        }
    }

    fn refresh_report(&self) -> Result<()> {
        match self {
            Reporter::Formatted(reporter) => reporter.refresh_report(),
            Reporter::Quiet => Ok(()),
        }
    }

    fn set_backorders(&self, backorders: usize) {
        if let Reporter::Formatted(reporter) = self {
            reporter.set_backorders(backorders);
        }
    }

    fn set_pending_orders(&self, orders: usize) {
        if let Reporter::Formatted(reporter) = self {
            reporter.set_pending_orders(orders);
        }
    }

    fn set_rnd_pool_task_status(&self, status: TaskStatus) {
        if let Reporter::Formatted(reporter) = self {
            reporter.set_rnd_pool_task_status(status);
        }
    }

    fn set_scenario_capacity(&self, capacity: usize) {
        if let Reporter::Formatted(reporter) = self {
            reporter.set_scenario_capacity(capacity);
        }
    }

    fn set_scenario_lines(&self, lines: usize) {
        if let Reporter::Formatted(reporter) = self {
            reporter.set_scenario_lines(lines);
        }
    }

    fn set_task_count(&self, count: usize) {
        if let Reporter::Formatted(reporter) = self {
            reporter.set_task_count(count);
        }
    }

    fn set_task_status(&self, task_id: usize, status: TaskStatus) {
        if let Reporter::Formatted(reporter) = self {
            reporter.set_task_status(task_id, status);
        }
    }

    fn start(&self) -> Result<()> {
        match self {
            Reporter::Formatted(reporter) => reporter.start(),
            Reporter::Quiet => Ok(()),
        }
    }

    fn stop(&self) -> Result<()> {
        match self {
            Reporter::Formatted(reporter) => reporter.stop(),
            Reporter::Quiet => Ok(()),
        }
    }
}

enum NumFormatter {
    Sys(Box<SystemLocale>),
    Explicit(Locale),
}

thread_local! {
    static NUM_LOCALE: RefCell<NumFormatter> = {
        RefCell::new(SystemLocale::default()
            .map_or_else(
                |_| NumFormatter::Explicit(Locale::en),
                |sl| NumFormatter::Sys(Box::new(sl)),
            ))
    };
}

impl Format for NumFormatter {
    fn decimal(&self) -> num_format::utils::DecimalStr<'_> {
        DecimalStr::new(match self {
            NumFormatter::Sys(locale) => locale.decimal(),
            NumFormatter::Explicit(locale) => locale.decimal(),
        })
        .unwrap()
    }

    fn grouping(&self) -> num_format::Grouping {
        match self {
            NumFormatter::Sys(locale) => locale.grouping(),
            NumFormatter::Explicit(locale) => locale.grouping(),
        }
    }

    fn infinity(&self) -> num_format::utils::InfinityStr<'_> {
        InfinityStr::new(match self {
            NumFormatter::Sys(locale) => locale.infinity(),
            NumFormatter::Explicit(locale) => locale.infinity(),
        })
        .unwrap()
    }

    fn minus_sign(&self) -> num_format::utils::MinusSignStr<'_> {
        MinusSignStr::new(match self {
            NumFormatter::Sys(locale) => locale.minus_sign(),
            NumFormatter::Explicit(locale) => locale.minus_sign(),
        })
        .unwrap()
    }

    fn nan(&self) -> num_format::utils::NanStr<'_> {
        NanStr::new(match self {
            NumFormatter::Sys(locale) => locale.nan(),
            NumFormatter::Explicit(locale) => locale.nan(),
        })
        .unwrap()
    }

    fn plus_sign(&self) -> num_format::utils::PlusSignStr<'_> {
        PlusSignStr::new(match self {
            NumFormatter::Sys(locale) => locale.plus_sign(),
            NumFormatter::Explicit(locale) => locale.plus_sign(),
        })
        .unwrap()
    }

    fn separator(&self) -> num_format::utils::SeparatorStr<'_> {
        SeparatorStr::new(match self {
            NumFormatter::Sys(locale) => locale.separator(),
            NumFormatter::Explicit(locale) => locale.separator(),
        })
        .unwrap()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TaskStatus {
    Idle,
    Busy,
}

#[fx_plus(child(ScriptWriter, unwrap(or_else(SimErrorAny, no_scenario))), sync, rc)]
pub struct FormattedReporter {
    #[fieldx(private, lock, get, get_mut, builder(off), default(Vec::new()))]
    task_status: Vec<TaskStatus>,

    #[fieldx(private, lock, get(copy), get_mut, builder(off), default(TaskStatus::Idle))]
    rnd_pool_task: TaskStatus,

    #[fieldx(private, lock, get, set, builder(off))]
    task_line: String,

    #[fieldx(lock, clearer, private, set, get(off))]
    refresher_handler: JoinHandle<Result<()>>,

    #[fieldx(private, lock, set, get(copy), builder(off))]
    shutdown: bool,

    #[fieldx(lock, set("_set_scenario_lines"), get(copy), builder(off), default(0))]
    scenario_lines: usize,

    #[fieldx(lock, set("_set_scenario_capacity"), get(copy), builder(off), default(0))]
    scenario_capacity: usize,

    #[fieldx(lock, set("_set_pending_orders"), get(copy), builder(off), default(0))]
    pending_orders: usize,

    #[fieldx(lock, set("_set_backorders"), get(copy), builder(off), default(0))]
    backorders: usize,

    #[fieldx(lazy, get_mut, private, builder(off))]
    term: Term,

    #[fieldx(lazy, get(copy), builder(off))]
    user_attended: bool,

    #[fieldx(get(copy), default(Duration::from_millis(100)))]
    refresh_interval: Duration,

    #[fieldx(inner_mut, set, get(copy), builder(off), default(Instant::now()))]
    last_refresh: Instant,

    #[fieldx(inner_mut, get, get_mut, builder(off), default(AtomicI32::new(0)))]
    refresher_count: AtomicI32,

    #[fieldx(inner_mut, get, get_mut, builder(off), default(AtomicI32::new(0)))]
    task_changes: AtomicI32,

    #[fieldx(lock, set, get, get_mut, builder(off), default(Vec::new()))]
    messages: Vec<String>,
}

impl FormattedReporter {
    fn build_term(&self) -> Term {
        Term::buffered_stdout()
    }

    fn build_user_attended(&self) -> bool {
        console::user_attended()
    }

    fn no_scenario(&self) -> SimErrorAny {
        simerr!("Scenario object is gone!")
    }

    fn status_report(&self) -> Result<Vec<String>> {
        let scenario = self.parent()?;
        let mut status_lines = Vec::new();
        status_lines.push(format!("Day            {:>11}", scenario.current_day()));
        status_lines.push(format!("Customers      {:>11}", format_num(scenario.customers().len())));
        status_lines.push(format!(
            "Pending orders {:>11} | Backordered {:>11}",
            format_num(self.pending_orders()),
            format_num(self.backorders())
        ));
        status_lines.push(format!(
            "Scenario lines {:>11} | Capacity {:>11} ({}%)",
            format_num(self.scenario_lines()),
            format_num(self.scenario_capacity()),
            (self.scenario_lines() as f64 / self.scenario_capacity() as f64 * 100.0).round()
        ));
        Ok(status_lines)
    }

    fn spurt_messages(&self, term: &Term, header_height: usize) -> Result<()> {
        let messages = self.set_messages(Vec::new());
        let msg_count = messages.len();
        if self.user_attended() {
            let (_, height) = term.size();
            let height = height as usize - header_height;
            let first_msg = msg_count.max(height) - height;
            for msg in &messages[first_msg..] {
                term.clear_line()?;
                term.write_line(&msg.to_string())?;
            }
        } else {
            for msg in messages {
                println!("{msg}");
            }
        }
        Ok(())
    }

    fn refresher(&self) -> Result<()> {
        let interval = self.refresh_interval();
        let mut next_interval = interval;
        loop {
            if self.shutdown() {
                break;
            }
            self.refresher_count_mut()
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            thread::sleep(next_interval);
            let duration = self.last_refresh().elapsed();
            if duration >= interval {
                self.refresh_report()?;
                next_interval = interval;
            } else {
                // If a refresh took place while we slept then sleep the remaining time to make it a full interval.
                next_interval = interval - duration;
            }
        }

        Ok(())
    }

    fn refresh_task_status(&self) {
        let statuses = self.task_status();
        let task_box_busy = style("■").on_black();
        let task_box_idle = style("□").on_black();
        let r_idx = self.refresher_count().load(std::sync::atomic::Ordering::Relaxed) as usize % TICKER_CHARS.len();
        let task_rotator = style(format!("{} ", TICKER_CHARS[r_idx])).cyan().dim();
        let task_changes = format_num(self.task_changes().load(std::sync::atomic::Ordering::Relaxed));
        let rnd_pool_status = match self.rnd_pool_task() {
            TaskStatus::Idle => task_box_idle.clone().white().dim(),
            TaskStatus::Busy => task_box_busy.clone().magenta().bright(),
        };
        self.set_task_line(format!(
            "{}Workers [{}{}] State transitions: {}",
            task_rotator,
            rnd_pool_status,
            statuses
                .iter()
                .map(|s| {
                    let tb = match s {
                        TaskStatus::Idle => task_box_idle.clone().cyan().dim(),
                        TaskStatus::Busy => task_box_busy.clone().yellow(),
                    };
                    tb.to_string()
                })
                .collect::<Vec<_>>()
                .join(""),
            task_changes
        ));
    }
}

impl SwReporter for FormattedReporter {
    fn start(&self) -> Result<()> {
        self.term().clear_screen().unwrap();
        self.term().hide_cursor().unwrap();
        self.set_shutdown(false);
        if self.user_attended() {
            let myself = self
                .myself()
                .ok_or(simerr!("Failed to get myself for reporter object"))?;
            self.set_refresher_handler(
                thread::Builder::new()
                    .name("refresher".to_string())
                    .spawn(move || myself.refresher())?,
            );
        }

        Ok(())
    }

    fn stop(&self) -> Result<()> {
        self.set_shutdown(true);
        if let Some(handle) = self.clear_refresher_handler() {
            if let Err(err) = handle.join() {
                eprintln!("Screen updater thread failed: {err:?}");
            }
        }
        self.term().show_cursor().unwrap();
        Ok(())
    }

    fn out(&self, msg: &str) -> Result<()> {
        self.messages_mut().push(msg.to_string());
        if !self.user_attended() && self.messages().len() > 20 {
            self.spurt_messages(&self.term(), 0)?;
        }
        Ok(())
    }

    fn set_task_count(&self, count: usize) {
        self.task_status_mut().resize(count, TaskStatus::Idle);
    }

    fn set_task_status(&self, task_id: usize, status: TaskStatus) {
        self.task_changes_mut()
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if task_id < self.task_status().len() {
            self.task_status_mut()[task_id] = status;
        }
    }

    fn set_rnd_pool_task_status(&self, status: TaskStatus) {
        *self.rnd_pool_task_mut() = status;
    }

    fn refresh_report(&self) -> Result<()> {
        let mut status_lines = self.status_report()?;
        let (t_height, t_width) = self.term().size();
        status_lines.push(style("─".repeat(t_width as usize)).cyan().dim().to_string());

        let header_height = status_lines.len();
        let term = self.term_mut();
        if self.user_attended() {
            term.move_cursor_to(0, t_height as usize - 1)?;
        }

        self.refresh_task_status();
        self.spurt_messages(&term, header_height)?;

        if self.user_attended() {
            term.move_cursor_to(0, header_height)?;
            term.clear_last_lines(header_height)?;
            term.write_line(&self.task_line())?;
            for line in status_lines {
                term.clear_line()?;
                term.write_line(&line)?;
            }
            term.move_cursor_to(0, t_height as usize - 1)?;
            term.flush()?;
        } else {
            for line in status_lines {
                println!("{line}");
            }
        }

        self.set_last_refresh(Instant::now());
        Ok(())
    }

    fn set_scenario_lines(&self, lines: usize) {
        self._set_scenario_lines(lines);
    }

    fn set_scenario_capacity(&self, capacity: usize) {
        self._set_scenario_capacity(capacity);
    }

    fn set_backorders(&self, backorders: usize) {
        self._set_backorders(backorders);
    }

    fn set_pending_orders(&self, orders: usize) {
        self._set_pending_orders(orders);
    }
}

fn format_num<N: ToFormattedString>(num: N) -> String {
    NUM_LOCALE.with_borrow(|l| num.to_formatted_string(l))
}
