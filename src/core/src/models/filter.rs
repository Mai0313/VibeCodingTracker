//! Session time-window filtering types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Time window applied when aggregating sessions.
///
/// Serializes as a lowercase string (`"daily"`, `"weekly"`, `"monthly"`, or `"all"`), defaulting to [`TimeRange::All`].
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
    /// Returns the inclusive cutoff date in the system local timezone, or `None` for [`TimeRange::All`].
    ///
    /// [`TimeRange::Weekly`] anchors on the most recent Monday and [`TimeRange::Monthly`] anchors on the first day of the current month.
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

/// Returns the [`TimeRange`] selected by the period flags, falling back to `default` when all flags are false.
///
/// If multiple flags are true, precedence is `daily`, then `weekly`, then `monthly`, then `all`.
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
