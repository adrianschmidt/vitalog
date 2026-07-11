//! `vitalog today [date]` — print a compact daily summary.

use std::io::IsTerminal;

use chrono::NaiveDate;
use color_eyre::eyre::{Result, WrapErr};
use color_eyre::Help;
use rusqlite::Connection;
use yaml_rust2::{Yaml, YamlLoader};

use crate::config::{Config, WeightUnit};
use crate::food_sum::FoodTotals;
use crate::goals::{Goals, Threshold};

#[derive(Debug, Clone, Default)]
pub struct DayFields {
    pub weight: Option<f64>,
    pub sleep_hours: Option<f64>,
    pub sleep_start: Option<String>,
    pub sleep_end: Option<String>,
    pub mood: Option<i32>,
    pub energy: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct BpReading {
    pub sys: i32,
    pub dia: i32,
    pub pulse: i32,
}

/// One row in the `[metrics]` config-driven custom-metrics list.
#[derive(Debug, Clone)]
pub struct CustomMetric {
    pub id: String,
    pub display: String,
    pub value: Option<f64>,
    pub unit: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DaySummary {
    pub date: NaiveDate,
    pub food: FoodTotals,
    pub day: DayFields,
    /// `(delta, previous_logged_date)` if today has a weight and a prior
    /// day with a weight exists.
    pub weight_delta: Option<(f64, NaiveDate)>,
    pub bp_morning: Option<BpReading>,
    pub bp_evening: Option<BpReading>,
    pub custom_metrics: Vec<CustomMetric>,
    pub goals_warnings: Vec<String>,
    pub weight_unit: WeightUnit,
}

pub fn execute(date: Option<&str>, json: bool, config: &Config) -> Result<()> {
    let date = match date {
        Some(s) => NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
            .map_err(|_| color_eyre::eyre::eyre!("Invalid date: '{s}'. Expected YYYY-MM-DD."))
            .suggestion("Use a date in YYYY-MM-DD form, e.g., 2026-04-30.")?,
        None => config.effective_today_date(),
    };

    let mut summary = build_summary(date, config)?;

    let goals = crate::goals::load_goals(&config.notes_dir_path())?;

    // Detect goal keys with no known data source → warnings.
    let known: std::collections::HashSet<&str> = [
        "kcal",
        "protein",
        "carbs",
        "fat",
        "weight",
        "sleep_hours",
        "mood",
        "energy",
    ]
    .into_iter()
    .collect();
    let custom_ids: std::collections::HashSet<String> = config.metrics.keys().cloned().collect();
    for name in goals.thresholds.keys() {
        if !known.contains(name.as_str()) && !custom_ids.contains(name) {
            summary
                .goals_warnings
                .push(format!("unknown metric `{name}` in goals.md"));
        }
    }

    let reminders_defs = crate::reminders::load_reminders(config)?;
    let reminder_eval = if reminders_defs.is_empty() {
        crate::reminders::EvaluationResult::default()
    } else {
        let conn = crate::db::open_ro(&config.db_path())?;
        crate::reminders::evaluate(
            &conn,
            date,
            chrono::Local::now().time(),
            &reminders_defs,
            config,
        )?
    };

    if json {
        let v = render_json_with_reminders(
            &summary,
            &goals,
            &reminder_eval.reminders,
            &reminder_eval.warnings,
        );
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        print!(
            "{}",
            render_reminders_block(&reminder_eval.reminders, color)
        );
        print!("{}", render_text(&summary, &goals, color));
        for w in &reminder_eval.warnings {
            let line = paint(color, DIM, &format!("({w})"));
            println!("{line}");
        }
    }
    Ok(())
}

/// Sync the DB from notes, then assemble the summary. Hand-edits to YAML
/// or writes from `vitalog log` only touch the markdown file; without a
/// pre-read sync the days/metrics tables would be stale and surface as
/// `not logged`. Sync errors are swallowed so a single malformed note
/// does not block `vitalog today` (matches the TUI's startup behavior in
/// `app::run`). See issue #27.
fn build_summary(date: NaiveDate, config: &Config) -> Result<DaySummary> {
    let db_path = config.db_path();
    if !db_path.exists() {
        color_eyre::eyre::bail!(
            "Database not found at {}. Run `vitalog init` or `vitalog sync` first.",
            db_path.display()
        );
    }
    let conn = crate::db::open_rw(&db_path)?;
    let registry = crate::modules::build_registry(config);
    crate::db::init_db(&conn, &registry)?;
    crate::modules::validate_module_tables(&registry)?;
    let _ = crate::materializer::sync_all(&conn, &config.notes_dir_path(), config, &registry);
    assemble(date, config, &conn)
}

pub fn assemble(date: NaiveDate, config: &Config, conn: &Connection) -> Result<DaySummary> {
    let date_str = date.format("%Y-%m-%d").to_string();

    // 1. Parse food from {date}.md (if it exists). Normalize CRLF for parsers.
    let note_path = config.notes_dir_path().join(format!("{date_str}.md"));
    let raw_content = match std::fs::read_to_string(&note_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(color_eyre::eyre::eyre!(e))
                .wrap_err_with(|| format!("Failed to read {}", note_path.display()));
        }
    };
    let note_content = raw_content.replace("\r\n", "\n");
    let food = crate::food_sum::sum_food_section(&note_content);

    // 2. days-table fields.
    let day = load_day_fields(conn, &date_str)?;

    // 3. Weight delta vs previous logged day (look back 60 days).
    let weight_delta = compute_weight_delta(conn, date, &day);

    // 4. BP morning / evening — extract from YAML frontmatter (not in DB).
    let bp_morning = parse_bp_reading(&note_content, "bp_morning");
    let bp_evening = parse_bp_reading(&note_content, "bp_evening");

    // 5. Custom metrics from [metrics] config.
    let custom_metrics = load_custom_metrics(conn, &date_str, config)?;

