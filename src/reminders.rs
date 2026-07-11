//! Smart reminders evaluated against the existing DB tables.
//!
//! A reminder watches one of: a custom metric, a row in `sessions`, a row in
//! `lift_sets`, or a built-in `days` column. `evaluate` returns the most
//! recent date the watched thing was logged (if any), and whether that
//! gap is ≥ `interval_days` calendar days ago — in which case the
//! reminder is "due".
//!
//! Sibling to `goals.rs`. No new DB schema, no daemon-side state.

use chrono::{NaiveDate, NaiveTime};
use color_eyre::eyre::Result;
use rusqlite::Connection;

use crate::config::Config;

/// A reminder definition, fully resolved from TOML config.
#[derive(Debug, Clone, PartialEq)]
pub struct Reminder {
    pub id: String,
    pub display: String,
    pub interval_days: u32,
    pub watch: WatchSource,
    pub not_before: Option<NaiveTime>,
    pub not_after: Option<NaiveTime>,
    pub show_streak: bool,
    pub show_days_past_due: bool,
}

/// What the reminder watches. Each variant maps to one prepared query in
/// `evaluate`.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchSource {
    /// Any row in `metrics` matching `name = id`. By default a value of 0
    /// does NOT count as "logged"; opt in via `count_zero_as_logged`.
    Metric {
        id: String,
        count_zero_as_logged: bool,
    },
    /// Any row in `sessions` matching the predicate.
    Session(SessionMatch),
    /// Any row in `lift_sets` with the named exercise, optionally filtered
    /// by `min_weight` (lbs, matching the column) and `min_reps`.
    Lift {
        exercise: String,
        min_weight: Option<f64>,
        min_reps: Option<u32>,
    },
    /// Any row in `days` with the named column non-null.
    DayField(DayColumn),
}

/// `sessions`-table predicates. Closed enum; column whitelist is enforced
/// by these variants.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionMatch {
    TextEquals {
        column: SessionTextColumn,
        value: String,
    },
    NumericAtLeast {
        column: SessionNumColumn,
        min: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTextColumn {
    /// SQL: `session_type`. YAML/config name: `type`.
    Type,
    Block,
    Vo2Intervals,
}

impl SessionTextColumn {
    /// SQL column name (not the YAML/config alias).
    pub fn sql_column(self) -> &'static str {
        match self {
            SessionTextColumn::Type => "session_type",
            SessionTextColumn::Block => "block",
            SessionTextColumn::Vo2Intervals => "vo2_intervals",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionNumColumn {
    Duration,
    Rpe,
    ZoneTwoMin,
    HrAvg,
    Week,
}

impl SessionNumColumn {
    pub fn sql_column(self) -> &'static str {
        match self {
            SessionNumColumn::Duration => "duration",
            SessionNumColumn::Rpe => "rpe",
            SessionNumColumn::ZoneTwoMin => "zone2_min",
            SessionNumColumn::HrAvg => "hr_avg",
            SessionNumColumn::Week => "week",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayColumn {
    Weight,
    SleepHours,
    Mood,
    Energy,
    SleepStart,
    SleepEnd,
}

impl DayColumn {
    pub fn sql_column(self) -> &'static str {
        match self {
            DayColumn::Weight => "weight",
            DayColumn::SleepHours => "sleep_hours",
            DayColumn::Mood => "mood",
            DayColumn::Energy => "energy",
            DayColumn::SleepStart => "sleep_start",
            DayColumn::SleepEnd => "sleep_end",
        }
    }
}

/// One reminder, evaluated against the DB.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedReminder {
    pub id: String,
    pub display: String,
    pub interval_days: u32,
    pub last_done: Option<NaiveDate>,
    pub days_since: Option<i64>,
    pub due: bool,
    pub not_before: Option<NaiveTime>,
    pub not_after: Option<NaiveTime>,
}

/// Output of `evaluate`: evaluated reminders plus any soft warnings (e.g.
/// a metric target that's not declared in `[metrics]`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvaluationResult {
    pub reminders: Vec<EvaluatedReminder>,
    pub warnings: Vec<String>,
}

/// Parse the `[reminders]` section of the config into `Reminder` values.
/// Fails fast on structural errors (missing fields, unknown enum variants,
/// invalid `interval_days`); soft issues (unknown metric target) are
/// surfaced by `evaluate` at runtime.
pub fn load_reminders(config: &Config) -> Result<Vec<Reminder>> {
    use color_eyre::Help;

    let mut out: Vec<Reminder> = Vec::with_capacity(config.reminders.len());
    for (id, cfg) in &config.reminders {
        if cfg.interval_days < 1 {
            return Err(color_eyre::eyre::eyre!(
                "reminder `{id}`: interval_days must be ≥ 1 (got {})",
                cfg.interval_days
            ))
            .suggestion("Set interval_days to a positive integer in config.toml.");
        }
        let watch = match cfg.watch.as_str() {
            "metric" => parse_metric_watch(id, &cfg.target, cfg.count_zero_as_logged)?,
            "session" => parse_session_watch(id, &cfg.target)?,
            "lift" => parse_lift_watch(id, &cfg.target)?,
            "day_field" => parse_day_field_watch(id, &cfg.target)?,
            other => {
                return Err(color_eyre::eyre::eyre!(
                    "reminder `{id}`: unknown watch kind `{other}`"
                ))
                .suggestion("watch must be one of: metric, session, lift, day_field.");
            }
        };
        let not_before = cfg
            .not_before
            .as_deref()
            .map(|s| parse_time_field(id, "not_before", s))
            .transpose()?;
        let not_after = cfg
            .not_after
            .as_deref()
            .map(|s| parse_time_field(id, "not_after", s))
            .transpose()?;
        if let (Some(nb), Some(na)) = (not_before, not_after) {
            let nb_off = offset_minutes(nb, config.day_start_hour);
            let na_off = offset_minutes(na, config.day_start_hour);
            if nb_off > na_off {
                return Err(color_eyre::eyre::eyre!(
                    "reminder `{id}`: not_after must not be earlier than not_before within the effective day (with day_start_hour = {})",
                    config.day_start_hour
                ))
                .suggestion(
                    "For overnight reminders, split into two reminders on the same metric — one with not_after, the other with not_before.",
                );
            }
        }
        let show_streak = cfg
            .show_streak
            .unwrap_or(config.reminder_defaults.show_streak);
        let show_days_past_due = cfg
            .show_days_past_due
            .unwrap_or(config.reminder_defaults.show_days_past_due);
        out.push(Reminder {
            id: id.clone(),
            display: cfg.display.clone(),
            interval_days: cfg.interval_days,
            watch,
            not_before,
            not_after,
            show_streak,
            show_days_past_due,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn parse_metric_watch(
    id: &str,
    target: &toml::Value,
    count_zero_as_logged: bool,
) -> Result<WatchSource> {
    use color_eyre::Help;
    let metric_id = target
        .as_str()
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "reminder `{id}`: watch = \"metric\" requires `target` to be a metric id string"
            )
        })
        .suggestion(r#"Example: target = "la_min""#)?;
    Ok(WatchSource::Metric {
        id: metric_id.to_string(),
        count_zero_as_logged,
    })
}

fn parse_session_watch(id: &str, target: &toml::Value) -> Result<WatchSource> {
    use color_eyre::Help;
    let table = target
        .as_table()
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "reminder `{id}`: watch = \"session\" requires `target` to be an inline table"
            )
        })
        .suggestion(r#"Example: target = { field = "zone2_min", min_value = 1 }"#)?;
    let field = table
        .get("field")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("reminder `{id}`: session target is missing `field`")
        })
        .suggestion(r#"Example: target = { field = "zone2_min", min_value = 1 }"#)?;

    let text_col = parse_session_text_column(field);
    let num_col = parse_session_numeric_column(field);
    let has_equals = table.contains_key("equals");
    let has_min_value = table.contains_key("min_value");

    match (text_col, num_col, has_equals, has_min_value) {
        (Some(col), _, true, false) => {
            let value = table
                .get("equals")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "reminder `{id}`: session target.equals must be a string"
                    )
                })?;
            Ok(WatchSource::Session(SessionMatch::TextEquals {
                column: col,
                value: value.to_string(),
            }))
        }
        (_, Some(col), false, true) => {
            let min = table
                .get("min_value")
                .and_then(toml_value_as_f64)
                .ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "reminder `{id}`: session target.min_value must be a number"
                    )
                })?;
            Ok(WatchSource::Session(SessionMatch::NumericAtLeast {
                column: col,
                min,
            }))
        }
        (Some(_), None, false, true) => Err(color_eyre::eyre::eyre!(
            "reminder `{id}`: session field `{field}` is a text column; use `equals = \"...\"`"
        ))
        .suggestion("Text-column predicates use `equals`; numeric columns use `min_value`."),
        (None, Some(_), true, false) => Err(color_eyre::eyre::eyre!(
            "reminder `{id}`: session field `{field}` is numeric; use `min_value = N`"
        ))
        .suggestion("Text-column predicates use `equals`; numeric columns use `min_value`."),
        (None, None, _, _) => Err(color_eyre::eyre::eyre!(
            "reminder `{id}`: session field `{field}` is not a recognized sessions column"
        ))
        .suggestion(
            "Allowed: type, block, vo2_intervals (text); duration, rpe, zone2_min, hr_avg, week (numeric).",
        ),
        _ => Err(color_eyre::eyre::eyre!(
            "reminder `{id}`: session target must have exactly one of `equals` or `min_value`"
        ))
        .suggestion(
            "Pick one: `equals = \"...\"` for text columns or `min_value = N` for numeric columns.",
        ),
    }
}

