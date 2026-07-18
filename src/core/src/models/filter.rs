//! Cross-cutting session filter types shared by the whole crate.
//!
//! [`TimeRange`] is a core domain concept — the aggregators, scanners, and
//! provider readers all filter sessions by it — so it lives here in `models`
//! rather than in the CLI layer. The clap surface re-exports it, and
//! `cli::resolve_time_range*` collapses the period flags into one.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Time window applied when aggregating sessions.
///
/// Each variant maps to a cutoff date via [`TimeRange::cutoff_date`];
/// [`TimeRange::All`] is the default and disables filtering. Serializes as a
/// lowercase string (`"daily"` / `"weekly"` / `"monthly"` / `"all"`) so it can
/// be reused verbatim as the `config.general.default_time_range` setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TimeRange {
    /// Today only.
    Daily,
    /// The current week, starting Monday.
    Weekly,
    /// The current calendar month, from the 1st.
    Monthly,
    /// No filtering — every session.
    #[default]
    All,
}

impl TimeRange {
    /// Returns the inclusive cutoff date for this range, or `None` for
    /// [`TimeRange::All`].
    ///
    /// The cutoff is computed against today in the system local timezone:
    /// `Weekly` anchors on the most recent Monday and `Monthly` on the first
    /// of the month. Sessions on or after the returned date are kept.
    ///
    /// # Panics
    ///
    /// Does not panic: the `with_day(1)` used for `Monthly` is always valid
    /// because day 1 exists in every month.
    pub fn cutoff_date(&self) -> Option<chrono::NaiveDate> {
        use chrono::{Datelike, Local};
        let today = Local::now().date_naive();
        match self {
            TimeRange::All => None,
            TimeRange::Daily => Some(today),
            TimeRange::Weekly => {
                let days_since_monday = today.weekday().num_days_from_monday() as i64;
                Some(today - chrono::Duration::days(days_since_monday))
            }
            TimeRange::Monthly => Some(today.with_day(1).unwrap()),
        }
    }
}

/// Collapses the period flags into a single [`TimeRange`].
///
/// Checks `daily`, then `weekly`, then `monthly`, returning the first that is
/// set; falls back to [`TimeRange::All`] when none are. The flags are mutually
/// exclusive at the CLI layer (shared `period` group), so at most one is ever
/// true here.
pub fn resolve_time_range(daily: bool, weekly: bool, monthly: bool) -> TimeRange {
    resolve_time_range_with_default(daily, weekly, monthly, false, TimeRange::All)
}

/// Collapses the period flags into a [`TimeRange`], falling back to `default`
/// when the caller passed none of them.
///
/// An explicit flag always wins (`--all` maps to [`TimeRange::All`]); only when
/// every flag is unset does `default` (from `config.general.default_time_range`)
/// apply. The flags share the CLI's `period` group, so at most one is ever true.
pub fn resolve_time_range_with_default(
    daily: bool,
    weekly: bool,
    monthly: bool,
    all: bool,
    default: TimeRange,
) -> TimeRange {
    if daily {
        TimeRange::Daily
    } else if weekly {
        TimeRange::Weekly
    } else if monthly {
        TimeRange::Monthly
    } else if all {
        TimeRange::All
    } else {
        default
    }
}