    Ok(DaySummary {
        date,
        food,
        day,
        weight_delta,
        bp_morning,
        bp_evening,
        custom_metrics,
        goals_warnings: vec![], // populated by execute() after loading goals
        weight_unit: config.weight_unit,
    })
}

fn load_day_fields(conn: &Connection, date_str: &str) -> Result<DayFields> {
    let mut stmt = conn.prepare(
        "SELECT sleep_start, sleep_end, sleep_hours, mood, energy, weight
         FROM days WHERE date = ?1",
    )?;
    let row = stmt
        .query_row([date_str], |r| {
            Ok(DayFields {
                sleep_start: r.get(0)?,
                sleep_end: r.get(1)?,
                sleep_hours: r.get(2)?,
                mood: r.get(3)?,
                energy: r.get(4)?,
                weight: r.get(5)?,
            })
        })
        .ok();
    Ok(row.unwrap_or_default())
}

fn compute_weight_delta(
    conn: &Connection,
    date: NaiveDate,
    day: &DayFields,
) -> Option<(f64, NaiveDate)> {
    let today_weight = day.weight?;
    let trend = crate::db::load_weight_trend(conn, 60).ok()?;
    for (d_str, w) in trend {
        let d = match NaiveDate::parse_from_str(&d_str, "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => continue, // unreachable in practice; defensive against malformed dates
        };
        if d < date {
            return Some((today_weight - w, d));
        }
    }
    None
}

/// Read sys/dia/pulse from a YAML map under `{prefix}_sys` /
/// `{prefix}_dia` / `{prefix}_pulse`. Returns `None` if any of the three
/// is missing or the frontmatter cannot be parsed.
fn parse_bp_reading(content: &str, prefix: &str) -> Option<BpReading> {
    let yaml_str = extract_frontmatter_str(content)?;
    let docs = YamlLoader::load_from_str(yaml_str).ok()?;
    let doc = docs.into_iter().next()?;
    let map = match doc {
        Yaml::Hash(h) => h,
        _ => return None,
    };
    let get_int = |key: &str| -> Option<i32> {
        map.iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .and_then(|(_, v)| v.as_i64())
            .map(|i| i as i32)
    };
    Some(BpReading {
        sys: get_int(&format!("{prefix}_sys"))?,
        dia: get_int(&format!("{prefix}_dia"))?,
        pulse: get_int(&format!("{prefix}_pulse"))?,
    })
}

fn extract_frontmatter_str(content: &str) -> Option<&str> {
    let body = content.strip_prefix("---\n")?;
    let close = body.find("\n---\n").or_else(|| {
        if body.ends_with("\n---") {
            Some(body.len() - 4)
        } else {
            None
        }
    })?;
    Some(&body[..close])
}

/// BP YAML keys whose values are already surfaced by the composite "BP
/// morning:" / "BP evening:" rows. When users register these as custom
/// metrics in `[metrics]` (so they can chart trends or set goals), we
/// suppress the duplicate per-component rows. See issue #20.
const BP_COMPOSITE_KEYS: &[&str] = &[
    "bp_morning_sys",
    "bp_morning_dia",
    "bp_morning_pulse",
    "bp_evening_sys",
    "bp_evening_dia",
    "bp_evening_pulse",
];

fn load_custom_metrics(
    conn: &Connection,
    date_str: &str,
    config: &Config,
) -> Result<Vec<CustomMetric>> {
    if config.metrics.is_empty() {
        return Ok(vec![]);
    }
    let logged: std::collections::HashMap<String, f64> = crate::db::load_metrics(conn, date_str)?
        .into_iter()
        .collect();
    let mut out: Vec<CustomMetric> = config
        .metrics
        .iter()
        .filter(|(id, _)| !BP_COMPOSITE_KEYS.contains(&id.as_str()))
        .map(|(id, cfg)| CustomMetric {
            id: id.clone(),
            display: cfg.display.clone(),
            unit: cfg.unit.clone(),
            value: logged.get(id).copied(),
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn paint(color: bool, code: &str, body: &str) -> String {
    if color {
        format!("{code}{body}{RESET}")
    } else {
        body.to_string()
    }
}

/// Render the "Reminders" block to prepend above the daily summary.
/// Returns `""` when no reminder is due — caller can append unconditionally.
///
/// Ordering: never-logged first, then by `days_since` descending (most
/// overdue first). Stable for equal keys via the input order, which
/// `reminders::load_reminders` already sorts alphabetically by id.
pub fn render_reminders_block(
    reminders: &[crate::reminders::EvaluatedReminder],
    color: bool,
) -> String {
    let mut due: Vec<&crate::reminders::EvaluatedReminder> =
        reminders.iter().filter(|r| r.due).collect();
    if due.is_empty() {
        return String::new();
    }
    due.sort_by(|a, b| match (a.days_since, b.days_since) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(x), Some(y)) => y.cmp(&x),
    });

    let mut out = String::new();
    let header = paint(color, RED, "⏰ Reminders");
    out.push_str(&header);
    out.push('\n');
    for r in due {
        let line = match (r.days_since, r.last_done) {
            (Some(n), Some(d)) => {
                let plural = if n == 1 { "" } else { "s" };
                format!(
                    "- {} — overdue ({n} day{plural} ago, {})",
                    r.display,
                    d.format("%Y-%m-%d")
                )
            }
            _ => format!("- {} — never logged", r.display),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out.push('\n');
    out
}

/// Render the summary as a human-readable terminal block.
/// `color = true` enables ANSI escape codes for accent colors.
pub fn render_text(summary: &DaySummary, goals: &Goals, color: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} — Daily summary\n\n", summary.date));

    // --- Food block ---
    let kcal_t = goals.thresholds.get("kcal");
    out.push_str(&render_food_row(
        "Calories",
        summary.food.kcal,
        "kcal",
        kcal_t,
        color,
    ));
    let protein_t = goals.thresholds.get("protein");
    out.push_str(&render_food_row(
        "Protein",
        summary.food.protein,
        "g",
        protein_t,
        color,
    ));
    let carbs_t = goals.thresholds.get("carbs");
    out.push_str(&render_food_row(
        "Carbs",
        summary.food.carbs,
        "g",
        carbs_t,
        color,
    ));
    let fat_t = goals.thresholds.get("fat");
    out.push_str(&render_food_row("Fat", summary.food.fat, "g", fat_t, color));

    out.push('\n');

    // --- Weight / Sleep / BP ---
    out.push_str(&render_weight_row(
        summary,
        goals.thresholds.get("weight"),
        color,
    ));
    out.push_str(&render_sleep_row(summary, color));
    out.push_str(&render_bp_row("BP morning:  ", &summary.bp_morning, color));
    out.push_str(&render_bp_row("BP evening:  ", &summary.bp_evening, color));

    // --- Custom metrics ---
    for m in &summary.custom_metrics {
        out.push_str(&render_custom_row(m, goals.thresholds.get(&m.id), color));
    }

    // --- Hint lines ---
    let mut hints: Vec<String> = Vec::new();
    if !goals.present {
        hints.push(format!(
            "(No goals defined — add `<metric>_min/_max/_target` keys to {}.)",
            goals.source_path.display()
        ));
    }
    if summary.food.skipped_lines > 0 {
        let plural = if summary.food.skipped_lines == 1 {
            ""
        } else {
            "s"
        };
        hints.push(format!(
            "({} food line{plural} couldn't be parsed)",
            summary.food.skipped_lines
        ));
    }
    for w in &summary.goals_warnings {
        hints.push(format!("({w})"));
    }
    if !hints.is_empty() {
        out.push('\n');
        for h in hints {
            out.push_str(&paint(color, DIM, &h));
            out.push('\n');
        }
    }

    out
}

fn render_food_row(
    label: &str,
    value: f64,
    unit: &str,
    threshold: Option<&Threshold>,
    color: bool,
) -> String {
    let value_int = value.round() as i64;
    let goal_part = match threshold {
        Some(t) => format_threshold_inline(t, unit),
        None => String::new(),
    };
    let annotation = match threshold {
        Some(t) => annotate_value(value, t, color),
        None => String::new(),
    };
    let body = if goal_part.is_empty() {
        format!("{label}: {value_int} {unit}")
    } else {
        format!("{label}: {value_int} / {goal_part}")
    };
    if annotation.is_empty() {
        format!("{body}\n")
    } else {
        format!("{body}     {annotation}\n")
    }
}

/// Format a threshold inline: "1900–2200 kcal", "≥140 g", "≤65 bpm",
/// "→ 110 kg", or combinations. When `target` accompanies a min/max
/// bound, it is appended parenthetically (e.g. "≤110 kg (target 95)")
/// so users who set both still see both in the text output.
fn format_threshold_inline(t: &Threshold, unit: &str) -> String {
    let bound = match (t.min, t.max) {
        (Some(min), Some(max)) => format!("{}–{} {unit}", trim_num(min), trim_num(max)),
        (Some(min), None) => format!("≥{} {unit}", trim_num(min)),
        (None, Some(max)) => format!("≤{} {unit}", trim_num(max)),
        (None, None) => String::new(),
    };
    match (bound.is_empty(), t.target) {
        (true, Some(tgt)) => format!("→ {} {unit}", trim_num(tgt)),
        (true, None) => String::new(),
        (false, Some(tgt)) => format!("{bound} (target {})", trim_num(tgt)),
        (false, None) => bound,
    }
}

fn trim_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

/// Build the trailing `(387 below min)` / `✓ over minimum` / `✓ within range`
/// annotation for a value vs threshold.
fn annotate_value(value: f64, t: &Threshold, color: bool) -> String {
    if let Some(min) = t.min {
        if value < min {
            let delta = (min - value).round() as i64;
            return paint(color, RED, &format!("({delta} below min)"));
        }
    }
    if let Some(max) = t.max {
        if value > max {
            let delta = (value - max).round() as i64;
            return paint(color, RED, &format!("({delta} above max)"));
        }
    }
    if t.min.is_some() && t.max.is_none() {
        return paint(color, GREEN, "✓ over minimum");
    }
    if t.min.is_none() && t.max.is_some() {
        return paint(color, GREEN, "✓ under maximum");
    }
    if t.min.is_some() && t.max.is_some() {
        return paint(color, GREEN, "✓ within range");
    }
    // Target-only: don't annotate (just show the target inline).
    String::new()
}

fn render_weight_row(summary: &DaySummary, threshold: Option<&Threshold>, color: bool) -> String {
    let unit = summary.weight_unit.to_string();
    let value = match summary.day.weight {
        Some(v) => v,
        None => {
            return format!("Weight:    {}\n", paint(color, DIM, "not logged"));
        }
    };
    let goal_part = match threshold {
        Some(t) => format_threshold_inline(t, &unit),
        None => String::new(),
    };
    let annotation = match threshold {
        Some(t) => annotate_value(value, t, color),
        None => String::new(),
    };
    let mut line = if goal_part.is_empty() {
        format!("Weight:    {} {unit}", trim_num(value))
    } else {
        format!("Weight:    {} {unit} / {goal_part}", trim_num(value))
    };
    if !annotation.is_empty() {
        line.push_str("     ");
        line.push_str(&annotation);
    }
    if let Some((delta, prev_date)) = summary.weight_delta {
        let label = format_delta_label(summary.date, prev_date);
        let sign = if delta >= 0.0 { "+" } else { "" };
        line.push_str(&format!("  (Δ {sign}{} vs {label})", trim_num(delta)));
    }
    line.push('\n');
    line
}

fn format_delta_label(today: NaiveDate, prev: NaiveDate) -> String {
    let diff = today.signed_duration_since(prev).num_days();
    if diff == 1 {
        "yesterday".into()
    } else {
        prev.format("%Y-%m-%d").to_string()
    }
}

fn render_sleep_row(summary: &DaySummary, color: bool) -> String {
    match summary.day.sleep_hours {
        Some(h) => {
            let hours = h.floor() as i64;
            let mins = ((h - h.floor()) * 60.0).round() as i64;
            format!("Sleep:     {hours}h {mins:02}min\n")
        }
        None => format!("Sleep:     {}\n", paint(color, DIM, "not logged")),
    }
}

fn render_bp_row(label: &str, reading: &Option<BpReading>, color: bool) -> String {
    match reading {
        Some(b) => format!("{label} {}/{} (pulse {})\n", b.sys, b.dia, b.pulse),
        None => format!("{label} {}\n", paint(color, DIM, "not logged")),
    }
}

fn render_custom_row(metric: &CustomMetric, threshold: Option<&Threshold>, color: bool) -> String {
    let unit_str = metric.unit.as_deref().unwrap_or("");
    let value_str = match metric.value {
        Some(v) => trim_num(v),
        None => return format!("{}: {}\n", metric.display, paint(color, DIM, "not logged")),
    };
    let goal_part = match threshold {
        Some(t) => format_threshold_inline(t, unit_str),
        None => String::new(),
    };
    let annotation = match (metric.value, threshold) {
        (Some(v), Some(t)) => annotate_value(v, t, color),
        _ => String::new(),
    };
    let body = if goal_part.is_empty() {
        if unit_str.is_empty() {
            format!("{}: {value_str}", metric.display)
        } else {
            format!("{}: {value_str} {unit_str}", metric.display)
        }
    } else {
        format!("{}: {value_str} / {goal_part}", metric.display)
    };
    if annotation.is_empty() {
        format!("{body}\n")
    } else {
        format!("{body}     {annotation}\n")
    }
}

pub fn render_json(summary: &DaySummary, goals: &Goals) -> serde_json::Value {
    let mut metrics = serde_json::Map::new();

    // Food macros — always present (zeros if no entries).
    metrics.insert(
        "kcal".into(),
        metric_obj(summary.food.kcal, goals.thresholds.get("kcal"), None),
    );
    metrics.insert(
        "protein".into(),
        metric_obj(summary.food.protein, goals.thresholds.get("protein"), None),
    );
    metrics.insert(
        "carbs".into(),
        metric_obj(summary.food.carbs, goals.thresholds.get("carbs"), None),
    );
    metrics.insert(
        "fat".into(),
        metric_obj(summary.food.fat, goals.thresholds.get("fat"), None),
    );

    // Optional days-table metrics.
    if let Some(w) = summary.day.weight {
        let mut o = metric_obj(w, goals.thresholds.get("weight"), None);
        if let Some((delta, prev)) = summary.weight_delta {
            o["delta"] = delta.into();
            o["delta_vs_date"] = prev.format("%Y-%m-%d").to_string().into();
        }
        metrics.insert("weight".into(), o);
    }
    if let Some(h) = summary.day.sleep_hours {
        metrics.insert(
            "sleep_hours".into(),
            metric_obj(h, goals.thresholds.get("sleep_hours"), None),
        );
    }
    if let Some(m) = summary.day.mood {
        metrics.insert(
            "mood".into(),
            metric_obj(m as f64, goals.thresholds.get("mood"), None),
        );
    }
    if let Some(e) = summary.day.energy {
        metrics.insert(
            "energy".into(),
            metric_obj(e as f64, goals.thresholds.get("energy"), None),
        );
    }

    // Custom metrics (only those with logged values).
    for m in &summary.custom_metrics {
        if let Some(v) = m.value {
            metrics.insert(
                m.id.clone(),
                metric_obj(v, goals.thresholds.get(&m.id), m.unit.clone()),
            );
        }
    }

    // Sleep object (richer view) — separate from `metrics.sleep_hours`.
    let sleep = match (
        summary.day.sleep_hours,
        &summary.day.sleep_start,
        &summary.day.sleep_end,
    ) {
        (Some(h), Some(s), Some(e)) => serde_json::json!({
            "hours": h,
            "start": s,
            "end": e,
        }),
        (Some(h), _, _) => serde_json::json!({ "hours": h }),
        _ => serde_json::Value::Null,
    };

    let bp_json = |r: &Option<BpReading>| match r {
        Some(b) => serde_json::json!({ "sys": b.sys, "dia": b.dia, "pulse": b.pulse }),
        None => serde_json::Value::Null,
    };
    let bp_morning = bp_json(&summary.bp_morning);
    let bp_evening = bp_json(&summary.bp_evening);

    // Warnings: collected from food.skipped_lines + goals_warnings.
    let mut warnings: Vec<serde_json::Value> = summary
        .goals_warnings
        .iter()
        .map(|s| serde_json::Value::String(s.clone()))
        .collect();
    if summary.food.skipped_lines > 0 {
        let plural = if summary.food.skipped_lines == 1 {
            ""
        } else {
            "s"
        };
        warnings.push(serde_json::Value::String(format!(
            "{} food line{plural} couldn't be parsed",
            summary.food.skipped_lines
        )));
    }

    serde_json::json!({
        "date": summary.date.format("%Y-%m-%d").to_string(),
        "metrics": serde_json::Value::Object(metrics),
        "sleep": sleep,
        "bp_morning": bp_morning,
        "bp_evening": bp_evening,
        "goals_present": goals.present,
        "warnings": warnings,
    })
}

/// Like `render_json` but also embeds the `reminders` array and a
/// `reminder_warnings` sibling. The existing `warnings` array is left
/// untouched — reminder warnings stay in their own stream so JSON
/// consumers can route them separately (per spec).
pub fn render_json_with_reminders(
    summary: &DaySummary,
    goals: &Goals,
    reminders: &[crate::reminders::EvaluatedReminder],
    reminder_warnings: &[String],
) -> serde_json::Value {
    let mut v = render_json(summary, goals);

    let (rs, warns) = crate::reminders::to_json(reminders, reminder_warnings);
    v["reminders"] = rs;
    v["reminder_warnings"] = warns;

    v
}

fn metric_obj(
    value: f64,
    threshold: Option<&Threshold>,
    unit: Option<String>,
) -> serde_json::Value {
    let mut o = serde_json::Map::new();
    o.insert("value".into(), value.into());
    let (min, max, target) = match threshold {
        Some(t) => (t.min, t.max, t.target),
        None => (None, None, None),
    };
    o.insert(
        "min".into(),
        min.map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null),
    );
    o.insert(
        "max".into(),
        max.map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null),
    );
    o.insert(
        "target".into(),
        target
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null),
    );
    if let Some(u) = unit {
        o.insert("unit".into(), serde_json::Value::String(u));
    }
    serde_json::Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fixture_summary() -> DaySummary {
        DaySummary {
            date: NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
            food: FoodTotals {
                kcal: 1513.0,
                protein: 147.0,
                carbs: 77.0,
                fat: 59.0,
                entry_count: 4,
                skipped_lines: 0,
            },
            day: DayFields {
                weight: Some(121.5),
                sleep_hours: Some(6.4),
                sleep_start: Some("23:00".into()),
                sleep_end: Some("05:24".into()),
                mood: None,
                energy: None,
            },
            weight_delta: Some((1.3, NaiveDate::from_ymd_opt(2026, 4, 29).unwrap())),
            bp_morning: None,
            bp_evening: None,
            custom_metrics: vec![],
            goals_warnings: vec![],
            weight_unit: WeightUnit::Kg,
        }
    }