fn parse_session_text_column(name: &str) -> Option<SessionTextColumn> {
    match name {
        "type" => Some(SessionTextColumn::Type),
        "block" => Some(SessionTextColumn::Block),
        "vo2_intervals" => Some(SessionTextColumn::Vo2Intervals),
        _ => None,
    }
}

fn parse_session_numeric_column(name: &str) -> Option<SessionNumColumn> {
    match name {
        "duration" => Some(SessionNumColumn::Duration),
        "rpe" => Some(SessionNumColumn::Rpe),
        "zone2_min" => Some(SessionNumColumn::ZoneTwoMin),
        "hr_avg" => Some(SessionNumColumn::HrAvg),
        "week" => Some(SessionNumColumn::Week),
        _ => None,
    }
}

fn parse_lift_watch(id: &str, target: &toml::Value) -> Result<WatchSource> {
    use color_eyre::Help;
    let table = target
        .as_table()
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "reminder `{id}`: watch = \"lift\" requires `target` to be an inline table"
            )
        })
        .suggestion(r#"Example: target = { exercise = "deadlift", min_weight = 200 }"#)?;
    let exercise = table
        .get("exercise")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("reminder `{id}`: lift target is missing `exercise`")
        })
        .suggestion(r#"Example: target = { exercise = "deadlift" }"#)?;
    let min_weight = match table.get("min_weight") {
        None => None,
        Some(v) => Some(toml_value_as_f64(v).ok_or_else(|| {
            color_eyre::eyre::eyre!("reminder `{id}`: lift target.min_weight must be a number")
        })?),
    };
    let min_reps = match table.get("min_reps") {
        None => None,
        Some(v) => {
            let n = v.as_integer().ok_or_else(|| {
                color_eyre::eyre::eyre!("reminder `{id}`: lift target.min_reps must be an integer")
            })?;
            if n < 0 {
                return Err(color_eyre::eyre::eyre!(
                    "reminder `{id}`: lift target.min_reps must be ≥ 0 (got {n})"
                ));
            }
            Some(n as u32)
        }
    };
    Ok(WatchSource::Lift {
        exercise: exercise.to_string(),
        min_weight,
        min_reps,
    })
}

fn parse_day_field_watch(id: &str, target: &toml::Value) -> Result<WatchSource> {
    use color_eyre::Help;
    let name = target
        .as_str()
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "reminder `{id}`: watch = \"day_field\" requires `target` to be a string"
            )
        })
        .suggestion(r#"Example: target = "weight""#)?;
    let col = match name {
        "weight" => DayColumn::Weight,
        "sleep_hours" => DayColumn::SleepHours,
        "mood" => DayColumn::Mood,
        "energy" => DayColumn::Energy,
        "sleep_start" => DayColumn::SleepStart,
        "sleep_end" => DayColumn::SleepEnd,
        _ => {
            return Err(color_eyre::eyre::eyre!(
                "reminder `{id}`: day_field target `{name}` is not a recognized days column"
            ))
            .suggestion("Allowed: weight, sleep_hours, mood, energy, sleep_start, sleep_end.");
        }
    };
    Ok(WatchSource::DayField(col))
}