    fn fixture_goals() -> Goals {
        let mut thresholds = HashMap::new();
        thresholds.insert(
            "kcal".into(),
            Threshold {
                min: Some(1900.0),
                max: Some(2200.0),
                target: None,
            },
        );
        thresholds.insert(
            "protein".into(),
            Threshold {
                min: Some(140.0),
                max: None,
                target: None,
            },
        );
        thresholds.insert(
            "weight".into(),
            Threshold {
                target: Some(110.0),
                min: None,
                max: None,
            },
        );
        Goals {
            thresholds,
            source_path: std::path::PathBuf::from("/tmp/goals.md"),
            present: true,
        }
    }

    #[test]
    fn render_text_food_block_with_goals() {
        let s = fixture_summary();
        let g = fixture_goals();
        let out = render_text(&s, &g, false);
        assert!(out.contains("2026-04-30 — Daily summary"), "got:\n{out}");
        assert!(out.contains("Calories:"), "got:\n{out}");
        assert!(out.contains("1513"), "got:\n{out}");
        assert!(out.contains("1900–2200 kcal"), "got:\n{out}");
        assert!(out.contains("387 below min"), "got:\n{out}");
        assert!(out.contains("Protein:"), "got:\n{out}");
        assert!(out.contains("147"), "got:\n{out}");
        assert!(out.contains("≥140 g"), "got:\n{out}");
        assert!(out.contains("over minimum"), "got:\n{out}");
        assert!(out.contains("Carbs:"), "got:\n{out}");
        assert!(out.contains("77 g"), "got:\n{out}");
        assert!(out.contains("Fat:"), "got:\n{out}");
        assert!(out.contains("59 g"), "got:\n{out}");
    }