fn toml_value_as_f64(v: &toml::Value) -> Option<f64> {
    match v {
        toml::Value::Float(f) => Some(*f),
        toml::Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

/// Parse an `HH:MM` time-of-day string. Returns a clear eyre error
/// (with `.suggestion()`) on failure, mentioning the reminder id and
/// the field name.
fn parse_time_field(id: &str, label: &str, s: &str) -> Result<NaiveTime> {
    use color_eyre::Help;
    NaiveTime::parse_from_str(s, "%H:%M")
        .map_err(|_| color_eyre::eyre::eyre!("reminder `{id}`: invalid {label} `{s}`"))
        .suggestion("Use HH:MM in 24-hour form, e.g., \"18:00\".")
}

/// Minutes elapsed since the effective-day start. With `day_start_hour = 0`
/// this is just `t.hour() * 60 + t.minute()`; with non-zero `day_start_hour`
/// it wraps so a wall-clock time before the day-start gets attributed to
/// the previous effective day.
fn offset_minutes(t: NaiveTime, day_start_hour: u8) -> u32 {
    use chrono::Timelike;
    let t_mins = t.hour() * 60 + t.minute();
    let start_mins = day_start_hour as u32 * 60;
    if t_mins >= start_mins {
        t_mins - start_mins
    } else {
        t_mins + 24 * 60 - start_mins
    }
}

/// Returns true if `now` falls inside the effective-day window defined by
/// `not_before` and `not_after`. Either bound being `None` means "no
/// limit on that side". Uses `offset_minutes` so non-zero `day_start_hour`
/// values correctly attribute wall-clock times to the right effective day.
fn within_time_window(
    now: NaiveTime,
    not_before: Option<NaiveTime>,
    not_after: Option<NaiveTime>,
    day_start_hour: u8,
) -> bool {
    let now_off = offset_minutes(now, day_start_hour);
    let after_lower = not_before.is_none_or(|nb| now_off >= offset_minutes(nb, day_start_hour));
    let before_upper = not_after.is_none_or(|na| now_off <= offset_minutes(na, day_start_hour));
    after_lower && before_upper
}

/// Evaluate reminders against the current DB state, computing `last_done`
/// per watched source and applying the per-reminder time gate. `today` is
/// the effective today date (callers should pass `config.effective_today_date()`);
/// `now` is the current wall-clock time (callers should pass
/// `chrono::Local::now().time()`).
pub fn evaluate(
    conn: &Connection,
    today: NaiveDate,
    now: NaiveTime,
    reminders: &[Reminder],
    config: &Config,
) -> Result<EvaluationResult> {
    let mut out = Vec::with_capacity(reminders.len());
    let mut warnings = Vec::new();
    for r in reminders {
        let last_done = query_last_done(conn, &r.watch)?;
        if let WatchSource::Metric { id, .. } = &r.watch {
            if last_done.is_none() && !config.metrics.contains_key(id) {
                warnings.push(format!(
                    "reminder `{}`: target metric `{id}` is not declared in [metrics]",
                    r.id
                ));
            }
        }
        let days_since = last_done.map(|d| (today - d).num_days());
        let data_overdue = match days_since {
            None => true,
            Some(n) => n >= r.interval_days as i64,
        };
        let in_window = within_time_window(now, r.not_before, r.not_after, config.day_start_hour);
        let due = data_overdue && in_window;
        out.push(EvaluatedReminder {
            id: r.id.clone(),
            display: r.display.clone(),
            interval_days: r.interval_days,
            last_done,
            days_since,
            due,
            not_before: r.not_before,
            not_after: r.not_after,
        });
    }
    Ok(EvaluationResult {
        reminders: out,
        warnings,
    })
}

/// Run one MAX(date) query per watch kind. Column names are taken from
/// the closed enum variants — never substituted from user input — so
/// `format!`-ing them into the SQL is safe.
fn query_last_done(conn: &Connection, watch: &WatchSource) -> Result<Option<NaiveDate>> {
    use color_eyre::eyre::WrapErr;

    let date_str: Option<String> = match watch {
        WatchSource::Metric {
            id,
            count_zero_as_logged,
        } => {
            let zero_flag: i64 = if *count_zero_as_logged { 1 } else { 0 };
            conn.query_row(
                "SELECT MAX(date) FROM metrics WHERE name = ?1 AND (value > 0 OR ?2 = 1)",
                rusqlite::params![id, zero_flag],
                |row| row.get::<_, Option<String>>(0),
            )
            .wrap_err("Failed to query metrics for reminder")?
        }
        WatchSource::Session(SessionMatch::TextEquals { column, value }) => {
            let sql = format!(
                "SELECT MAX(date) FROM sessions WHERE {} = ?1",
                column.sql_column()
            );
            conn.query_row(&sql, [value], |row| row.get::<_, Option<String>>(0))
                .wrap_err("Failed to query sessions for reminder (text-equals)")?
        }
        WatchSource::Session(SessionMatch::NumericAtLeast { column, min }) => {
            let sql = format!(
                "SELECT MAX(date) FROM sessions WHERE {col} IS NOT NULL AND {col} >= ?1",
                col = column.sql_column()
            );
            conn.query_row(&sql, rusqlite::params![min], |row| {
                row.get::<_, Option<String>>(0)
            })
            .wrap_err("Failed to query sessions for reminder (numeric-at-least)")?
        }
        WatchSource::Lift {
            exercise,
            min_weight,
            min_reps,
        } => {
            let mut sql = String::from("SELECT MAX(date) FROM lift_sets WHERE exercise = ?1");
            // We bind params in order; build the SQL conditionally to match.
            let mut params: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::Text(exercise.clone())];
            if let Some(w) = min_weight {
                sql.push_str(&format!(" AND weight_lbs >= ?{}", params.len() + 1));
                params.push(rusqlite::types::Value::Real(*w));
            }
            if let Some(r) = min_reps {
                sql.push_str(&format!(" AND reps >= ?{}", params.len() + 1));
                params.push(rusqlite::types::Value::Integer(*r as i64));
            }
            let params_refs: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
            conn.query_row(&sql, params_refs.as_slice(), |row| {
                row.get::<_, Option<String>>(0)
            })
            .wrap_err("Failed to query lift_sets for reminder")?
        }
        WatchSource::DayField(col) => {
            let sql = format!(
                "SELECT MAX(date) FROM days WHERE {} IS NOT NULL",
                col.sql_column()
            );
            conn.query_row(&sql, [], |row| row.get::<_, Option<String>>(0))
                .wrap_err("Failed to query days for reminder")?
        }
    };
    Ok(date_str.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()))
}

/// Length of the current streak in *days*, per the cadence model in
/// `docs/superpowers/specs/2026-07-11-reminder-streaks-and-days-past-due-design.md`.
///
/// `dates_desc` are the distinct qualifying completion dates, most-recent
/// first. Returns 0 when there is no live streak: empty history, the most
/// recent completion is in the future, or the streak has broken
/// (`today - last_done > interval_days`).
pub(crate) fn compute_streak(
    dates_desc: &[NaiveDate],
    today: NaiveDate,
    interval_days: u32,
) -> u32 {
    let interval = interval_days as i64;
    let last = match dates_desc.first() {
        Some(d) => *d,
        None => return 0,
    };
    let days_since = (today - last).num_days();
    if days_since < 0 || days_since > interval {
        return 0;
    }
    // Walk back through the run: consecutive completions stay in the same
    // chain while the gap between them is ≤ interval.
    let mut run_start = last;
    let mut prev = last;
    for &earlier in &dates_desc[1..] {
        if (prev - earlier).num_days() <= interval {
            run_start = earlier;
            prev = earlier;
        } else {
            break;
        }
    }
    // Each completion credits `interval` days (its own day + interval-1 more),
    // capped at today — the streak never counts into the future. An absurdly
    // large `interval_days` can push the credit date past `NaiveDate`'s
    // representable range; fall back to `today` rather than panicking.
    let credited_end = last
        .checked_add_signed(chrono::Duration::days(interval - 1))
        .map_or(today, |d| std::cmp::min(today, d));
    ((credited_end - run_start).num_days() + 1) as u32
}