    #[test]
    fn render_text_weight_sleep_bp_block() {
        let s = fixture_summary();
        let g = fixture_goals();
        let out = render_text(&s, &g, false);
        assert!(out.contains("Weight:    121.5 kg"), "got:\n{out}");
        assert!(out.contains("→ 110 kg"), "got:\n{out}");
        assert!(out.contains("Δ +1.3 vs yesterday"), "got:\n{out}");
        assert!(out.contains("Sleep:     6h 24min"), "got:\n{out}");
        assert!(out.contains("BP morning:"), "got:\n{out}");
        assert!(out.contains("BP evening:"), "got:\n{out}");
        assert!(out.contains("not logged"), "got:\n{out}");
    }

    #[test]
    fn render_text_bp_evening_row_with_values() {
        let mut s = fixture_summary();
        s.bp_evening = Some(BpReading {
            sys: 132,
            dia: 82,
            pulse: 65,
        });
        let g = fixture_goals();
        let out = render_text(&s, &g, false);
        let line = out
            .lines()
            .find(|l| l.starts_with("BP evening:"))
            .expect("BP evening row missing");
        assert!(line.contains("132/82"), "got: {line}");
        assert!(line.contains("pulse 65"), "got: {line}");
    }

    #[test]
    fn render_text_bp_evening_row_not_logged_when_missing() {
        let s = fixture_summary(); // bp_evening = None
        let g = fixture_goals();
        let out = render_text(&s, &g, false);
        let line = out
            .lines()
            .find(|l| l.starts_with("BP evening:"))
            .expect("BP evening row missing");
        assert!(line.contains("not logged"), "got: {line}");
    }