/// Build the JSON values for the `reminders` and `reminder_warnings` keys
/// emitted by `vitalog today --json` and `vitalog status`. Lifted here so
/// both surfaces agree on the per-reminder schema as fields evolve.
pub fn to_json(
    reminders: &[EvaluatedReminder],
    warnings: &[String],
) -> (serde_json::Value, serde_json::Value) {
    let rs: Vec<serde_json::Value> = reminders
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "display": r.display,
                "interval_days": r.interval_days,
                "last_done": r.last_done.map(|d| d.format("%Y-%m-%d").to_string()),
                "days_since": r.days_since,
                "due": r.due,
                "not_before": r.not_before.map(|t| t.format("%H:%M").to_string()),
                "not_after": r.not_after.map(|t| t.format("%H:%M").to_string()),
            })
        })
        .collect();
    let warns: Vec<serde_json::Value> = warnings
        .iter()
        .cloned()
        .map(serde_json::Value::String)
        .collect();
    (
        serde_json::Value::Array(rs),
        serde_json::Value::Array(warns),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_with(reminders_toml: &str) -> Config {
        let toml_str = format!(
            r#"
notes_dir = "/tmp/x"
time_format = "24h"

[metrics]
la_min = {{ display = "Lactic acid (min)", color = "red" }}

[exercises]
deadlift = {{ display = "Deadlift", color = "yellow" }}

{reminders_toml}
"#
        );
        toml::from_str(&toml_str).unwrap()
    }

    #[test]
    fn load_parses_metric_watch() {
        let config = config_with(
            r#"
[reminders.lactic_acid]
display = "Lactic acid training"
interval_days = 2
watch = "metric"
target = "la_min"
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].id, "lactic_acid");
        assert_eq!(rs[0].display, "Lactic acid training");
        assert_eq!(rs[0].interval_days, 2);
        assert_eq!(
            rs[0].watch,
            WatchSource::Metric {
                id: "la_min".into(),
                count_zero_as_logged: false,
            }
        );
    }

    #[test]
    fn load_parses_metric_watch_with_count_zero() {
        let config = config_with(
            r#"
[reminders.brushed]
display = "Brushed teeth"
interval_days = 1
watch = "metric"
target = "brushed_morning"
count_zero_as_logged = true
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(
            rs[0].watch,
            WatchSource::Metric {
                id: "brushed_morning".into(),
                count_zero_as_logged: true,
            }
        );
    }

    #[test]
    fn load_parses_session_text_equals() {
        let config = config_with(
            r#"
[reminders.vo2]
display = "VO2max"
interval_days = 7
watch = "session"
target = { field = "type", equals = "vo2_max" }
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(
            rs[0].watch,
            WatchSource::Session(SessionMatch::TextEquals {
                column: SessionTextColumn::Type,
                value: "vo2_max".into(),
            })
        );
    }

    #[test]
    fn load_parses_session_numeric_at_least() {
        let config = config_with(
            r#"
[reminders.zone2]
display = "Zone 2"
interval_days = 3
watch = "session"
target = { field = "zone2_min", min_value = 1 }
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(
            rs[0].watch,
            WatchSource::Session(SessionMatch::NumericAtLeast {
                column: SessionNumColumn::ZoneTwoMin,
                min: 1.0,
            })
        );
    }

    #[test]
    fn load_parses_lift_with_filters() {
        let config = config_with(
            r#"
[reminders.deads]
display = "Heavy deadlifts"
interval_days = 7
watch = "lift"
target = { exercise = "deadlift", min_weight = 200, min_reps = 3 }
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(
            rs[0].watch,
            WatchSource::Lift {
                exercise: "deadlift".into(),
                min_weight: Some(200.0),
                min_reps: Some(3),
            }
        );
    }

    #[test]
    fn load_parses_lift_without_filters() {
        let config = config_with(
            r#"
[reminders.any_deadlift]
display = "Any deadlift"
interval_days = 7
watch = "lift"
target = { exercise = "deadlift" }
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(
            rs[0].watch,
            WatchSource::Lift {
                exercise: "deadlift".into(),
                min_weight: None,
                min_reps: None,
            }
        );
    }

    #[test]
    fn load_parses_day_field() {
        let config = config_with(
            r#"
[reminders.weigh_in]
display = "Daily weigh-in"
interval_days = 1
watch = "day_field"
target = "weight"
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(rs[0].watch, WatchSource::DayField(DayColumn::Weight));
    }

    #[test]
    fn load_rejects_interval_zero() {
        let config = config_with(
            r#"
[reminders.bad]
display = "Bad"
interval_days = 0
watch = "metric"
target = "la_min"
"#,
        );
        let err = load_reminders(&config).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("interval_days"), "got: {msg}");
        assert!(msg.contains("bad"), "got: {msg}");
    }

    #[test]
    fn load_rejects_unknown_watch_kind() {
        let config = config_with(
            r#"
[reminders.bad]
display = "Bad"
interval_days = 1
watch = "constellation"
target = "la_min"
"#,
        );
        let err = load_reminders(&config).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("constellation"), "got: {msg}");
    }

    #[test]
    fn load_rejects_unknown_session_field() {
        let config = config_with(
            r#"
[reminders.bad]
display = "Bad"
interval_days = 1
watch = "session"
target = { field = "nonsense", equals = "x" }
"#,
        );
        let err = load_reminders(&config).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("nonsense"), "got: {msg}");
    }

    #[test]
    fn load_rejects_session_text_op_on_numeric_column() {
        let config = config_with(
            r#"
[reminders.bad]
display = "Bad"
interval_days = 1
watch = "session"
target = { field = "duration", equals = "60" }
"#,
        );
        let err = load_reminders(&config).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("duration"), "got: {msg}");
        assert!(
            msg.contains("min_value") || msg.contains("numeric"),
            "got: {msg}"
        );
    }

    #[test]
    fn load_rejects_unknown_day_field() {
        let config = config_with(
            r#"
[reminders.bad]
display = "Bad"
interval_days = 1
watch = "day_field"
target = "wakefulness"
"#,
        );
        let err = load_reminders(&config).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("wakefulness"), "got: {msg}");
    }

    #[test]
    fn load_rejects_lift_without_exercise() {
        let config = config_with(
            r#"
[reminders.bad]
display = "Bad"
interval_days = 1
watch = "lift"
target = { min_weight = 200 }
"#,
        );
        let err = load_reminders(&config).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("exercise"), "got: {msg}");
    }

    #[test]
    fn load_returns_reminders_in_id_alphabetical_order() {
        let config = config_with(
            r#"
[reminders.zzz_last]
display = "Last"
interval_days = 1
watch = "metric"
target = "la_min"

[reminders.aaa_first]
display = "First"
interval_days = 1
watch = "metric"
target = "la_min"
"#,
        );
        let rs = load_reminders(&config).unwrap();
        let ids: Vec<&str> = rs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["aaa_first", "zzz_last"]);
    }

    #[test]
    fn load_empty_when_no_reminders() {
        let mut cfg: Config = toml::from_str(r#"notes_dir = "/tmp/x""#).unwrap();
        cfg.reminders = HashMap::new();
        assert!(load_reminders(&cfg).unwrap().is_empty());
    }

    #[test]
    fn load_resolves_streak_toggles_from_defaults_and_overrides() {
        let cfg: Config = toml::from_str(
            r#"
notes_dir = "/tmp/x"

[metrics]
la_min = { display = "LA", color = "red" }

[reminder_defaults]
show_streak = false
show_days_past_due = true

[reminders.uses_default]
display = "Uses default"
interval_days = 1
watch = "metric"
target = "la_min"

[reminders.overrides]
display = "Overrides"
interval_days = 1
watch = "metric"
target = "la_min"
show_streak = true
show_days_past_due = false
"#,
        )
        .unwrap();
        let rs = load_reminders(&cfg).unwrap();
        // load_reminders sorts by id: "overrides" then "uses_default".
        let overrides = rs.iter().find(|r| r.id == "overrides").unwrap();
        let uses_default = rs.iter().find(|r| r.id == "uses_default").unwrap();
        assert!(overrides.show_streak);
        assert!(!overrides.show_days_past_due);
        assert!(!uses_default.show_streak);
        assert!(uses_default.show_days_past_due);
    }

    use rusqlite::Connection;

    /// Minimal in-memory DB with the tables `evaluate` reads.
    /// We mirror the production schema shape but skip foreign keys to keep
    /// the test setup compact — these tests only exercise the reminder
    /// queries, not referential integrity.
    fn make_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE days (
                date TEXT PRIMARY KEY,
                sleep_start TEXT,
                sleep_end TEXT,
                sleep_hours REAL,
                mood INTEGER,
                energy INTEGER,
                weight REAL
            );
            CREATE TABLE metrics (
                date TEXT NOT NULL,
                name TEXT NOT NULL,
                value REAL NOT NULL,
                PRIMARY KEY (date, name)
            );
            CREATE TABLE sessions (
                date TEXT NOT NULL,
                session_number INTEGER NOT NULL DEFAULT 1,
                session_type TEXT,
                week INTEGER,
                block TEXT,
                duration INTEGER,
                rpe REAL,
                zone2_min INTEGER,
                hr_avg INTEGER,
                vo2_intervals TEXT,
                PRIMARY KEY (date, session_number)
            );
            CREATE TABLE lift_sets (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                date TEXT NOT NULL,
                session_number INTEGER NOT NULL DEFAULT 1,
                exercise TEXT NOT NULL,
                set_number INTEGER NOT NULL,
                weight_lbs REAL NOT NULL,
                reps INTEGER NOT NULL,
                estimated_1rm REAL
            );
            ",
        )
        .unwrap();
        conn
    }

    fn metric_reminder(id: &str, interval_days: u32, metric: &str) -> Reminder {
        Reminder {
            id: id.into(),
            display: id.into(),
            interval_days,
            watch: WatchSource::Metric {
                id: metric.into(),
                count_zero_as_logged: false,
            },
            not_before: None,
            not_after: None,
            show_streak: false,
            show_days_past_due: false,
        }
    }

    fn insert_day(conn: &Connection, date: &str) {
        conn.execute("INSERT OR IGNORE INTO days(date) VALUES (?1)", [date])
            .unwrap();
    }

    fn insert_metric(conn: &Connection, date: &str, name: &str, value: f64) {
        insert_day(conn, date);
        conn.execute(
            "INSERT INTO metrics(date, name, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![date, name, value],
        )
        .unwrap();
    }

    fn empty_config() -> Config {
        toml::from_str(
            r#"
notes_dir = "/tmp/x"

[metrics]
la_min = { display = "LA", color = "red" }
"#,
        )
        .unwrap()
    }

    #[test]
    fn evaluate_metric_never_logged_is_due() {
        let conn = make_test_db();
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let result = evaluate(
            &conn,
            today,
            noon(),
            &[metric_reminder("la", 2, "la_min")],
            &empty_config(),
        )
        .unwrap();
        assert_eq!(result.reminders.len(), 1);
        let r = &result.reminders[0];
        assert_eq!(r.last_done, None);
        assert_eq!(r.days_since, None);
        assert!(r.due);
    }

    #[test]
    fn evaluate_metric_logged_today_not_due() {
        let conn = make_test_db();
        insert_metric(&conn, "2026-05-12", "la_min", 15.0);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let result = evaluate(
            &conn,
            today,
            noon(),
            &[metric_reminder("la", 2, "la_min")],
            &empty_config(),
        )
        .unwrap();
        let r = &result.reminders[0];
        assert_eq!(r.last_done, Some(today));
        assert_eq!(r.days_since, Some(0));
        assert!(!r.due);
    }

    #[test]
    fn evaluate_metric_logged_exactly_interval_days_ago_is_due() {
        // interval=2, logged Monday at 23:00 (DB only stores date),
        // checking Wednesday → due all day Wednesday.
        let conn = make_test_db();
        insert_metric(&conn, "2026-05-10", "la_min", 15.0);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let result = evaluate(
            &conn,
            today,
            noon(),
            &[metric_reminder("la", 2, "la_min")],
            &empty_config(),
        )
        .unwrap();
        let r = &result.reminders[0];
        assert_eq!(
            r.last_done,
            Some(NaiveDate::from_ymd_opt(2026, 5, 10).unwrap())
        );
        assert_eq!(r.days_since, Some(2));
        assert!(r.due);
    }

    #[test]
    fn evaluate_metric_logged_interval_minus_one_days_ago_not_due() {
        // interval=2, logged yesterday → not due today.
        let conn = make_test_db();
        insert_metric(&conn, "2026-05-11", "la_min", 15.0);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let result = evaluate(
            &conn,
            today,
            noon(),
            &[metric_reminder("la", 2, "la_min")],
            &empty_config(),
        )
        .unwrap();
        let r = &result.reminders[0];
        assert_eq!(r.days_since, Some(1));
        assert!(!r.due);
    }

    #[test]
    fn evaluate_metric_zero_value_does_not_count_by_default() {
        let conn = make_test_db();
        insert_metric(&conn, "2026-05-12", "la_min", 0.0);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let result = evaluate(
            &conn,
            today,
            noon(),
            &[metric_reminder("la", 2, "la_min")],
            &empty_config(),
        )
        .unwrap();
        assert_eq!(result.reminders[0].last_done, None);
        assert!(result.reminders[0].due);
    }

    #[test]
    fn evaluate_metric_zero_value_counts_when_opted_in() {
        let conn = make_test_db();
        insert_metric(&conn, "2026-05-12", "la_min", 0.0);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let reminder = Reminder {
            id: "la".into(),
            display: "LA".into(),
            interval_days: 1,
            watch: WatchSource::Metric {
                id: "la_min".into(),
                count_zero_as_logged: true,
            },
            not_before: None,
            not_after: None,
            show_streak: false,
            show_days_past_due: false,
        };
        let result = evaluate(&conn, today, noon(), &[reminder], &empty_config()).unwrap();
        assert_eq!(result.reminders[0].last_done, Some(today));
        assert!(!result.reminders[0].due);
    }

    fn insert_session(
        conn: &Connection,
        date: &str,
        session_type: Option<&str>,
        zone2_min: Option<i64>,
    ) {
        insert_day(conn, date);
        conn.execute(
            "INSERT INTO sessions(date, session_number, session_type, zone2_min) \
             VALUES (?1, 1, ?2, ?3)",
            rusqlite::params![date, session_type, zone2_min],
        )
        .unwrap();
    }

    fn session_text_reminder(
        id: &str,
        interval: u32,
        column: SessionTextColumn,
        value: &str,
    ) -> Reminder {
        Reminder {
            id: id.into(),
            display: id.into(),
            interval_days: interval,
            watch: WatchSource::Session(SessionMatch::TextEquals {
                column,
                value: value.into(),
            }),
            not_before: None,
            not_after: None,
            show_streak: false,
            show_days_past_due: false,
        }
    }

    fn session_num_reminder(
        id: &str,
        interval: u32,
        column: SessionNumColumn,
        min: f64,
    ) -> Reminder {
        Reminder {
            id: id.into(),
            display: id.into(),
            interval_days: interval,
            watch: WatchSource::Session(SessionMatch::NumericAtLeast { column, min }),
            not_before: None,
            not_after: None,
            show_streak: false,
            show_days_past_due: false,
        }
    }

    #[test]
    fn evaluate_session_text_equals_match() {
        let conn = make_test_db();
        insert_session(&conn, "2026-05-10", Some("vo2_max"), None);
        insert_session(&conn, "2026-05-11", Some("zone2"), None);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = session_text_reminder("vo2", 7, SessionTextColumn::Type, "vo2_max");
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(
            result.reminders[0].last_done,
            Some(NaiveDate::from_ymd_opt(2026, 5, 10).unwrap())
        );
        assert!(!result.reminders[0].due);
    }

    #[test]
    fn evaluate_session_text_equals_no_match_is_due() {
        let conn = make_test_db();
        insert_session(&conn, "2026-05-10", Some("zone2"), None);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = session_text_reminder("vo2", 7, SessionTextColumn::Type, "vo2_max");
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(result.reminders[0].last_done, None);
        assert!(result.reminders[0].due);
    }

    #[test]
    fn evaluate_session_numeric_at_least_match() {
        let conn = make_test_db();
        insert_session(&conn, "2026-05-09", None, Some(0));
        insert_session(&conn, "2026-05-10", None, Some(30));
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = session_num_reminder("zone2", 3, SessionNumColumn::ZoneTwoMin, 1.0);
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(
            result.reminders[0].last_done,
            Some(NaiveDate::from_ymd_opt(2026, 5, 10).unwrap())
        );
        assert!(!result.reminders[0].due);
    }

    #[test]
    fn evaluate_session_numeric_below_threshold_not_counted() {
        let conn = make_test_db();
        insert_session(&conn, "2026-05-11", None, Some(15));
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = session_num_reminder("zone2_long", 1, SessionNumColumn::ZoneTwoMin, 30.0);
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(result.reminders[0].last_done, None);
        assert!(result.reminders[0].due);
    }

    fn insert_lift(conn: &Connection, date: &str, exercise: &str, weight_lbs: f64, reps: i64) {
        insert_day(conn, date);
        conn.execute(
            "INSERT INTO sessions(date, session_number) VALUES (?1, 1) \
             ON CONFLICT(date, session_number) DO NOTHING",
            [date],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lift_sets(date, session_number, exercise, set_number, weight_lbs, reps) \
             VALUES (?1, 1, ?2, 1, ?3, ?4)",
            rusqlite::params![date, exercise, weight_lbs, reps],
        )
        .unwrap();
    }

    fn lift_reminder(
        id: &str,
        interval: u32,
        exercise: &str,
        min_weight: Option<f64>,
        min_reps: Option<u32>,
    ) -> Reminder {
        Reminder {
            id: id.into(),
            display: id.into(),
            interval_days: interval,
            watch: WatchSource::Lift {
                exercise: exercise.into(),
                min_weight,
                min_reps,
            },
            not_before: None,
            not_after: None,
            show_streak: false,
            show_days_past_due: false,
        }
    }

    #[test]
    fn evaluate_lift_exercise_only() {
        let conn = make_test_db();
        insert_lift(&conn, "2026-05-09", "deadlift", 200.0, 5);
        insert_lift(&conn, "2026-05-11", "squat", 185.0, 5);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = lift_reminder("deads", 3, "deadlift", None, None);
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(
            result.reminders[0].last_done,
            Some(NaiveDate::from_ymd_opt(2026, 5, 9).unwrap())
        );
        assert!(result.reminders[0].due);
    }

    #[test]
    fn evaluate_lift_with_min_weight_excludes_lighter_sets() {
        let conn = make_test_db();
        insert_lift(&conn, "2026-05-09", "deadlift", 200.0, 5);
        insert_lift(&conn, "2026-05-11", "deadlift", 135.0, 5);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = lift_reminder("heavy_deads", 7, "deadlift", Some(180.0), None);
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(
            result.reminders[0].last_done,
            Some(NaiveDate::from_ymd_opt(2026, 5, 9).unwrap())
        );
    }

    #[test]
    fn evaluate_lift_with_min_reps_excludes_shorter_sets() {
        let conn = make_test_db();
        insert_lift(&conn, "2026-05-09", "deadlift", 200.0, 5);
        insert_lift(&conn, "2026-05-11", "deadlift", 250.0, 2);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = lift_reminder("deads_for_reps", 7, "deadlift", None, Some(4));
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(
            result.reminders[0].last_done,
            Some(NaiveDate::from_ymd_opt(2026, 5, 9).unwrap())
        );
    }

    #[test]
    fn evaluate_lift_no_match_is_due() {
        let conn = make_test_db();
        insert_lift(&conn, "2026-05-09", "squat", 185.0, 5);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = lift_reminder("deads", 3, "deadlift", None, None);
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(result.reminders[0].last_done, None);
        assert!(result.reminders[0].due);
    }

    fn insert_day_field(conn: &Connection, date: &str, column: &str, value: &str) {
        insert_day(conn, date);
        let sql = format!("UPDATE days SET {column} = ?1 WHERE date = ?2");
        conn.execute(&sql, [value, date]).unwrap();
    }

    fn day_field_reminder(id: &str, interval: u32, col: DayColumn) -> Reminder {
        Reminder {
            id: id.into(),
            display: id.into(),
            interval_days: interval,
            watch: WatchSource::DayField(col),
            not_before: None,
            not_after: None,
            show_streak: false,
            show_days_past_due: false,
        }
    }

    #[test]
    fn evaluate_day_field_weight_match() {
        let conn = make_test_db();
        insert_day_field(&conn, "2026-05-11", "weight", "121.5");
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = day_field_reminder("weigh_in", 1, DayColumn::Weight);
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(
            result.reminders[0].last_done,
            Some(NaiveDate::from_ymd_opt(2026, 5, 11).unwrap())
        );
        assert_eq!(result.reminders[0].days_since, Some(1));
        // interval=1, days_since=1 → due (1 >= 1)
        assert!(result.reminders[0].due);
    }

    #[test]
    fn evaluate_day_field_sleep_hours_match() {
        let conn = make_test_db();
        insert_day_field(&conn, "2026-05-12", "sleep_hours", "7.5");
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = day_field_reminder("sleep_log", 1, DayColumn::SleepHours);
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(result.reminders[0].days_since, Some(0));
        assert!(!result.reminders[0].due);
    }

    #[test]
    fn evaluate_day_field_no_rows_is_due() {
        let conn = make_test_db();
        // A day row exists but `weight` is null.
        insert_day(&conn, "2026-05-12");
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = day_field_reminder("weigh_in", 1, DayColumn::Weight);
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(result.reminders[0].last_done, None);
        assert!(result.reminders[0].due);
    }

    #[test]
    fn evaluate_unknown_metric_target_emits_warning() {
        let conn = make_test_db();
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = metric_reminder("typoed", 1, "definitely_not_in_metrics");
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert!(result.reminders[0].due);
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("definitely_not_in_metrics") && w.contains("typoed")),
            "expected unknown-metric warning, got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn evaluate_unknown_metric_with_history_does_not_warn() {
        // If the metric isn't in [metrics] but the DB has historical rows,
        // suppress the warning — the user is likely watching legacy data.
        let conn = make_test_db();
        insert_metric(&conn, "2026-05-05", "legacy_metric", 1.0);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = metric_reminder("legacy", 1, "legacy_metric");
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert!(result.warnings.is_empty(), "got: {:?}", result.warnings);
    }

    #[test]
    fn load_parses_not_before_and_not_after() {
        let config = config_with(
            r#"
[reminders.brush_evening]
display = "Brush teeth (evening)"
interval_days = 1
watch = "metric"
target = "la_min"
not_before = "18:00"
not_after = "23:00"
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(
            rs[0].not_before,
            Some(NaiveTime::from_hms_opt(18, 0, 0).unwrap())
        );
        assert_eq!(
            rs[0].not_after,
            Some(NaiveTime::from_hms_opt(23, 0, 0).unwrap())
        );
    }

    #[test]
    fn load_parses_not_before_only() {
        let config = config_with(
            r#"
[reminders.la]
display = "LA"
interval_days = 2
watch = "metric"
target = "la_min"
not_before = "10:00"
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(
            rs[0].not_before,
            Some(NaiveTime::from_hms_opt(10, 0, 0).unwrap())
        );
        assert_eq!(rs[0].not_after, None);
    }

    #[test]
    fn load_parses_not_after_only() {
        let config = config_with(
            r#"
[reminders.morning]
display = "Morning"
interval_days = 1
watch = "metric"
target = "la_min"
not_after = "12:00"
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(rs[0].not_before, None);
        assert_eq!(
            rs[0].not_after,
            Some(NaiveTime::from_hms_opt(12, 0, 0).unwrap())
        );
    }

    #[test]
    fn load_no_time_gates_yields_none() {
        let config = config_with(
            r#"
[reminders.la]
display = "LA"
interval_days = 2
watch = "metric"
target = "la_min"
"#,
        );
        let rs = load_reminders(&config).unwrap();
        assert_eq!(rs[0].not_before, None);
        assert_eq!(rs[0].not_after, None);
    }

    #[test]
    fn load_rejects_unparseable_not_before() {
        let config = config_with(
            r#"
[reminders.bad]
display = "Bad"
interval_days = 1
watch = "metric"
target = "la_min"
not_before = "25:00"
"#,
        );
        let err = load_reminders(&config).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not_before"), "got: {msg}");
        assert!(msg.contains("25:00"), "got: {msg}");
        assert!(msg.contains("bad"), "got: {msg}");
    }

    #[test]
    fn load_rejects_unparseable_not_after() {
        let config = config_with(
            r#"
[reminders.bad]
display = "Bad"
interval_days = 1
watch = "metric"
target = "la_min"
not_after = "noon"
"#,
        );
        let err = load_reminders(&config).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not_after"), "got: {msg}");
        assert!(msg.contains("noon"), "got: {msg}");
    }

    #[test]
    fn load_rejects_window_wraparound_default_day_start() {
        // day_start_hour = 0 (default): a window from 22:00 to 06:00 crosses
        // the effective-day boundary → reject.
        let config = config_with(
            r#"
[reminders.bad]
display = "Bad"
interval_days = 1
watch = "metric"
target = "la_min"
not_before = "22:00"
not_after = "06:00"
"#,
        );
        let err = load_reminders(&config).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not_after"), "got: {msg}");
        assert!(msg.contains("not_before"), "got: {msg}");
    }

    #[test]
    fn load_accepts_window_wrapping_wall_clock_with_day_start_hour() {
        // day_start_hour = 4: a window 22:00 → 02:00 spans midnight in
        // wall-clock but stays within the effective day (offsets 1080 → 1320)
        // → accept.
        let toml_str = r#"
notes_dir = "/tmp/x"
day_start_hour = 4

[metrics]
la_min = { display = "LA", color = "red" }

[reminders.evening]
display = "Evening"
interval_days = 1
watch = "metric"
target = "la_min"
not_before = "22:00"
not_after = "02:00"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let rs = load_reminders(&config).unwrap();
        assert_eq!(
            rs[0].not_before,
            Some(NaiveTime::from_hms_opt(22, 0, 0).unwrap())
        );
        assert_eq!(
            rs[0].not_after,
            Some(NaiveTime::from_hms_opt(2, 0, 0).unwrap())
        );
    }

    #[test]
    fn load_rejects_window_crossing_effective_day_with_day_start_hour() {
        // day_start_hour = 4: not_before = 02:00 (offset 1320, late in
        // effective day D), not_after = 06:00 (offset 120, early in
        // effective day D+1) → reject.
        let toml_str = r#"
notes_dir = "/tmp/x"
day_start_hour = 4

[metrics]
la_min = { display = "LA", color = "red" }

[reminders.bad]
display = "Bad"
interval_days = 1
watch = "metric"
target = "la_min"
not_before = "02:00"
not_after = "06:00"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let err = load_reminders(&config).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not_after"), "got: {msg}");
    }

    #[test]
    fn within_time_window_no_gates_always_true() {
        let now = NaiveTime::from_hms_opt(3, 14, 0).unwrap();
        assert!(within_time_window(now, None, None, 0));
    }

    #[test]
    fn within_time_window_lower_only() {
        let nb = NaiveTime::from_hms_opt(18, 0, 0).unwrap();
        let before = NaiveTime::from_hms_opt(17, 59, 0).unwrap();
        let on = NaiveTime::from_hms_opt(18, 0, 0).unwrap();
        let after = NaiveTime::from_hms_opt(18, 1, 0).unwrap();
        assert!(!within_time_window(before, Some(nb), None, 0));
        assert!(within_time_window(on, Some(nb), None, 0));
        assert!(within_time_window(after, Some(nb), None, 0));
    }

    #[test]
    fn within_time_window_upper_only() {
        let na = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        let before = NaiveTime::from_hms_opt(11, 59, 0).unwrap();
        let on = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        let after = NaiveTime::from_hms_opt(12, 1, 0).unwrap();
        assert!(within_time_window(before, None, Some(na), 0));
        assert!(within_time_window(on, None, Some(na), 0));
        assert!(!within_time_window(after, None, Some(na), 0));
    }

    #[test]
    fn within_time_window_both_gates() {
        let nb = NaiveTime::from_hms_opt(18, 0, 0).unwrap();
        let na = NaiveTime::from_hms_opt(23, 0, 0).unwrap();
        assert!(!within_time_window(
            NaiveTime::from_hms_opt(17, 0, 0).unwrap(),
            Some(nb),
            Some(na),
            0,
        ));
        assert!(within_time_window(
            NaiveTime::from_hms_opt(20, 0, 0).unwrap(),
            Some(nb),
            Some(na),
            0,
        ));
        assert!(!within_time_window(
            NaiveTime::from_hms_opt(23, 30, 0).unwrap(),
            Some(nb),
            Some(na),
            0,
        ));
    }

    #[test]
    fn within_time_window_day_start_hour_zero_baseline() {
        // Sanity check: at day_start_hour = 0, offset math reduces to
        // wall-clock comparison.
        let nb = NaiveTime::from_hms_opt(6, 0, 0).unwrap();
        let na = NaiveTime::from_hms_opt(22, 0, 0).unwrap();
        assert!(!within_time_window(
            NaiveTime::from_hms_opt(5, 59, 0).unwrap(),
            Some(nb),
            Some(na),
            0,
        ));
        assert!(within_time_window(
            NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            Some(nb),
            Some(na),
            0,
        ));
        assert!(!within_time_window(
            NaiveTime::from_hms_opt(22, 1, 0).unwrap(),
            Some(nb),
            Some(na),
            0,
        ));
    }

    #[test]
    fn within_time_window_evening_gate_with_day_start_four() {
        // day_start_hour = 4, not_before = "22:00" → offset 1080.
        // Gate is open from 22:00 wall-clock through 03:59 the next
        // wall-day (offset 1439), then closes at 04:00 (offset 0).
        let nb = NaiveTime::from_hms_opt(22, 0, 0).unwrap();
        assert!(!within_time_window(
            NaiveTime::from_hms_opt(21, 59, 0).unwrap(),
            Some(nb),
            None,
            4,
        ));
        assert!(within_time_window(
            NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
            Some(nb),
            None,
            4,
        ));
        // 01:00 wall-clock the next day — still within the effective day
        // that started at 04:00 yesterday.
        assert!(within_time_window(
            NaiveTime::from_hms_opt(1, 0, 0).unwrap(),
            Some(nb),
            None,
            4,
        ));
        // 04:00 wall-clock — new effective day starts, gate closes
        // until the next 22:00.
        assert!(!within_time_window(
            NaiveTime::from_hms_opt(4, 0, 0).unwrap(),
            Some(nb),
            None,
            4,
        ));
    }

    #[test]
    fn within_time_window_early_morning_gate_with_day_start_four() {
        // day_start_hour = 4, not_before = "02:00" → offset 1320 (late
        // in the effective day, near its end). Gate opens at 02:00
        // wall-clock the day after the effective day started.
        let nb = NaiveTime::from_hms_opt(2, 0, 0).unwrap();
        assert!(!within_time_window(
            NaiveTime::from_hms_opt(1, 59, 0).unwrap(),
            Some(nb),
            None,
            4,
        ));
        assert!(within_time_window(
            NaiveTime::from_hms_opt(2, 30, 0).unwrap(),
            Some(nb),
            None,
            4,
        ));
        // 04:00 — effective day rolls, gate closes.
        assert!(!within_time_window(
            NaiveTime::from_hms_opt(4, 0, 0).unwrap(),
            Some(nb),
            None,
            4,
        ));
    }

    fn noon() -> NaiveTime {
        NaiveTime::from_hms_opt(12, 0, 0).unwrap()
    }

    #[test]
    fn evaluate_data_overdue_before_window_not_due() {
        let conn = make_test_db();
        // Logged 3 days ago, interval=1 → data-overdue.
        insert_metric(&conn, "2026-05-09", "la_min", 15.0);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = Reminder {
            id: "evening".into(),
            display: "Evening".into(),
            interval_days: 1,
            watch: WatchSource::Metric {
                id: "la_min".into(),
                count_zero_as_logged: false,
            },
            not_before: Some(NaiveTime::from_hms_opt(18, 0, 0).unwrap()),
            not_after: None,
            show_streak: false,
            show_days_past_due: false,
        };
        // Wall-clock 10:00 — before the gate.
        let now = NaiveTime::from_hms_opt(10, 0, 0).unwrap();
        let result = evaluate(&conn, today, now, &[r], &empty_config()).unwrap();
        let er = &result.reminders[0];
        assert_eq!(
            er.last_done,
            Some(NaiveDate::from_ymd_opt(2026, 5, 9).unwrap())
        );
        assert_eq!(er.days_since, Some(3));
        assert!(!er.due, "data-overdue but before window → not due");
    }

    #[test]
    fn evaluate_data_overdue_in_window_is_due() {
        let conn = make_test_db();
        insert_metric(&conn, "2026-05-09", "la_min", 15.0);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = Reminder {
            id: "evening".into(),
            display: "Evening".into(),
            interval_days: 1,
            watch: WatchSource::Metric {
                id: "la_min".into(),
                count_zero_as_logged: false,
            },
            not_before: Some(NaiveTime::from_hms_opt(18, 0, 0).unwrap()),
            not_after: Some(NaiveTime::from_hms_opt(23, 0, 0).unwrap()),
            show_streak: false,
            show_days_past_due: false,
        };
        let now = NaiveTime::from_hms_opt(20, 0, 0).unwrap();
        let result = evaluate(&conn, today, now, &[r], &empty_config()).unwrap();
        assert!(result.reminders[0].due);
    }

    #[test]
    fn evaluate_data_overdue_after_window_not_due() {
        let conn = make_test_db();
        insert_metric(&conn, "2026-05-09", "la_min", 15.0);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = Reminder {
            id: "morning".into(),
            display: "Morning".into(),
            interval_days: 1,
            watch: WatchSource::Metric {
                id: "la_min".into(),
                count_zero_as_logged: false,
            },
            not_before: Some(NaiveTime::from_hms_opt(7, 0, 0).unwrap()),
            not_after: Some(NaiveTime::from_hms_opt(12, 0, 0).unwrap()),
            show_streak: false,
            show_days_past_due: false,
        };
        // Wall-clock 14:00 — past the upper bound.
        let now = NaiveTime::from_hms_opt(14, 0, 0).unwrap();
        let result = evaluate(&conn, today, now, &[r], &empty_config()).unwrap();
        assert!(!result.reminders[0].due);
    }

    #[test]
    fn evaluate_not_data_overdue_in_window_not_due() {
        let conn = make_test_db();
        // Logged today — not data-overdue.
        insert_metric(&conn, "2026-05-12", "la_min", 15.0);
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = Reminder {
            id: "evening".into(),
            display: "Evening".into(),
            interval_days: 1,
            watch: WatchSource::Metric {
                id: "la_min".into(),
                count_zero_as_logged: false,
            },
            not_before: Some(NaiveTime::from_hms_opt(18, 0, 0).unwrap()),
            not_after: None,
            show_streak: false,
            show_days_past_due: false,
        };
        let now = NaiveTime::from_hms_opt(20, 0, 0).unwrap();
        let result = evaluate(&conn, today, now, &[r], &empty_config()).unwrap();
        assert!(!result.reminders[0].due);
    }

    #[test]
    fn evaluate_never_logged_outside_window_not_due() {
        let conn = make_test_db();
        // No history.
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let r = Reminder {
            id: "evening".into(),
            display: "Evening".into(),
            interval_days: 1,
            watch: WatchSource::Metric {
                id: "la_min".into(),
                count_zero_as_logged: false,
            },
            not_before: Some(NaiveTime::from_hms_opt(18, 0, 0).unwrap()),
            not_after: None,
            show_streak: false,
            show_days_past_due: false,
        };
        let now = NaiveTime::from_hms_opt(10, 0, 0).unwrap();
        let result = evaluate(&conn, today, now, &[r], &empty_config()).unwrap();
        assert_eq!(result.reminders[0].last_done, None);
        assert!(!result.reminders[0].due, "gate trumps 'never logged'");
    }

    #[test]
    fn evaluate_propagates_not_before_not_after_into_evaluated_reminder() {
        let conn = make_test_db();
        let today = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let nb = NaiveTime::from_hms_opt(18, 0, 0).unwrap();
        let na = NaiveTime::from_hms_opt(23, 0, 0).unwrap();
        let r = Reminder {
            id: "x".into(),
            display: "X".into(),
            interval_days: 1,
            watch: WatchSource::Metric {
                id: "la_min".into(),
                count_zero_as_logged: false,
            },
            not_before: Some(nb),
            not_after: Some(na),
            show_streak: false,
            show_days_past_due: false,
        };
        let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
        assert_eq!(result.reminders[0].not_before, Some(nb));
        assert_eq!(result.reminders[0].not_after, Some(na));
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn streak_every_other_day_worked_example() {
        // interval 2, done May 1/3/5.
        let dates = vec![d("2026-05-05"), d("2026-05-03"), d("2026-05-01")];
        assert_eq!(compute_streak(&dates, d("2026-05-05"), 2), 5); // today 5th
        assert_eq!(compute_streak(&dates, d("2026-05-06"), 2), 6); // 5th bought the 6th
        assert_eq!(compute_streak(&dates, d("2026-05-07"), 2), 6); // plateau, not yet logged
        assert_eq!(compute_streak(&dates, d("2026-05-08"), 2), 0); // broken (days_since 3 > 2)
    }

    #[test]
    fn streak_logging_the_plateau_day_advances_it() {
        // interval 2, done May 1/3/5/7, today 7th.
        let dates = vec![
            d("2026-05-07"),
            d("2026-05-05"),
            d("2026-05-03"),
            d("2026-05-01"),
        ];
        assert_eq!(compute_streak(&dates, d("2026-05-07"), 2), 7);
    }

    #[test]
    fn streak_logging_early_earns_nothing_extra() {
        // interval 2, done May 1/3/5/6, today 6th → still 6.
        let dates = vec![
            d("2026-05-06"),
            d("2026-05-05"),
            d("2026-05-03"),
            d("2026-05-01"),
        ];
        assert_eq!(compute_streak(&dates, d("2026-05-06"), 2), 6);
    }

    #[test]
    fn streak_daily_behaves_like_duolingo() {
        // interval 1, done May 1..5.
        let dates = vec![
            d("2026-05-05"),
            d("2026-05-04"),
            d("2026-05-03"),
            d("2026-05-02"),
            d("2026-05-01"),
        ];
        assert_eq!(compute_streak(&dates, d("2026-05-05"), 1), 5); // logged today
        assert_eq!(compute_streak(&dates, d("2026-05-06"), 1), 5); // done yesterday, plateau
        assert_eq!(compute_streak(&dates, d("2026-05-07"), 1), 0); // missed a full day → broken
    }

    #[test]
    fn streak_gap_exactly_interval_chains_one_more_breaks() {
        // interval 2. Chain 1,3 (gap 2 chains). Add an earlier 0 with gap 3 → breaks the chain there.
        let chained = vec![d("2026-05-03"), d("2026-05-01")];
        assert_eq!(compute_streak(&chained, d("2026-05-03"), 2), 3); // run_start = May 1
        let broken_earlier = vec![d("2026-05-03"), d("2026-05-01"), d("2026-04-28")];
        // Apr 28 → May 1 gap is 3 (> 2): run_start stays May 1, streak unchanged.
        assert_eq!(compute_streak(&broken_earlier, d("2026-05-03"), 2), 3);
    }

    #[test]
    fn streak_empty_and_single() {
        assert_eq!(compute_streak(&[], d("2026-05-05"), 2), 0);
        assert_eq!(compute_streak(&[d("2026-05-05")], d("2026-05-05"), 2), 1); // single completion today
    }

    #[test]
    fn streak_future_most_recent_is_zero() {
        // Guard: a completion dated after `today` yields no streak.
        assert_eq!(compute_streak(&[d("2026-05-10")], d("2026-05-05"), 2), 0);
    }

    #[test]
    fn streak_survives_absurd_interval_days_without_panic() {
        // An unbounded `interval_days` (e.g. a config typo) must not panic
        // when added to `last` — `checked_add_signed` should fall back to
        // `today` instead of overflowing `NaiveDate`'s representable range.
        let dates = vec![d("2026-05-05")];
        assert_eq!(compute_streak(&dates, d("2026-05-05"), u32::MAX), 1);
        assert_eq!(compute_streak(&dates, d("2026-05-05"), 4_000_000_000), 1);
    }
}