    #[test]
    fn render_text_weight_above_max_annotates() {
        // vitalog#18: weight row should annotate over-max values like food rows do.
        let mut s = fixture_summary();
        s.day.weight = Some(119.4);
        s.weight_delta = Some((0.2, NaiveDate::from_ymd_opt(2026, 4, 29).unwrap()));
        let mut g = fixture_goals();
        g.thresholds.insert(
            "weight".into(),
            Threshold {
                min: None,
                max: Some(110.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert!(out.contains("Weight:    119.4 kg / ≤110 kg"), "got:\n{out}");
        assert!(out.contains("9 above max"), "got:\n{out}");
        assert!(out.contains("Δ +0.2 vs yesterday"), "got:\n{out}");
    }

    #[test]
    fn render_text_weight_within_range_annotates() {
        let mut s = fixture_summary();
        s.day.weight = Some(95.0);
        s.weight_delta = None;
        let mut g = fixture_goals();
        g.thresholds.insert(
            "weight".into(),
            Threshold {
                min: Some(80.0),
                max: Some(110.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert!(out.contains("80–110 kg"), "got:\n{out}");
        assert!(out.contains("✓ within range"), "got:\n{out}");
    }

    #[test]
    fn render_text_weight_target_only_no_annotation() {
        // Target-only threshold should produce no annotation for the weight row
        // (matches annotate_value behavior for target-only thresholds).
        let s = fixture_summary();
        let g = fixture_goals(); // weight has target: Some(110.0), no min/max
        let out = render_text(&s, &g, false);
        let weight_line = out
            .lines()
            .find(|l| l.starts_with("Weight:"))
            .expect("weight row");
        assert!(weight_line.contains("→ 110 kg"), "got: {weight_line}");
        assert!(!weight_line.contains("above max"), "got: {weight_line}");
        assert!(!weight_line.contains("below min"), "got: {weight_line}");
        assert!(!weight_line.contains("within range"), "got: {weight_line}");
    }

    #[test]
    fn render_text_no_goals_emits_hint() {
        let s = fixture_summary();
        let g = Goals {
            thresholds: HashMap::new(),
            source_path: std::path::PathBuf::from("/notes/goals.md"),
            present: false,
        };
        let out = render_text(&s, &g, false);
        assert!(out.contains("No goals defined"), "got:\n{out}");
        assert!(out.contains("/notes/goals.md"), "got:\n{out}");
        // No goal annotations on rows.
        assert!(!out.contains("below min"));
        assert!(!out.contains("over minimum"));
    }

    #[test]
    fn render_text_skipped_food_lines_emits_hint() {
        let mut s = fixture_summary();
        s.food.skipped_lines = 2;
        let g = fixture_goals();
        let out = render_text(&s, &g, false);
        assert!(
            out.contains("2 food lines couldn't be parsed"),
            "got:\n{out}"
        );
    }

    #[test]
    fn render_text_unknown_metric_warning() {
        let mut s = fixture_summary();
        s.goals_warnings
            .push("unknown metric `mystery` in goals.md".into());
        let g = fixture_goals();
        let out = render_text(&s, &g, false);
        assert!(out.contains("unknown metric `mystery`"), "got:\n{out}");
    }

    #[test]
    fn render_text_weight_delta_non_yesterday_uses_actual_date() {
        let mut s = fixture_summary();
        s.date = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        s.weight_delta = Some((0.4, NaiveDate::from_ymd_opt(2026, 4, 25).unwrap()));
        let g = fixture_goals();
        let out = render_text(&s, &g, false);
        assert!(out.contains("Δ +0.4 vs 2026-04-25"), "got:\n{out}");
        assert!(!out.contains("vs yesterday"));
    }

    #[test]
    fn render_text_color_off_strips_escapes() {
        let mut s = fixture_summary();
        s.day.weight = None; // forces a "not logged" row
        let g = fixture_goals();
        let out = render_text(&s, &g, false);
        assert!(!out.contains("\x1b["), "got:\n{out:?}");
    }

    #[test]
    fn render_text_color_on_includes_escapes_for_below_min() {
        let s = fixture_summary();
        let g = fixture_goals();
        let out = render_text(&s, &g, true);
        assert!(out.contains("\x1b[31m"), "got:\n{out:?}");
    }

    #[test]
    fn render_text_custom_metric_with_max_above_max() {
        let mut s = fixture_summary();
        s.custom_metrics.push(CustomMetric {
            id: "resting_hr".into(),
            display: "Resting HR".into(),
            value: Some(72.0),
            unit: Some("bpm".into()),
        });
        let mut g = fixture_goals();
        g.thresholds.insert(
            "resting_hr".into(),
            Threshold {
                max: Some(65.0),
                min: None,
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert!(out.contains("Resting HR: 72 / ≤65 bpm"), "got:\n{out}");
        assert!(out.contains("7 above max"), "got:\n{out}");
    }

    #[test]
    fn render_json_shape() {
        let s = fixture_summary();
        let g = fixture_goals();
        let v = render_json(&s, &g);
        assert_eq!(v["date"], "2026-04-30");
        let kcal = &v["metrics"]["kcal"];
        assert_eq!(kcal["value"], 1513.0);
        assert_eq!(kcal["min"], 1900.0);
        assert_eq!(kcal["max"], 2200.0);
        assert!(kcal["target"].is_null());
        assert_eq!(v["metrics"]["weight"]["value"], 121.5);
        assert_eq!(v["metrics"]["weight"]["target"], 110.0);
        assert_eq!(v["metrics"]["weight"]["delta"], 1.3);
        assert_eq!(v["metrics"]["weight"]["delta_vs_date"], "2026-04-29");
        assert!(v["bp_morning"].is_null());
        assert!(v["bp_evening"].is_null());
        assert_eq!(v["sleep"]["hours"], 6.4);
        assert_eq!(v["sleep"]["start"], "23:00");
        assert_eq!(v["sleep"]["end"], "05:24");
        assert_eq!(v["goals_present"], true);
        assert!(v["warnings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn render_json_bp_evening_present_when_set() {
        let mut s = fixture_summary();
        s.bp_evening = Some(BpReading {
            sys: 130,
            dia: 80,
            pulse: 62,
        });
        let g = fixture_goals();
        let v = render_json(&s, &g);
        assert_eq!(v["bp_evening"]["sys"], 130);
        assert_eq!(v["bp_evening"]["dia"], 80);
        assert_eq!(v["bp_evening"]["pulse"], 62);
    }

    #[test]
    fn render_json_includes_warnings_and_skipped() {
        let mut s = fixture_summary();
        s.food.skipped_lines = 1;
        s.goals_warnings
            .push("unknown metric `mystery` in goals.md".into());
        let g = fixture_goals();
        let v = render_json(&s, &g);
        let warnings = v["warnings"].as_array().unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("mystery")));
        assert!(warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("food line")));
    }

    #[test]
    fn render_text_target_only_threshold_has_no_annotation() {
        let mut s = fixture_summary();
        s.custom_metrics.push(CustomMetric {
            id: "rhr".into(),
            display: "RHR".into(),
            value: Some(60.0),
            unit: Some("bpm".into()),
        });
        let mut g = fixture_goals();
        g.thresholds.insert(
            "rhr".into(),
            Threshold {
                target: Some(58.0),
                min: None,
                max: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert!(out.contains("RHR: 60 / → 58 bpm"), "got:\n{out}");
        // No annotation suffix for target-only thresholds: isolate the RHR
        // row and confirm it has no ✓ / below min / above max marker.
        let rhr_line = out
            .lines()
            .find(|l| l.starts_with("RHR:"))
            .expect("RHR row missing");
        assert!(!rhr_line.contains("✓"), "got:\n{rhr_line}");
        assert!(!rhr_line.contains("below min"), "got:\n{rhr_line}");
        assert!(!rhr_line.contains("above max"), "got:\n{rhr_line}");
    }

    use crate::db;

    fn config_in(notes_dir: &std::path::Path) -> Config {
        let toml_str = format!(
            "notes_dir = '{}'\ntime_format = '24h'\nweight_unit = 'kg'\n",
            notes_dir.display().to_string().replace('\\', "/")
        );
        toml::from_str(&toml_str).unwrap()
    }

    #[test]
    fn assemble_reads_food_weight_sleep_bp() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        // Write a daily note with food + BP morning frontmatter.
        let date = "2026-04-30";
        let note = format!(
            "---\n\
             date: {date}\n\
             weight: 121.5\n\
             sleep: \"23:00-05:24\"\n\
             bp_morning_sys: 138\n\
             bp_morning_dia: 88\n\
             bp_morning_pulse: 70\n\
             ---\n\n\
             ## Food\n\
             - **08:00** Eggs (200 kcal, 12.0g protein, 1.0g carbs, 15.0g fat)\n\
             - **12:00** Pasta (500 kcal, 18.0g protein, 80.0g carbs, 10.0g fat)\n"
        );
        std::fs::write(dir.path().join(format!("{date}.md")), note).unwrap();

        // Set up DB and sync the note (so days table gets weight/sleep).
        let registry = crate::modules::build_registry(&config);
        let conn = db::open_rw(&config.db_path()).unwrap();
        db::init_db(&conn, &registry).unwrap();
        crate::modules::validate_module_tables(&registry).unwrap();
        crate::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry).unwrap();

        let target = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let summary = assemble(target, &config, &conn).unwrap();

        assert_eq!(summary.food.kcal, 700.0);
        assert_eq!(summary.food.entry_count, 2);
        assert_eq!(summary.day.weight, Some(121.5));
        assert!((summary.day.sleep_hours.unwrap() - 6.4).abs() < 0.05);
        let bp = summary.bp_morning.unwrap();
        assert_eq!(bp.sys, 138);
        assert_eq!(bp.dia, 88);
        assert_eq!(bp.pulse, 70);
    }

    #[test]
    fn assemble_parses_bp_evening_from_yaml() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        let date = "2026-04-30";
        let note = format!(
            "---\n\
             date: {date}\n\
             bp_evening_sys: 132\n\
             bp_evening_dia: 82\n\
             bp_evening_pulse: 65\n\
             ---\n"
        );
        std::fs::write(dir.path().join(format!("{date}.md")), note).unwrap();

        let registry = crate::modules::build_registry(&config);
        let conn = db::open_rw(&config.db_path()).unwrap();
        db::init_db(&conn, &registry).unwrap();
        crate::modules::validate_module_tables(&registry).unwrap();
        crate::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry).unwrap();

        let target = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let summary = assemble(target, &config, &conn).unwrap();

        let bp = summary.bp_evening.expect("bp_evening should be parsed");
        assert_eq!(bp.sys, 132);
        assert_eq!(bp.dia, 82);
        assert_eq!(bp.pulse, 65);
    }

    /// Regression for vitalog#20: when a user registers `bp_morning_*` or
    /// `bp_evening_*` as custom metrics, the composite "BP morning:" /
    /// "BP evening:" rows already cover those values, so the duplicated
    /// custom-metric rows must be suppressed.
    #[test]
    fn assemble_filters_bp_keys_from_custom_metrics() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml_str = format!(
            "notes_dir = '{}'\ntime_format = '24h'\nweight_unit = 'kg'\n\
             [metrics]\n\
             bp_morning_sys = {{ display = \"BP AM Sys\", color = \"red\", unit = \"mmHg\" }}\n\
             bp_morning_dia = {{ display = \"BP AM Dia\", color = \"red\", unit = \"mmHg\" }}\n\
             bp_morning_pulse = {{ display = \"BP AM Pulse\", color = \"red\", unit = \"bpm\" }}\n\
             bp_evening_sys = {{ display = \"BP PM Sys\", color = \"magenta\", unit = \"mmHg\" }}\n\
             bp_evening_dia = {{ display = \"BP PM Dia\", color = \"magenta\", unit = \"mmHg\" }}\n\
             bp_evening_pulse = {{ display = \"BP PM Pulse\", color = \"magenta\", unit = \"bpm\" }}\n\
             other_metric = {{ display = \"Other\", color = \"green\" }}\n",
            dir.path().display().to_string().replace('\\', "/")
        );
        let config: Config = toml::from_str(&toml_str).unwrap();

        let date = "2026-04-30";
        let note = format!(
            "---\n\
             date: {date}\n\
             bp_morning_sys: 138\n\
             bp_morning_dia: 88\n\
             bp_morning_pulse: 70\n\
             bp_evening_sys: 132\n\
             bp_evening_dia: 82\n\
             bp_evening_pulse: 65\n\
             other_metric: 42\n\
             ---\n"
        );
        std::fs::write(dir.path().join(format!("{date}.md")), note).unwrap();

        let registry = crate::modules::build_registry(&config);
        let conn = db::open_rw(&config.db_path()).unwrap();
        db::init_db(&conn, &registry).unwrap();
        crate::modules::validate_module_tables(&registry).unwrap();
        crate::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry).unwrap();

        let target = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let summary = assemble(target, &config, &conn).unwrap();

        let ids: Vec<&str> = summary
            .custom_metrics
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        for bp_key in [
            "bp_morning_sys",
            "bp_morning_dia",
            "bp_morning_pulse",
            "bp_evening_sys",
            "bp_evening_dia",
            "bp_evening_pulse",
        ] {
            assert!(
                !ids.contains(&bp_key),
                "BP key `{bp_key}` should be filtered from custom_metrics (composite row covers it); got ids: {ids:?}"
            );
        }
        // Non-BP custom metrics are still passed through.
        assert!(
            ids.contains(&"other_metric"),
            "non-BP custom metrics should remain; got ids: {ids:?}"
        );
    }

    #[test]
    fn assemble_weight_delta_uses_previous_logged_day() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        for (d, w) in [("2026-04-25", 120.0), ("2026-04-30", 121.3)] {
            let note = format!("---\ndate: {d}\nweight: {w}\n---\n\n## Food\n");
            std::fs::write(dir.path().join(format!("{d}.md")), note).unwrap();
        }

        let registry = crate::modules::build_registry(&config);
        let conn = db::open_rw(&config.db_path()).unwrap();
        db::init_db(&conn, &registry).unwrap();
        crate::modules::validate_module_tables(&registry).unwrap();
        crate::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry).unwrap();

        let target = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let summary = assemble(target, &config, &conn).unwrap();
        let (delta, prev) = summary.weight_delta.unwrap();
        assert!((delta - 1.3).abs() < 1e-6);
        assert_eq!(prev, NaiveDate::from_ymd_opt(2026, 4, 25).unwrap());
    }

    #[test]
    fn assemble_missing_note_yields_zero_food() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        let registry = crate::modules::build_registry(&config);
        let conn = db::open_rw(&config.db_path()).unwrap();
        db::init_db(&conn, &registry).unwrap();
        crate::modules::validate_module_tables(&registry).unwrap();

        let target = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let summary = assemble(target, &config, &conn).unwrap();
        assert_eq!(summary.food, FoodTotals::default());
        assert!(summary.day.weight.is_none());
        assert!(summary.bp_morning.is_none());
        assert!(summary.bp_evening.is_none());
    }

    /// vitalog#20: rendering with both morning and evening readings
    /// populated produces exactly one row per slot — no duplicate from
    /// any custom-metric pass-through.
    #[test]
    fn render_text_bp_both_slots_populated_render_once_each() {
        let mut s = fixture_summary();
        s.bp_morning = Some(BpReading {
            sys: 138,
            dia: 88,
            pulse: 70,
        });
        s.bp_evening = Some(BpReading {
            sys: 132,
            dia: 82,
            pulse: 65,
        });
        let g = fixture_goals();
        let out = render_text(&s, &g, false);
        let morning_rows = out.lines().filter(|l| l.starts_with("BP morning:")).count();
        let evening_rows = out.lines().filter(|l| l.starts_with("BP evening:")).count();
        assert_eq!(morning_rows, 1, "got:\n{out}");
        assert_eq!(evening_rows, 1, "got:\n{out}");
        assert!(out.contains("138/88 (pulse 70)"), "got:\n{out}");
        assert!(out.contains("132/82 (pulse 65)"), "got:\n{out}");
    }

    /// Regression: `vitalog log metric ...` writes to YAML only, so a
    /// subsequent `vitalog today` previously rendered the value as
    /// `not logged`. `build_summary` must sync the DB from notes before
    /// reading so the just-written value shows up. See issue #27.
    #[test]
    fn build_summary_syncs_log_writes_before_reading_metrics() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml_str = format!(
            "notes_dir = '{}'\ntime_format = '24h'\nweight_unit = 'kg'\n\
             [metrics]\nbrushed_morning = {{ display = \"Brush AM\", color = \"green\" }}\n",
            dir.path().display().to_string().replace('\\', "/")
        );
        let config: Config = toml::from_str(&toml_str).unwrap();

        let registry = crate::modules::build_registry(&config);
        {
            let conn = db::open_rw(&config.db_path()).unwrap();
            db::init_db(&conn, &registry).unwrap();
            crate::modules::validate_module_tables(&registry).unwrap();
            crate::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry)
                .unwrap();
        }

        crate::cli::log_cmd::execute(
            "metric",
            &["brushed_morning".into(), "1".into()],
            &config,
            &registry,
        )
        .unwrap();

        let date = config.effective_today_date();
        let summary = build_summary(date, &config).unwrap();

        let metric = summary
            .custom_metrics
            .iter()
            .find(|m| m.id == "brushed_morning")
            .expect("custom metric should be registered in summary");
        assert_eq!(
            metric.value,
            Some(1.0),
            "metric should be synced from YAML before read; got {:?}",
            metric.value
        );
    }

    #[test]
    fn trim_num_subtracted_decimals_round_to_one_dp() {
        // Reproduce the IEEE-754 artifact: 121.5 - 121.3 = 0.20000000000000284.
        let delta = 121.5_f64 - 121.3_f64;
        assert!(delta != 0.2, "test premise broken: got {delta}");
        assert_eq!(trim_num(delta), "0.2");
    }

    #[test]
    fn trim_num_integer_values_have_no_decimal() {
        assert_eq!(trim_num(1900.0), "1900");
        assert_eq!(trim_num(0.0), "0");
        assert_eq!(trim_num(-7.0), "-7");
    }

    #[test]
    fn trim_num_clean_decimal_renders_one_dp() {
        assert_eq!(trim_num(121.5), "121.5");
        assert_eq!(trim_num(0.5), "0.5");
        assert_eq!(trim_num(-1.3), "-1.3");
    }

    fn th(min: Option<f64>, max: Option<f64>, target: Option<f64>) -> Threshold {
        Threshold { min, max, target }
    }

    #[test]
    fn format_threshold_inline_target_only_arms() {
        assert_eq!(
            format_threshold_inline(&th(Some(1900.0), Some(2200.0), None), "kcal"),
            "1900–2200 kcal"
        );
        assert_eq!(
            format_threshold_inline(&th(Some(140.0), None, None), "g"),
            "≥140 g"
        );
        assert_eq!(
            format_threshold_inline(&th(None, Some(110.0), None), "kg"),
            "≤110 kg"
        );
        assert_eq!(
            format_threshold_inline(&th(None, None, Some(110.0)), "kg"),
            "→ 110 kg"
        );
        assert_eq!(format_threshold_inline(&th(None, None, None), "kg"), "");
    }

    #[test]
    fn format_threshold_inline_target_with_max_appends_parenthetical() {
        // The vitalog#19 case: weight_max + weight_target shows both.
        assert_eq!(
            format_threshold_inline(&th(None, Some(110.0), Some(95.0)), "kg"),
            "≤110 kg (target 95)"
        );
    }

    #[test]
    fn format_threshold_inline_target_with_min_appends_parenthetical() {
        assert_eq!(
            format_threshold_inline(&th(Some(140.0), None, Some(175.0)), "g"),
            "≥140 g (target 175)"
        );
    }

    #[test]
    fn format_threshold_inline_target_with_range_appends_parenthetical() {
        assert_eq!(
            format_threshold_inline(&th(Some(1900.0), Some(2200.0), Some(2000.0)), "kcal"),
            "1900–2200 kcal (target 2000)"
        );
    }

    use crate::reminders::EvaluatedReminder;

    fn evald(
        id: &str,
        display: &str,
        days_since: Option<i64>,
        last_done: Option<NaiveDate>,
        due: bool,
        interval: u32,
    ) -> EvaluatedReminder {
        EvaluatedReminder {
            id: id.into(),
            display: display.into(),
            interval_days: interval,
            last_done,
            days_since,
            due,
            not_before: None,
            not_after: None,
            streak: None,
            days_past_due: None,
        }
    }

    #[test]
    fn reminders_block_empty_when_nothing_due() {
        let rs = vec![evald(
            "a",
            "A",
            Some(0),
            Some(NaiveDate::from_ymd_opt(2026, 5, 12).unwrap()),
            false,
            1,
        )];
        let block = render_reminders_block(&rs, false);
        assert_eq!(block, "");
    }

    #[test]
    fn reminders_block_empty_when_no_reminders() {
        let block = render_reminders_block(&[], false);
        assert_eq!(block, "");
    }

    #[test]
    fn reminders_block_renders_due_lines_with_days_since() {
        let rs = vec![
            evald(
                "lactic_acid",
                "Lactic acid training",
                Some(3),
                Some(NaiveDate::from_ymd_opt(2026, 5, 9).unwrap()),
                true,
                2,
            ),
            evald("weigh_in", "Daily weigh-in", None, None, true, 1),
        ];
        let block = render_reminders_block(&rs, false);
        assert!(block.contains("Reminders"), "got:\n{block}");
        assert!(block.contains("Lactic acid training"), "got:\n{block}");
        assert!(block.contains("3 days ago"), "got:\n{block}");
        assert!(block.contains("2026-05-09"), "got:\n{block}");
        assert!(block.contains("Daily weigh-in"), "got:\n{block}");
        assert!(block.contains("never logged"), "got:\n{block}");
        // Block ends with a blank line separator before the date header.
        assert!(block.ends_with("\n\n"), "got:\n{block:?}");
    }

    #[test]
    fn reminders_block_orders_most_overdue_first() {
        let rs = vec![
            evald(
                "a",
                "A two-day",
                Some(2),
                Some(NaiveDate::from_ymd_opt(2026, 5, 10).unwrap()),
                true,
                1,
            ),
            evald("b", "Never B", None, None, true, 1),
            evald(
                "c",
                "C five-day",
                Some(5),
                Some(NaiveDate::from_ymd_opt(2026, 5, 7).unwrap()),
                true,
                1,
            ),
        ];
        let block = render_reminders_block(&rs, false);
        let lines: Vec<&str> = block.lines().filter(|l| l.starts_with("- ")).collect();
        // never-logged ranks above any finite days_since; then descending
        // by days_since.
        assert!(lines[0].contains("Never B"), "got:\n{block}");
        assert!(lines[1].contains("C five-day"), "got:\n{block}");
        assert!(lines[2].contains("A two-day"), "got:\n{block}");
    }

    #[test]
    fn reminders_block_skips_not_due_entries() {
        let rs = vec![
            evald(
                "due",
                "Due one",
                Some(2),
                Some(NaiveDate::from_ymd_opt(2026, 5, 10).unwrap()),
                true,
                1,
            ),
            evald(
                "ok",
                "Not due one",
                Some(0),
                Some(NaiveDate::from_ymd_opt(2026, 5, 12).unwrap()),
                false,
                1,
            ),
        ];
        let block = render_reminders_block(&rs, false);
        assert!(block.contains("Due one"), "got:\n{block}");
        assert!(!block.contains("Not due one"), "got:\n{block}");
    }

    #[test]
    fn reminders_block_color_on_emits_ansi() {
        let rs = vec![evald(
            "a",
            "A",
            Some(3),
            Some(NaiveDate::from_ymd_opt(2026, 5, 9).unwrap()),
            true,
            2,
        )];
        let block = render_reminders_block(&rs, true);
        assert!(
            block.contains("\x1b["),
            "expected ANSI codes, got:\n{block:?}"
        );
    }

    #[test]
    fn reminders_block_color_off_strips_ansi() {
        let rs = vec![evald(
            "a",
            "A",
            Some(3),
            Some(NaiveDate::from_ymd_opt(2026, 5, 9).unwrap()),
            true,
            2,
        )];
        let block = render_reminders_block(&rs, false);
        assert!(!block.contains("\x1b["), "got:\n{block:?}");
    }

    #[test]
    fn render_json_includes_empty_reminders_when_none_configured() {
        let s = fixture_summary();
        let g = fixture_goals();
        let v = render_json_with_reminders(&s, &g, &[], &[]);
        assert!(v["reminders"].is_array(), "got:\n{v}");
        assert_eq!(v["reminders"].as_array().unwrap().len(), 0);
        assert!(v["reminder_warnings"].is_array(), "got:\n{v}");
        assert_eq!(v["reminder_warnings"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn render_json_lists_all_reminders_including_not_due() {
        let s = fixture_summary();
        let g = fixture_goals();
        let rs = vec![
            evald(
                "lactic_acid",
                "Lactic acid training",
                Some(3),
                Some(NaiveDate::from_ymd_opt(2026, 5, 9).unwrap()),
                true,
                2,
            ),
            evald(
                "weigh_in",
                "Daily weigh-in",
                Some(0),
                Some(NaiveDate::from_ymd_opt(2026, 5, 12).unwrap()),
                false,
                1,
            ),
        ];
        let v = render_json_with_reminders(&s, &g, &rs, &[]);
        let arr = v["reminders"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let la = &arr[0];
        assert_eq!(la["id"], "lactic_acid");
        assert_eq!(la["display"], "Lactic acid training");
        assert_eq!(la["interval_days"], 2);
        assert_eq!(la["last_done"], "2026-05-09");
        assert_eq!(la["days_since"], 3);
        assert_eq!(la["due"], true);

        let weigh = &arr[1];
        assert_eq!(weigh["id"], "weigh_in");
        assert_eq!(weigh["due"], false);
        assert_eq!(weigh["days_since"], 0);
    }

    #[test]
    fn render_json_reminder_with_no_last_done_uses_null() {
        let s = fixture_summary();
        let g = fixture_goals();
        let rs = vec![evald("never", "Never logged", None, None, true, 1)];
        let v = render_json_with_reminders(&s, &g, &rs, &[]);
        let r = &v["reminders"][0];
        assert!(r["last_done"].is_null());
        assert!(r["days_since"].is_null());
    }

    #[test]
    fn render_json_includes_not_before_and_not_after() {
        let s = fixture_summary();
        let g = fixture_goals();
        let rs = vec![EvaluatedReminder {
            id: "evening".into(),
            display: "Evening".into(),
            interval_days: 1,
            last_done: None,
            days_since: None,
            due: false,
            not_before: Some(chrono::NaiveTime::from_hms_opt(18, 0, 0).unwrap()),
            not_after: Some(chrono::NaiveTime::from_hms_opt(23, 0, 0).unwrap()),
            streak: None,
            days_past_due: None,
        }];
        let v = render_json_with_reminders(&s, &g, &rs, &[]);
        let r = &v["reminders"][0];
        assert_eq!(r["not_before"], "18:00");
        assert_eq!(r["not_after"], "23:00");
    }

    #[test]
    fn render_json_omits_time_gates_as_null_when_unset() {
        let s = fixture_summary();
        let g = fixture_goals();
        let rs = vec![EvaluatedReminder {
            id: "all_day".into(),
            display: "All day".into(),
            interval_days: 1,
            last_done: None,
            days_since: None,
            due: true,
            not_before: None,
            not_after: None,
            streak: None,
            days_past_due: None,
        }];
        let v = render_json_with_reminders(&s, &g, &rs, &[]);
        let r = &v["reminders"][0];
        assert!(r["not_before"].is_null());
        assert!(r["not_after"].is_null());
    }

    #[test]
    fn render_json_includes_reminder_warnings() {
        let s = fixture_summary();
        let g = fixture_goals();
        let v = render_json_with_reminders(
            &s,
            &g,
            &[],
            &["reminder `x`: target metric `y` is not declared in [metrics]".to_string()],
        );
        let w = v["reminder_warnings"].as_array().unwrap();
        assert_eq!(w.len(), 1);
        assert!(w[0].as_str().unwrap().contains("target metric"));
        // The regular `warnings` array stays untouched.
        let regular = v["warnings"].as_array().unwrap();
        assert!(regular
            .iter()
            .all(|x| !x.as_str().unwrap().contains("target metric")));
    }

    #[test]
    fn execute_text_prepends_reminders_block_when_due() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml_str = format!(
            r#"
notes_dir = "{}"
time_format = "24h"
weight_unit = "kg"

[metrics]
la_min = {{ display = "Lactic acid (min)", color = "red" }}

[reminders.lactic_acid]
display = "Lactic acid training"
interval_days = 2
watch = "metric"
target = "la_min"
"#,
            dir.path().display().to_string().replace('\\', "/")
        );
        let config: Config = toml::from_str(&toml_str).unwrap();

        let registry = crate::modules::build_registry(&config);
        let conn = db::open_rw(&config.db_path()).unwrap();
        db::init_db(&conn, &registry).unwrap();
        crate::modules::validate_module_tables(&registry).unwrap();
        // Seed nothing — la_min has never been logged → reminder is due.

        // Smoke: execute should not error and the rendered text (captured
        // via the pure helper) should contain the reminder line above
        // the date header.
        let date = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
        let goals = crate::goals::load_goals(&config.notes_dir_path()).unwrap();
        let summary = assemble(date, &config, &conn).unwrap();
        let reminders = crate::reminders::load_reminders(&config).unwrap();
        let eval = crate::reminders::evaluate(
            &conn,
            date,
            chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            &reminders,
            &config,
        )
        .unwrap();

        let mut out = render_reminders_block(&eval.reminders, false);
        out.push_str(&render_text(&summary, &goals, false));
        let header_idx = out
            .find("2026-05-12 — Daily summary")
            .expect("header present");
        let block_idx = out.find("Lactic acid training").expect("reminder present");
        assert!(
            block_idx < header_idx,
            "reminder block must precede summary; got:\n{out}"
        );
    }
}
