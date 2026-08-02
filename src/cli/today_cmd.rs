//! `vitalog today [date]` — print a compact daily summary.

use std::io::IsTerminal;

use chrono::NaiveDate;
use color_eyre::eyre::{Result, WrapErr};
use color_eyre::Help;
use rusqlite::Connection;
use yaml_rust2::{Yaml, YamlLoader};

use crate::config::{Config, WeightUnit};
use crate::food_sum::{FoodTotals, NutrientTotal};
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

    summary.goals_warnings = detect_config_warnings(&goals, config);

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

/// Goal keys with a built-in data source. Anything else in `goals.md` must
/// match a `[metrics]` entry, or it is reported as an unknown metric.
const KNOWN_GOAL_METRICS: &[&str] = &[
    "kcal",
    "protein",
    "carbs",
    "fat",
    "fiber",
    "salt",
    "weight",
    "sleep_hours",
    "mood",
    "energy",
];

/// Goal keys with no data source, plus `[metrics]` ids that shadow a
/// built-in nutrient total. Surfaced as dim hints in the text output and in
/// the JSON `warnings` array.
fn detect_config_warnings(goals: &Goals, config: &Config) -> Vec<String> {
    let mut warnings = Vec::new();

    let known: std::collections::HashSet<&str> = KNOWN_GOAL_METRICS.iter().copied().collect();
    let custom_ids: std::collections::HashSet<&str> =
        config.metrics.keys().map(String::as_str).collect();
    let mut unknown: Vec<&str> = goals
        .thresholds
        .keys()
        .map(String::as_str)
        .filter(|n| !known.contains(n) && !custom_ids.contains(n))
        .collect();
    unknown.sort_unstable();
    for name in unknown {
        warnings.push(format!("unknown metric `{name}` in goals.md"));
    }

    // A `[metrics.fiber]` / `[metrics.salt]` predating the built-in food
    // totals now yields two rows with the same meaning. Say so instead of
    // silently resolving the collision either way — and do not suggest a
    // rename, which looks like the obvious fix and is usually the wrong
    // one: the config id doubles as the note frontmatter key that
    // `materializer::daily` reads values from, so renaming orphans every
    // `salt:` already written in past notes.
    let mut shadowed: Vec<&str> = config
        .metrics
        .keys()
        .map(String::as_str)
        .filter(|id| BUILTIN_NUTRIENT_METRICS.contains(id))
        .collect();
    shadowed.sort_unstable();
    for id in shadowed {
        warnings.push(format!(
            "`[metrics.{id}]` duplicates the built-in {id} total derived from your food \
             entries. Both rows are shown and the goal is ruled once, by whichever \
             row can settle it — so a shortfall on one above a check on the other \
             is a single ruling, not the same goal checked twice. `--json` keeps \
             the food-derived total at `metrics.{id}` and your figure at \
             `metrics.{id}.logged_value`. Renaming the metric would orphan the \
             `{id}:` values already in your notes. `vitalog readme` has the full \
             rule"
        ));
    }

    warnings
}

/// Built-in metric ids whose `--json` object carries a documented shape a
/// plain custom metric would break. That is the membership rule, and today
/// it selects exactly the nutrients derived from partial food coverage:
/// `fiber` and `salt` are the only `metrics.*` objects with
/// `unknown_entries` / `entry_count`, and a `metric_obj` written over one
/// of them would silently remove keys consumers are told to rely on. A
/// third such nutrient belongs here; a metric that merely happens to be
/// built in does not.
///
/// Before fiber and salt were reported, a `[metrics.salt]` entry was the
/// only way to track salt, so an existing config may well define one. The
/// two are different measurements of the same quantity — a manual daily
/// estimate versus a partial sum over logged entries — so vitalog does not
/// pick a winner: the text output shows both rows and warns, and `--json`
/// keeps the built-in in the `metrics.<id>` slot with the logged figure
/// alongside it as `logged_value`.
///
/// The other ids in `KNOWN_GOAL_METRICS` are deliberately not covered.
/// `[metrics.kcal]` does overwrite the built-in `metrics.kcal` object, but
/// both are plain `metric_obj`s, so no key disappears and nothing about
/// the collision is new in this feature — changing that resolution would
/// be a behavior break for existing configs, unrelated to fiber and salt.
const BUILTIN_NUTRIENT_METRICS: &[&str] = &["fiber", "salt"];

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
                if let Some(k) = r.streak.filter(|k| *k >= 1) {
                    format!(
                        "- {} — due today · 🔥 {k}-day streak (keep it alive)",
                        r.display
                    )
                } else if let Some(k) = r.days_past_due.filter(|k| *k >= 1) {
                    let plural = if k == 1 { "" } else { "s" };
                    format!("- {} — {k} day{plural} past due", r.display)
                } else {
                    let plural = if n == 1 { "" } else { "s" };
                    format!(
                        "- {} — overdue ({n} day{plural} ago, {})",
                        r.display,
                        d.format("%Y-%m-%d")
                    )
                }
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
    out.push_str(&render_nutrient_row(
        "Fiber",
        &summary.food.fiber,
        &summary.food,
        goals.thresholds.get("fiber"),
        &nutrient_verdicts("fiber", summary, goals),
        color,
    ));
    out.push_str(&render_nutrient_row(
        "Salt",
        &summary.food.salt,
        &summary.food,
        goals.thresholds.get("salt"),
        &nutrient_verdicts("salt", summary, goals),
        color,
    ));

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
        let threshold = goals.thresholds.get(&m.id);
        out.push_str(&render_custom_row(
            m,
            threshold,
            color,
            shadowed_row_must_withhold_annotation(m, summary, goals),
        ));
    }

    // --- Hint lines ---
    let mut hints: Vec<String> = Vec::new();
    if !goals.present {
        hints.push(format!(
            "(No goals defined — add `<metric>_min/_max/_target` keys to {}.)",
            goals.source_path.display()
        ));
    }
    if let Some(note) = summary.food.skipped_note() {
        hints.push(format!("({note})"));
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

/// Row for one of the four macros, which annotates its goal
/// unconditionally.
///
/// That is an exemption from the rule its two neighbors below follow, and
/// it is deliberate for now on grounds of scope rather than principle.
/// `skipped_lines > 0` does make these totals lower bounds in exactly the
/// sense `render_nutrient_row` withholds on — `sum_food_section` counts a
/// dropped line in neither `entry_count` nor any sum — so on a day with one
/// unparseable line and `kcal_min`/`kcal_max`,
/// `Calories: 1900 / 1900–2200 kcal     ✓ within range` can print on the
/// same screen where `Salt: 2.0+` correctly refuses its check. The day does
/// say so, once, in the `(n food lines couldn't be parsed)` hint below the
/// block.
///
/// The predicate that would close it is already here, and it is
/// `annotation_survives_unknowns(value, t)` — the same one
/// `nutrient_row_annotates` applies to Fiber and Salt — gated on
/// `skipped_lines > 0` alone, since unlike fiber and salt there is no
/// per-entry unknown to consider: a missing macro token has always read as
/// `0.0` and is indistinguishable from a measured zero. Not
/// `lower_bound_proves`, which is the *evidence* test and one step
/// stronger: gating display on it would drop
/// `Calories: 1500 / 1900–2200 kcal  (400 below min)` from any day with an
/// unparseable line — suppressing a shortfall the user can act on because
/// the total might be even lower, which is backwards. What stops it being
/// done here is that it is not this function and `skipped_lines` both predate
/// fiber and salt, and applying the rule would change Calories, Protein,
/// Carbs and Fat on every day with a dropped line. That belongs in a change
/// where it is the subject rather than a side effect.
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

/// Row for a nutrient whose coverage may be partial. The value is a lower
/// bound when entries lack it — marked with `+` and an explicit unknown
/// count — and one decimal is kept because integer rounding would destroy
/// salt, whose interesting range is 0.4–8 g.
///
/// A food line the parser dropped makes the total a lower bound in exactly
/// the sense a missing token does: its nutrients are missing from the sum
/// and `sum_food_section` counts it in neither `entry_count` nor `unknown`.
/// It therefore marks the row `+` and suppresses the same reassuring
/// verdicts, even though there is no per-entry count to attach to it — the
/// `(n food lines couldn't be parsed)` hint below the block carries that
/// number, once for the whole day rather than once per nutrient.
fn render_nutrient_row(
    label: &str,
    total: &NutrientTotal,
    food: &FoodTotals,
    threshold: Option<&Threshold>,
    verdicts: &NutrientVerdicts,
    color: bool,
) -> String {
    let unit = NUTRIENT_UNIT;
    let lower_bound = total.is_lower_bound(food.skipped_lines);
    let value_str = if lower_bound {
        format!("{:.1}+", total.sum)
    } else {
        format!("{:.1}", total.sum)
    };
    let goal_part = match threshold {
        Some(t) => format_threshold_inline(t, unit),
        None => String::new(),
    };
    let mut line = if goal_part.is_empty() {
        format!("{label}: {value_str} {unit}")
    } else {
        format!("{label}: {value_str} / {goal_part}")
    };

    let annotation = match threshold {
        Some(t) if verdicts.food_annotates => annotate_value(total.sum, t, color),
        _ => String::new(),
    };
    if !annotation.is_empty() {
        line.push_str("     ");
        line.push_str(&annotation);
    }
    if total.unknown > 0 {
        line.push_str("  ");
        line.push_str(&paint(color, DIM, &format!("({} unknown)", total.unknown)));
    }
    // The note lands here rather than on the `[metrics.*]` row because
    // this is the row whose verdict the disagreement most often removes,
    // and because it is the first of the two on screen. It names both
    // figures, so it reads the same wherever it is met.
    if let Some(note) = &verdicts.note {
        line.push_str("  ");
        line.push_str(&paint(color, RED, &format!("⚠ {note}")));
    }
    line.push('\n');
    line
}

/// Whether the food-derived row for this nutrient prints a goal verdict.
///
/// This is *the* decision `render_nutrient_row` makes about its
/// annotation, and it is stated once here so that `render_json` can ask for
/// the answer instead of predicting it. What the row prints and what its
/// total *proves* are two different questions, though — see
/// `food_evidence_verdict` for the second one.
///
/// An exact total earns every verdict; anything less earns only the ones
/// that survive being a lower bound. "Exact" needs both halves — no gaps
/// *and* something actually measured — because a day with no food entries
/// has no gaps either and would otherwise collect the green check on the
/// strength of a structural zero. The final clause covers the threshold
/// shapes there is no verdict for at all (target-only): the gate can pass
/// while the row still prints nothing. It asks `goal_verdict` rather than
/// testing `annotate_value`'s string for emptiness, so no gate in this file
/// reads another's wording.
///
/// Note what is *not* here: a shortfall against a `_min` goal survives a
/// structural zero, deliberately. `fiber_min: 35` on a day whose entries
/// carried no fiber still reads `(35 below min)` beside the `+` and the
/// `(n unknown)` count, because a running total that cannot tell you
/// whether you are short is the gap this feature was asked to close, and
/// zero coverage is the common case while most of the food db carries no
/// `fiber:` key. `nutrient_verdicts` steps that verdict aside in the one
/// case where something else on screen can rule instead — a `[metrics.*]`
/// row logging the same nutrient — and nowhere else.
fn nutrient_row_annotates(total: &NutrientTotal, food: &FoodTotals, t: &Threshold) -> bool {
    let exact = !total.is_lower_bound(food.skipped_lines) && nutrient_row_is_measured(total, food);
    (exact || annotation_survives_unknowns(total.sum, t))
        && goal_verdict(total.sum, t) != GoalVerdict::Silent
}

/// Whether the food-derived row for this nutrient rests on at least one
/// actual measurement — an entry that carried the value.
///
/// `is_complete()` alone cannot answer that: with nothing counted, nothing
/// is unknown either, so a day with no food entries reports as an *exact*
/// zero and collects whatever verdict that zero happens to earn. For salt
/// against a cap that verdict is `✓ under maximum`, computed from zero
/// measurements — the reassurance this whole feature exists to withhold.
/// This is the same "nothing is known" state `--json` marks as
/// `unknown_entries == entry_count`, and it covers both shapes of it: no
/// entries at all, and entries none of which carried the nutrient.
///
/// It is also the shape of the entire back-catalogue: every `## Food` line
/// written before nutrients were tracked carries none, so on those days
/// the food-derived total is a structural `0.0+` forever and a manually
/// logged figure is the only real number there is. Nothing here may take
/// the goal check away from it.
fn nutrient_row_is_measured(total: &NutrientTotal, food: &FoodTotals) -> bool {
    total.is_measured(food.entry_count)
}

/// Whether an open lower bound *proves* the verdict `annotate_value` picks
/// for `value` against `t`. The one monotone-safety test in the file.
///
/// Unknown entries can only add, so all the day's data establishes is that
/// the true total is *at least* `value`. That settles lower-bound claims and
/// nothing else — and exactly two of the five verdicts are claims of that
/// shape. `(n above max)` and `✓ over minimum` both say "the true value is
/// at least X". `(n below min)`, `✓ under maximum` and `✓ within range` all
/// say "the true value is at most X", which no lower bound can establish,
/// whichever way the goal points.
///
/// So the rule is one predicate rather than a table of surviving verdicts:
/// **a lower bound can only prove a lower-bound claim.** It needs no
/// per-direction reasoning, which is the point — the same defect has been
/// found three times in this file, each time as the invariant re-derived in
/// one more branch and got wrong in one of them. Every path that turns a
/// food-derived total into a verdict routes through here:
/// `food_evidence_verdict` for what the collision rule may treat as
/// evidence, `annotation_survives_unknowns` for what the row prints.
///
/// The branch order mirrors `annotate_value`'s so the two cannot disagree
/// about *which* verdict is on screen for a figure that breaks a min and a
/// max at once (a `min > max` config).
fn lower_bound_proves(value: f64, t: &Threshold) -> bool {
    if t.min.is_some_and(|min| value < min) {
        // `(n below min)` — "you are at most X", which more entries can undo.
        return false;
    }
    if t.max.is_some_and(|max| value > max) {
        // `(n above max)` — more entries can only make it truer.
        return true;
    }
    // `✓ over minimum` is the last verdict that claims a lower bound;
    // `✓ under maximum` and `✓ within range` both bound from above.
    t.min.is_some() && t.max.is_none()
}

/// Whether a goal annotation still *prints* when the total is only a lower
/// bound.
///
/// Everything `lower_bound_proves` establishes, plus one deliberate
/// exception: `(n below min)` is kept even though the unknowns could erase
/// the shortfall entirely. That is sound as *display* — the `+` and the
/// `(n unknown)` count sit on the same row, so the number reads "of what has
/// been measured, you are short", which is true and is the signal this
/// feature was asked for. It is not sound as *evidence*, and nothing may
/// carry it into a claim about the day's true total, which is why the
/// collision rule reads `food_evidence_verdict` and not this. See the design
/// doc's annotation table.
fn annotation_survives_unknowns(value: f64, t: &Threshold) -> bool {
    lower_bound_proves(value, t) || t.min.is_some_and(|min| value < min)
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

/// Format a goal overage/shortfall.
///
/// Whole numbers read best for the metrics whose bounds are in the
/// hundreds, but salt is the first built-in whose cap is small enough for
/// integer rounding to degenerate: against the recommended `salt_max: 6`,
/// every total from 6.01 to 6.49 g rounded to `(0 above max)` — a red
/// warning stating that the overage is nothing. Decimals are added only as
/// far as the first that shows something: one when the integer rounds to
/// zero, two when one decimal does as well.
///
/// Two is where it stops, because two is what salt is stored at
/// (`render_salt_grams`) — a total assembled from hundredths cannot land
/// strictly between zero and 0.005 of a cap except through float noise, so
/// a third decimal would only ever print rounding error. That noise is
/// what the remaining degenerate band renders: a delta under 0.005 still
/// prints `(0.00 above max)`. `main` printed `(0 above max)` for the same
/// input, and the root cause is upstream — `annotate_value` compares
/// against the bound without an epsilon — so this stops at making the
/// reachable deltas legible rather than papering over that. Rows whose values
/// are written as integers (kcal) never reach the fallback at all; protein
/// is written at one decimal and does, so a 139.9 g total against
/// `protein_min: 140` now reads `(0.1 below min)` where it used to read
/// `(0 below min)`.
fn format_goal_delta(delta: f64) -> String {
    let rounded = delta.round();
    if rounded != 0.0 {
        return format!("{}", rounded as i64);
    }
    if (delta * 10.0).round() != 0.0 {
        return format!("{delta:.1}");
    }
    format!("{delta:.2}")
}

/// Build the trailing `(387 below min)` / `✓ over minimum` / `✓ within range`
/// annotation for a value vs threshold.
fn annotate_value(value: f64, t: &Threshold, color: bool) -> String {
    if let Some(min) = t.min {
        if value < min {
            let delta = format_goal_delta(min - value);
            return paint(color, RED, &format!("({delta} below min)"));
        }
    }
    if let Some(max) = t.max {
        if value > max {
            let delta = format_goal_delta(value - max);
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

/// The food-derived total a built-in nutrient id names.
///
/// `BUILTIN_NUTRIENT_METRICS` says *which* ids have a food-derived row;
/// this says *what* that row sums. Adding a third nutrient to the constant
/// without adding it here would make the shadowing rule silently skip it,
/// so `builtin_nutrient_metrics_all_resolve_to_a_total` pins the two
/// together.
fn builtin_nutrient_total<'a>(id: &str, food: &'a FoodTotals) -> Option<&'a NutrientTotal> {
    match id {
        "fiber" => Some(&food.fiber),
        "salt" => Some(&food.salt),
        _ => None,
    }
}

/// Which side of a goal a figure falls on, independent of how the verdict
/// is worded.
///
/// `annotate_value` picks the wording; this is the classification the
/// collision rule keys on, and it is derived from the threshold rather
/// than from that wording so the two can be pinned against each other
/// (`goal_verdict_agrees_with_annotate_value`) instead of one parsing the
/// other's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoalVerdict {
    /// A green check — the figure is on the right side of every bound set.
    Reassuring,
    /// A red shortfall or overage — the figure breaks a bound.
    Warning,
    /// The threshold has no pass/fail to give (target-only, or empty).
    Silent,
}

fn goal_verdict(value: f64, t: &Threshold) -> GoalVerdict {
    if t.min.is_some_and(|min| value < min) || t.max.is_some_and(|max| value > max) {
        GoalVerdict::Warning
    } else if t.min.is_some() || t.max.is_some() {
        GoalVerdict::Reassuring
    } else {
        GoalVerdict::Silent
    }
}

/// What the food-derived total *proves* about the goal, as opposed to what
/// its row prints.
///
/// The single owner of the food side's contribution to the collision rule.
/// Every branch of `nutrient_verdicts` reads this and nothing else, so no
/// branch can re-derive monotone safety and miss a case — which is exactly
/// how a `(27 below min)` computed off `8.4+` with nine entries unmeasured
/// came to be read as a firm verdict and made `⚠ … cannot reconcile` fire
/// against a logged 40 g the data agrees with completely.
///
/// Two states prove nothing, for two different reasons:
///
/// - Nothing was measured. The zero is structural, so there is no
///   observation to reason from at all — not even a lower bound worth the
///   name. (`nutrient_verdicts` intercepts this shape earlier for the
///   display decision it also drives; repeating the test keeps this
///   function correct read on its own, and it is not redundant — a day with
///   no food entries has no *gaps* either, so `is_lower_bound` is false
///   there.)
/// - The total is still an open lower bound and `lower_bound_proves` says
///   the bound does not establish the verdict.
fn food_evidence_verdict(total: &NutrientTotal, food: &FoodTotals, t: &Threshold) -> GoalVerdict {
    if !nutrient_row_is_measured(total, food) {
        return GoalVerdict::Silent;
    }
    if total.is_lower_bound(food.skipped_lines) && !lower_bound_proves(total.sum, t) {
        return GoalVerdict::Silent;
    }
    goal_verdict(total.sum, t)
}

/// The unit both food-derived nutrient totals are summed in.
const NUTRIENT_UNIT: &str = "g";

/// What the two rows for one built-in nutrient say about the day's goal.
///
/// Computed once from the summary and read by both renderers, so `--json`
/// cannot bless a figure `render_text` refused to. Text and JSON
/// disagreeing is worse than either being wrong alone: a JSON consumer has
/// no way to tell it is reading a number the text surface deliberately
/// withheld its verdict from.
#[derive(Debug, Clone, Default)]
struct NutrientVerdicts {
    /// The food-derived row prints its goal verdict.
    food_annotates: bool,
    /// The shadowing `[metrics.*]` row prints its goal verdict. Meaningless
    /// when there is no such row.
    logged_annotates: bool,
    /// Names both figures when they disagree about the goal.
    note: Option<String>,
}

/// Note for a day whose two figures for one nutrient disagree about the
/// goal.
///
/// Names both numbers so it stands on its own wherever it is read: the two
/// rows sit in different blocks of the summary, and `--json` carries the
/// same string with no rows at all.
///
/// The measured half carries the same `+` its row prints when the total is a
/// lower bound. On screen the `+` is a few characters to the left, but in
/// `verdict_note` the note is all a consumer gets, and a bare `8.0 g
/// measured` there claims an exactness the figure does not have.
fn reconciliation_note(logged: f64, measured: f64, measured_is_lower_bound: bool) -> String {
    let plus = if measured_is_lower_bound { "+" } else { "" };
    format!(
        "logged {} {NUTRIENT_UNIT} vs {measured:.1}{plus} {NUTRIENT_UNIT} measured — cannot reconcile",
        trim_num(logged),
    )
}

/// Resolve the goal check for one built-in nutrient across the food-derived
/// row and any `[metrics.*]` row shadowing it.
///
/// Pure and cheap, so every caller recomputes rather than threading the
/// result around — there is no second copy to fall out of date.
///
/// The two rows carry the same label and the same threshold, so annotating
/// both puts two verdicts on screen for one goal. The base rule is
/// therefore: the goal is checked once, on the row vitalog controls,
/// unless that row cannot rule. `food_evidence_verdict` answers whether it
/// can — *not* `nutrient_row_annotates`, which answers the different
/// question of what the row prints. When the food-derived side cannot rule
/// the logged row takes over, with one restriction: it may not hand back a
/// reassurance the food-derived row withheld as unprovable, which is what
/// `annotation_survives_unknowns` screens the logged figure for, being
/// false for exactly the two reassuring verdicts a lower bound cannot
/// establish. A warning always survives: it says something the
/// food-derived row's silence never claimed.
///
/// Two shapes of day fall outside that base rule, and both are reached only
/// when a `[metrics.*]` row actually logged a figure. Nothing below changes
/// what a lone food-derived row prints.
///
/// **The food-derived row measured nothing.** No entries, or none carrying
/// the nutrient, and its zero is structural rather than observed. It never
/// had a reassuring verdict to give there, and the shortfall it *does* give
/// (`(35 below min)` off an empty sum) is computed from nothing measured,
/// so the logged figure — the day's only real number — rules instead. One
/// line, not two. This is not a corner case: every `## Food` line predating
/// nutrient tracking carries none, so it is the shape of the whole
/// back-catalogue.
///
/// Standing that verdict down is worth it only because something else takes
/// its place. With no `[metrics.*]` row there is no second line to prefer,
/// so the shortfall stays where it is — see `nutrient_row_annotates`.
///
/// **The two figures disagree about the goal.** Full coverage does not mean
/// all of the nutrient is accounted for: salt added while cooking or at the
/// table never reaches the food-derived total, and a restaurant meal logged
/// as one entry systematically under-captures seasoning. The food-derived
/// total is a lower bound *even at full coverage*, so a manual figure above
/// it may be the more complete number rather than a contradiction of the
/// measurement. Printing `✓ under maximum` off 3.5 g while a deliberately
/// logged 8 g sits below it under `salt_max: 6` is reassurance the day's
/// own data denies. Stated as the rule: **never show a reassuring verdict
/// that another logged figure on the same day contradicts.** The `✓` is
/// withheld, the warning stands, and a note names both figures so the
/// missing check has a reason on screen.
///
/// That is keyed strictly on the two verdicts, never on the gap between the
/// figures. A numeric threshold would be arbitrary and would need
/// re-tuning per goal; this fires exactly when the discrepancy changes what
/// the day calls for, so 3.4 against 3.5 stays silent and 3.5 against 8
/// under a cap of 6 does not. It also needs no special-casing per goal
/// direction: under-reporting is the dangerous error under a `_max` goal
/// and over-reporting under a `_min` one, and withholding the contradicted
/// reassurance is correct for both.
///
/// Disagreement needs *two* verdicts, and the food-derived side supplies one
/// only where its total proves it. That is `food_evidence_verdict`, which
/// parts company with what the row prints on exactly one shape: a shortfall
/// against a `_min` goal off an open lower bound. `8.4+` with nine of twelve
/// entries unmeasured under `fiber_min: 35` correctly prints
/// `(27 below min)` — of what was measured, the day is short — but proves
/// nothing about the day's true total, which those nine entries could carry
/// well past 35. Mirroring it, `2.5+` against a logged 8 g under a cap of 6
/// is not a contradiction either: the unmeasured entry could carry the
/// missing 5.5 g. Both are the same predicate, not two rules — a lower
/// bound can only prove a lower-bound claim — and claiming either pair
/// "cannot reconcile" would be false.
///
/// Nothing is lost by staying quiet there. The row keeps its shortfall, and
/// the logged figure — which the food-derived side has not contradicted —
/// rules on the goal in its place.
///
/// **One consequence worth naming**, because it is where "one goal, one
/// verdict" visibly bends: under a `_min` goal at partial coverage both rows
/// can print a shortfall at once.
///
/// ```text
/// Fiber: 8.4+ / ≥35 g     (27 below min)  (9 unknown)
/// Logged fiber: 20 / ≥35 g     (15 below min)
/// ```
///
/// Two numbers against one goal, and both are true — but only the second is
/// a *ruling*. The first says what the measured entries add up to, beside
/// the `+` and the `(9 unknown)` that say they are not the whole day. The
/// invariant the sibling tests pin by name is that the goal is **decided**
/// once, not that only one row may carry a number; deciding it here is
/// something the food-derived row cannot do. The two neighboring shapes are
/// both wrong in ways that are easy to reach for: suppressing the food
/// row's shortfall hides a signal the user can act on, and letting that
/// shortfall stand the logged row down lets an unproven verdict silence a
/// proven one. Printing both is what is left, and the coverage sweep's
/// one-verdict-per-goal property requires it.
fn nutrient_verdicts(id: &str, summary: &DaySummary, goals: &Goals) -> NutrientVerdicts {
    let (Some(total), Some(t)) = (
        builtin_nutrient_total(id, &summary.food),
        goals.thresholds.get(id),
    ) else {
        return NutrientVerdicts::default();
    };
    let food_annotates = nutrient_row_annotates(total, &summary.food, t);
    let Some(logged) = summary
        .custom_metrics
        .iter()
        .find(|m| m.id == id)
        .and_then(|m| m.value)
    else {
        return NutrientVerdicts {
            food_annotates,
            ..NutrientVerdicts::default()
        };
    };

    // A structural zero is not a measurement: nothing to rule with, and
    // nothing for the logged figure to contradict.
    if !nutrient_row_is_measured(total, &summary.food) {
        return NutrientVerdicts {
            food_annotates: false,
            logged_annotates: true,
            note: None,
        };
    }

    // What the food-derived total proves, which is not always what its row
    // prints — see `food_evidence_verdict`. Only the first may be treated as
    // one half of a disagreement, or as a ruling that stands the logged row
    // down.
    let food_verdict = food_evidence_verdict(total, &summary.food, t);
    let lower_bound = total.is_lower_bound(summary.food.skipped_lines);
    match (food_verdict, goal_verdict(logged, t)) {
        (GoalVerdict::Reassuring, GoalVerdict::Warning) => NutrientVerdicts {
            food_annotates: false,
            logged_annotates: true,
            note: Some(reconciliation_note(logged, total.sum, lower_bound)),
        },
        (GoalVerdict::Warning, GoalVerdict::Reassuring) => NutrientVerdicts {
            food_annotates,
            logged_annotates: false,
            note: Some(reconciliation_note(logged, total.sum, lower_bound)),
        },
        // They agree, or the food-derived side has no verdict to give: the
        // base rule stands. Note the row may still be printing a shortfall
        // here — it just isn't evidence, so it does not stand the logged
        // row down.
        _ => NutrientVerdicts {
            food_annotates,
            logged_annotates: food_verdict == GoalVerdict::Silent
                && annotation_survives_unknowns(logged, t),
            note: None,
        },
    }
}

/// Whether a `[metrics.*]` row whose id shadows a built-in nutrient has to
/// go without its goal annotation. See `nutrient_verdicts` for the rule.
fn shadowed_row_must_withhold_annotation(
    metric: &CustomMetric,
    summary: &DaySummary,
    goals: &Goals,
) -> bool {
    if !BUILTIN_NUTRIENT_METRICS.contains(&metric.id.as_str()) {
        return false;
    }
    !nutrient_verdicts(&metric.id, summary, goals).logged_annotates
}

fn render_custom_row(
    metric: &CustomMetric,
    threshold: Option<&Threshold>,
    color: bool,
    withhold_annotation: bool,
) -> String {
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
        (Some(v), Some(t)) if !withhold_annotation => annotate_value(v, t, color),
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
    metrics.insert(
        "fiber".into(),
        nutrient_metric_obj(
            &summary.food.fiber,
            goals.thresholds.get("fiber"),
            summary.food.entry_count,
            summary.food.skipped_lines,
            &nutrient_verdicts("fiber", summary, goals),
        ),
    );
    metrics.insert(
        "salt".into(),
        nutrient_metric_obj(
            &summary.food.salt,
            goals.thresholds.get("salt"),
            summary.food.entry_count,
            summary.food.skipped_lines,
            &nutrient_verdicts("salt", summary, goals),
        ),
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

    // Custom metrics (only those with logged values). A custom id that
    // collides with a built-in nutrient must not replace that object with
    // a differently-shaped one — consumers rely on `unknown_entries` /
    // `entry_count` being present on `fiber` and `salt`. The logged figure
    // is not dropped for it either: it hangs off the same object as
    // `logged_value`, so both measurements stay reachable and neither
    // surface has to pick a winner. `logged_verdict` rides along so a
    // consumer can tell which of the two figures the goal was checked on —
    // the same call `render_text` makes, not a second opinion. `execute`
    // warns about the collision.
    for m in &summary.custom_metrics {
        let Some(v) = m.value else { continue };
        if BUILTIN_NUTRIENT_METRICS.contains(&m.id.as_str()) {
            if let Some(o) = metrics.get_mut(&m.id) {
                o["logged_value"] = v.into();
                if let Some(u) = &m.unit {
                    o["logged_unit"] = serde_json::Value::String(u.clone());
                }
                let verdicts = nutrient_verdicts(&m.id, summary, goals);
                o["logged_verdict"] =
                    verdict_json(verdicts.logged_annotates, v, goals.thresholds.get(&m.id));
            }
            continue;
        }
        metrics.insert(
            m.id.clone(),
            metric_obj(v, goals.thresholds.get(&m.id), m.unit.clone()),
        );
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
    if let Some(note) = summary.food.skipped_note() {
        warnings.push(serde_json::Value::String(note));
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

/// Like `metric_obj`, plus how many entries lacked the value, how many
/// food entries the day has in total, and how many food lines the parser
/// dropped.
///
/// `unknown_entries` is the only way a consumer can tell an exact total
/// from a lower bound. `entry_count` is what lets it reconstruct the third
/// state the text surface renders — `unknown_entries == entry_count` means
/// nothing is known, which `{"value": 0.0, "unknown_entries": 3}` alone
/// cannot be distinguished from a partial total whose known entries
/// happened to sum to zero. Without it an agent reading `--json` would draw
/// the reassuring conclusion `render_text` deliberately suppresses.
///
/// `skipped_lines` closes the remaining hole in that reconstruction. A
/// dropped food line is counted in neither of the other two, so
/// `{"unknown_entries": 0, "entry_count": 1}` alone reads as an exact
/// total on a day where two further lines went unparsed. It is a
/// day-scoped count repeated on both nutrient objects, so the exactness
/// test stays a property of the object a consumer is already holding.
///
/// `verdict` and `verdict_note` carry the goal check `render_text` made,
/// rather than leaving a consumer to redo it from `value` / `min` / `max`
/// and reach a conclusion the text surface refused to print. The counts
/// above make that redo *possible*, but only for a reader who has read the
/// rules; the verdict states the answer.
fn nutrient_metric_obj(
    total: &NutrientTotal,
    threshold: Option<&Threshold>,
    entry_count: usize,
    skipped_lines: usize,
    verdicts: &NutrientVerdicts,
) -> serde_json::Value {
    let mut o = metric_obj(total.sum, threshold, None);
    o["unknown_entries"] = serde_json::Value::from(total.unknown);
    o["entry_count"] = serde_json::Value::from(entry_count);
    o["skipped_lines"] = serde_json::Value::from(skipped_lines);
    o["verdict"] = verdict_json(verdicts.food_annotates, total.sum, threshold);
    o["verdict_note"] = match &verdicts.note {
        Some(n) => serde_json::Value::String(n.clone()),
        None => serde_json::Value::Null,
    };
    o
}

/// The JSON form of a printed goal verdict: `"ok"`, `"warn"`, or `null`.
///
/// `null` covers every reason no verdict reaches the screen — no goal, a
/// target-only goal, and the cases the rules above withhold one — because
/// a consumer needs to act on all three the same way: vitalog is declining
/// to rule, so do not rule for it. `min` / `max` are on the same object
/// for anyone who needs to tell those apart.
fn verdict_json(annotates: bool, value: f64, threshold: Option<&Threshold>) -> serde_json::Value {
    if !annotates {
        return serde_json::Value::Null;
    }
    match threshold.map(|t| goal_verdict(value, t)) {
        Some(GoalVerdict::Reassuring) => serde_json::Value::String("ok".into()),
        Some(GoalVerdict::Warning) => serde_json::Value::String("warn".into()),
        _ => serde_json::Value::Null,
    }
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
                fiber: NutrientTotal::default(),
                salt: NutrientTotal::default(),
                entry_count: 4,
                skipped_lines: 0,
                ..Default::default()
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

    fn summary_with(fiber: NutrientTotal, salt: NutrientTotal, entries: usize) -> DaySummary {
        let mut s = fixture_summary();
        s.food.fiber = fiber;
        s.food.salt = salt;
        s.food.entry_count = entries;
        s
    }

    fn goals_with(name: &str, t: Threshold) -> Goals {
        let mut g = fixture_goals();
        g.thresholds.insert(name.into(), t);
        g
    }

    fn row<'a>(out: &'a str, label: &str) -> &'a str {
        out.lines()
            .find(|l| l.starts_with(label))
            .unwrap_or_else(|| panic!("row `{label}` missing in:\n{out}"))
    }

    #[test]
    fn render_text_shows_fiber_and_salt_rows() {
        let s = summary_with(
            NutrientTotal {
                sum: 12.4,
                unknown: 0,
            },
            NutrientTotal {
                sum: 5.6,
                unknown: 0,
            },
            4,
        );
        let out = render_text(&s, &fixture_goals(), false);
        assert!(row(&out, "Fiber:").contains("12.4"), "got:\n{out}");
        assert!(row(&out, "Salt:").contains("5.6"), "got:\n{out}");
    }

    #[test]
    fn render_text_marks_incomplete_totals_with_plus_and_count() {
        let s = summary_with(
            NutrientTotal {
                sum: 8.4,
                unknown: 9,
            },
            NutrientTotal {
                sum: 5.6,
                unknown: 2,
            },
            12,
        );
        let out = render_text(&s, &fixture_goals(), false);
        let fiber = row(&out, "Fiber:");
        assert!(fiber.contains("8.4+"), "got: {fiber}");
        assert!(fiber.contains("(9 unknown)"), "got: {fiber}");
        assert!(row(&out, "Salt:").contains("(2 unknown)"), "got:\n{out}");
    }

    #[test]
    fn render_text_complete_total_has_no_plus_or_unknown_note() {
        let s = summary_with(
            NutrientTotal {
                sum: 12.4,
                unknown: 0,
            },
            NutrientTotal {
                sum: 5.6,
                unknown: 0,
            },
            4,
        );
        let out = render_text(&s, &fixture_goals(), false);
        let fiber = row(&out, "Fiber:");
        assert!(!fiber.contains('+'), "got: {fiber}");
        assert!(!fiber.contains("unknown"), "got: {fiber}");
    }

    #[test]
    fn render_text_suppresses_under_maximum_check_while_incomplete() {
        // 5.6 g is under a 6 g cap, but 2 unknown entries could push it over.
        let s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 5.6,
                unknown: 2,
            },
            12,
        );
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let salt = row(&render_text(&s, &g, false), "Salt:").to_string();
        assert!(salt.contains("5.6+ / ≤6 g"), "got: {salt}");
        assert!(!salt.contains("under maximum"), "got: {salt}");
        assert!(salt.contains("(2 unknown)"), "got: {salt}");
    }

    #[test]
    fn render_text_keeps_under_maximum_check_when_complete() {
        let s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 5.6,
                unknown: 0,
            },
            12,
        );
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let salt = row(&render_text(&s, &g, false), "Salt:").to_string();
        assert!(salt.contains("✓ under maximum"), "got: {salt}");
    }

    #[test]
    fn render_text_suppresses_within_range_check_while_incomplete() {
        let s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 4.0,
                unknown: 2,
            },
            12,
        );
        let g = goals_with(
            "salt",
            Threshold {
                min: Some(1.0),
                max: Some(6.0),
                target: None,
            },
        );
        let salt = row(&render_text(&s, &g, false), "Salt:").to_string();
        assert!(!salt.contains("within range"), "got: {salt}");
    }

    #[test]
    fn render_text_keeps_over_minimum_check_while_incomplete() {
        // A lower bound already past the minimum proves the minimum is met.
        let s = summary_with(
            NutrientTotal {
                sum: 41.2,
                unknown: 9,
            },
            NutrientTotal::default(),
            12,
        );
        let g = goals_with(
            "fiber",
            Threshold {
                min: Some(35.0),
                max: None,
                target: None,
            },
        );
        let fiber = row(&render_text(&s, &g, false), "Fiber:").to_string();
        assert!(fiber.contains("✓ over minimum"), "got: {fiber}");
        assert!(fiber.contains("(9 unknown)"), "got: {fiber}");
    }

    #[test]
    fn render_text_keeps_above_max_while_incomplete() {
        // Unknown entries can only add, so exceeding the cap is proven.
        let s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 8.5,
                unknown: 2,
            },
            12,
        );
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let salt = row(&render_text(&s, &g, false), "Salt:").to_string();
        assert!(salt.contains("3 above max"), "got: {salt}");
    }

    #[test]
    fn render_text_keeps_below_min_while_incomplete() {
        let s = summary_with(
            NutrientTotal {
                sum: 8.4,
                unknown: 9,
            },
            NutrientTotal::default(),
            12,
        );
        let g = goals_with(
            "fiber",
            Threshold {
                min: Some(35.0),
                max: None,
                target: None,
            },
        );
        let fiber = row(&render_text(&s, &g, false), "Fiber:").to_string();
        assert!(fiber.contains("27 below min"), "got: {fiber}");
    }

    #[test]
    fn render_text_nutrient_row_without_goal_shows_unit() {
        let s = summary_with(
            NutrientTotal {
                sum: 12.4,
                unknown: 0,
            },
            NutrientTotal::default(),
            4,
        );
        // fixture_goals has no fiber threshold.
        let fiber = row(&render_text(&s, &fixture_goals(), false), "Fiber:").to_string();
        assert!(fiber.contains("12.4 g"), "got: {fiber}");
    }

    fn goals_with_keys(keys: &[&str]) -> Goals {
        let mut thresholds = HashMap::new();
        for k in keys {
            thresholds.insert(
                (*k).to_string(),
                Threshold {
                    min: Some(1.0),
                    max: None,
                    target: None,
                },
            );
        }
        Goals {
            thresholds,
            source_path: std::path::PathBuf::from("/tmp/goals.md"),
            present: true,
        }
    }

    fn config_with_metrics(ids: &[&str]) -> Config {
        let mut toml_str = String::from("notes_dir = '/tmp'\n");
        for id in ids {
            toml_str.push_str(&format!("[metrics.{id}]\ndisplay = '{id}'\nunit = 'g'\n"));
        }
        toml::from_str(&toml_str).unwrap()
    }

    #[test]
    fn fiber_and_salt_goals_raise_no_unknown_metric_warning() {
        // The behavior `KNOWN_GOAL_METRICS` exists for: `fiber_min: 35` /
        // `salt_max: 6` in goals.md must not be reported as unknown.
        let warnings = detect_config_warnings(
            &goals_with_keys(&["fiber", "salt"]),
            &config_with_metrics(&[]),
        );
        assert!(warnings.is_empty(), "got: {warnings:?}");
    }

    #[test]
    fn goal_key_with_no_data_source_still_warns() {
        let warnings =
            detect_config_warnings(&goals_with_keys(&["resting_hr"]), &config_with_metrics(&[]));
        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert!(warnings[0].contains("unknown metric `resting_hr`"));
    }

    #[test]
    fn custom_metric_shadowing_a_builtin_nutrient_warns() {
        let warnings =
            detect_config_warnings(&goals_with_keys(&[]), &config_with_metrics(&["salt"]));
        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert!(warnings[0].contains("[metrics.salt]"), "got: {warnings:?}");
        assert!(warnings[0].contains("duplicates"), "got: {warnings:?}");
        // The remedy that looks obvious is the destructive one: the config
        // id doubles as the note frontmatter key, so a rename orphans
        // every `salt:` already written. The warning must say so.
        assert!(warnings[0].contains("orphan"), "got: {warnings:?}");
        assert!(
            warnings[0].contains("metrics.salt.logged_value"),
            "got: {warnings:?}"
        );
        // The hint is the surface a surprised user is actually standing on,
        // and it is the one that carried the pre-fix model longest: "the
        // goal is checked once" is true of rulings and false of what is on
        // screen, where an unproven shortfall sits above a logged row's
        // check. The README says so; so must this.
        assert!(warnings[0].contains("shortfall"), "got: {warnings:?}");
    }

    #[test]
    fn unrelated_custom_metric_does_not_warn() {
        let warnings = detect_config_warnings(
            &goals_with_keys(&["resting_hr"]),
            &config_with_metrics(&["resting_hr"]),
        );
        assert!(warnings.is_empty(), "got: {warnings:?}");
    }

    #[test]
    fn shadowing_custom_metric_does_not_overwrite_the_builtin_json_object() {
        let mut s = fixture_summary();
        s.food.entry_count = 3;
        s.food.salt = NutrientTotal {
            sum: 2.5,
            unknown: 1,
        };
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Salt".into(),
            unit: Some("g".into()),
            value: Some(9.9),
        }];
        let v = render_json(&s, &fixture_goals());
        assert_eq!(v["metrics"]["salt"]["value"], 2.5);
        assert_eq!(v["metrics"]["salt"]["unknown_entries"], 1);
        assert_eq!(v["metrics"]["salt"]["entry_count"], 3);
    }

    #[test]
    fn nutrient_json_carries_entry_count_so_fully_unknown_is_reconstructible() {
        let mut s = fixture_summary();
        s.food.entry_count = 3;
        s.food.fiber = NutrientTotal {
            sum: 0.0,
            unknown: 3,
        };
        let v = render_json(&s, &fixture_goals());
        // value 0.0 with unknown == entry_count means "nothing is known",
        // not "the known entries summed to zero".
        assert_eq!(v["metrics"]["fiber"]["value"], 0.0);
        assert_eq!(v["metrics"]["fiber"]["unknown_entries"], 3);
        assert_eq!(v["metrics"]["fiber"]["entry_count"], 3);
    }

    #[test]
    fn nutrient_row_with_no_coverage_still_shows_the_unknown_count() {
        // The `unknown == entry_count` state — nothing about the day's
        // fiber is known. It has to be driven through `render_text`:
        // `render_nutrient_row` never sees `entry_count`, so a bare
        // `NutrientTotal` cannot express the state at all. The one place
        // the dashboard deliberately diverges from `format_nutrient_total`:
        // it stays numeric rather than saying "fiber unknown", so the `+`
        // and the count have to carry the caveat.
        let s = summary_with(
            NutrientTotal {
                sum: 0.0,
                unknown: 3,
            },
            NutrientTotal::default(),
            3,
        );
        assert_eq!(s.food.fiber.unknown, s.food.entry_count);
        let g = goals_with(
            "fiber",
            Threshold {
                min: Some(35.0),
                max: None,
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        let r = row(&out, "Fiber:");
        assert!(r.contains("Fiber: 0.0+"), "got: {r}");
        assert!(r.contains("(3 unknown)"), "got: {r}");
        assert!(r.contains("below min"), "got: {r}");
    }

    #[test]
    fn zero_coverage_shortfall_is_kept_when_no_metric_shadows_the_row() {
        // The collision rule stands a structural zero's shortfall down so a
        // `[metrics.*]` row can rule in its place. With no such row there
        // is nothing to rule instead, and suppressing here would delete the
        // headline signal on the shape that occurs most: a day of logged
        // food none of which carries fiber, which is what most of the food
        // db still looks like. A running total that cannot say whether you
        // are short is the gap this feature was asked to close.
        //
        // Both shapes of "nothing measured" are checked, because the
        // suppression that has to stay scoped covers both: entries none of
        // which carried the nutrient, and no entries at all.
        //
        // This is also the contract the README's `--json` section states
        // about a structural zero (`verdict` is `"warn"`, not `null`), which
        // `vitalog readme` ships as the agent-facing document. It drifted
        // from the code once already; the assertion below is what keeps the
        // two honest.
        for (fiber, entries) in [
            (
                NutrientTotal {
                    sum: 0.0,
                    unknown: 2,
                },
                2,
            ),
            (NutrientTotal::default(), 0),
        ] {
            let s = summary_with(fiber, NutrientTotal::default(), entries);
            assert!(s.custom_metrics.is_empty(), "no row may shadow this one");
            let g = goals_with(
                "fiber",
                Threshold {
                    min: Some(35.0),
                    max: None,
                    target: None,
                },
            );
            let out = render_text(&s, &g, false);
            let r = row(&out, "Fiber:");
            assert!(r.contains("(35 below min)"), "shortfall dropped: {r}");
            assert!(!r.contains('✓'), "reassurance off an empty sum: {r}");

            // `--json` agrees, as it must for every row.
            assert_eq!(render_json(&s, &g)["metrics"]["fiber"]["verdict"], "warn");
        }
    }

    #[test]
    fn nutrient_row_with_no_coverage_suppresses_a_reassuring_max_check() {
        let s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 0.0,
                unknown: 3,
            },
            3,
        );
        assert_eq!(s.food.salt.unknown, s.food.entry_count);
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        let r = row(&out, "Salt:");
        assert!(r.contains("Salt: 0.0+"), "got: {r}");
        assert!(r.contains("(3 unknown)"), "got: {r}");
        assert!(!r.contains("under maximum"), "got: {r}");
    }

    #[test]
    fn food_derived_row_withholds_a_verdict_it_measured_nothing_for() {
        // No food entries: the total is a structural zero, not an observed
        // one. `is_complete()` is vacuously true there — nothing counted,
        // so nothing unknown — and the row used to collect the green check
        // on the strength of it.
        let s = summary_with(NutrientTotal::default(), NutrientTotal::default(), 0);
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        let r = row(&out, "Salt:");
        assert!(r.contains("Salt: 0.0 / ≤6 g"), "got: {r}");
        assert!(
            !r.contains("under maximum"),
            "verdict computed from zero measurements: {r}"
        );
    }

    #[test]
    fn a_skipped_food_line_makes_the_total_a_lower_bound() {
        // A dropped line's nutrients are missing from the sum in exactly
        // the sense a missing token's are, but `sum_food_section` counts it
        // in neither `entry_count` nor `unknown`, so the row used to report
        // as exact and hand out the green check.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 2.0,
                unknown: 0,
            },
            1,
        );
        s.food.skipped_lines = 1;
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        let r = row(&out, "Salt:");
        assert!(r.contains("Salt: 2.0+"), "got: {r}");
        assert!(
            !r.contains("under maximum"),
            "the unparsed line could carry the rest of the cap: {r}"
        );
        // No per-entry count to attach — the day-scoped hint carries it.
        assert!(!r.contains("unknown"), "got: {r}");
        assert!(
            out.contains("1 food line couldn't be parsed"),
            "got:\n{out}"
        );
    }

    #[test]
    fn a_lower_bound_only_proves_a_lower_bound_claim() {
        // The invariant every verdict path routes through, checked against
        // the wording rather than against a restatement of the code.
        //
        // A lower bound establishes "the true value is at least `sum`", so
        // it settles claims of that same shape and no others. `(n above
        // max)` and `✓ over minimum` are the two verdicts that make one;
        // `(n below min)`, `✓ under maximum` and `✓ within range` each
        // claim the true value is *at most* something, which more entries
        // can always undo. A verdict added to `annotate_value` without a
        // branch here fails this — which is the point, since the last three
        // rounds of review each found one case of the rule missing from one
        // branch.
        let bounds = [None, Some(6.0)];
        let values = [0.0, 5.0, 6.0, 6.5, 35.0, 40.0];
        for min in [None, Some(2.0), Some(35.0)] {
            for max in bounds {
                for target in [None, Some(10.0)] {
                    for v in values {
                        let t = Threshold { min, max, target };
                        let verdict = annotate_value(v, &t, false);
                        let claims_a_lower_bound =
                            verdict.contains("above max") || verdict.contains("over minimum");
                        assert_eq!(
                            lower_bound_proves(v, &t),
                            claims_a_lower_bound,
                            "value {v}, min {min:?}, max {max:?} → verdict `{verdict}`"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn annotation_survival_matches_the_verdict_annotate_value_picks() {
        // What the row *prints* is the proven set plus one deliberate
        // exception, `(n below min)`. Cross-check that against
        // `annotate_value`'s wording over a grid, so a reorder of its
        // branches cannot silently reinstate a reassuring check on a lower
        // bound: survival must hold exactly when there is a verdict at all
        // and it is not one of the two greens a max bound can invalidate.
        // `Some(2.0)` is in the min set so that a *satisfiable* both-bounds
        // threshold appears: paired with `max: 6` it is the only cell where
        // `annotate_value` reaches `✓ within range`, the second of the two
        // verdicts survival is supposed to refuse. Without it that half of
        // the assertion never runs — `min 35 / max 6` cannot be satisfied
        // by any value.
        let bounds = [None, Some(6.0)];
        let values = [0.0, 5.0, 6.0, 6.5, 35.0, 40.0];
        for min in [None, Some(2.0), Some(35.0)] {
            for max in bounds {
                for v in values {
                    let t = Threshold {
                        min,
                        max,
                        target: None,
                    };
                    let verdict = annotate_value(v, &t, false);
                    let reassuring =
                        verdict.contains("under maximum") || verdict.contains("within range");
                    assert_eq!(
                        annotation_survives_unknowns(v, &t),
                        !verdict.is_empty() && !reassuring,
                        "value {v}, min {min:?}, max {max:?} → verdict `{verdict}`"
                    );
                }
            }
        }
    }

    #[test]
    fn small_goal_overage_is_not_rounded_to_a_zero_delta() {
        // `salt_max: 6` is the README's own recommendation, and it is the
        // first built-in bound small enough for an integer delta to
        // degenerate: 6.3 g over a 6 g cap used to print the
        // self-contradictory `(0 above max)` in red.
        let s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 6.3,
                unknown: 0,
            },
            3,
        );
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        let r = row(&out, "Salt:");
        assert!(r.contains("(0.3 above max)"), "got: {r}");
    }

    #[test]
    fn hundredth_gram_overages_do_not_degenerate_either() {
        // Salt is written at two decimals per entry, so a daily total
        // lands on a hundredths grid: 6.03 against `salt_max: 6` is an
        // ordinary value, not a float artifact. One decimal rounds its
        // overage back to the self-contradictory `(0.0 above max)`.
        assert_eq!(format_goal_delta(0.03), "0.03");
        assert_eq!(format_goal_delta(0.3), "0.3");
        assert_eq!(format_goal_delta(2.0), "2");

        let s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 6.03,
                unknown: 0,
            },
            3,
        );
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        let r = row(&out, "Salt:");
        assert!(r.contains("(0.03 above max)"), "got: {r}");
    }

    #[test]
    fn large_goal_deltas_stay_whole_numbers() {
        let s = summary_with(
            NutrientTotal {
                sum: 8.4,
                unknown: 0,
            },
            NutrientTotal::default(),
            3,
        );
        let g = goals_with(
            "fiber",
            Threshold {
                min: Some(35.0),
                max: None,
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        let r = row(&out, "Fiber:");
        assert!(r.contains("(27 below min)"), "got: {r}");
    }

    #[test]
    fn shadowing_custom_row_cannot_grant_the_check_the_builtin_row_withheld() {
        // Same label, same cap, one screen: the food-derived row suppresses
        // `✓ under maximum` because 2.5 g is only a lower bound, so the
        // manually logged row must not hand it back.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 2.5,
                unknown: 1,
            },
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Salt".into(),
            unit: Some("g".into()),
            value: Some(4.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert_eq!(
            out.matches("under maximum").count(),
            0,
            "no row may claim the cap is met:\n{out}"
        );
        // The figure and its threshold still render — only the verdict is
        // withheld.
        assert!(out.contains("Salt: 4 / ≤6 g"), "got:\n{out}");
    }

    #[test]
    fn shadowing_custom_row_cannot_contradict_an_overage_either() {
        // The clash from the other direction: the food-derived total is
        // already over the cap, so a manual estimate reading `✓ under
        // maximum` right beneath `(0.3 above max)` is the same defect.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 6.3,
                unknown: 1,
            },
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Salt".into(),
            unit: Some("g".into()),
            value: Some(4.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert!(out.contains("(0.3 above max)"), "got:\n{out}");
        assert_eq!(
            out.matches("under maximum").count(),
            0,
            "one goal, one verdict:\n{out}"
        );
    }

    #[test]
    fn shadowing_custom_row_withholds_its_verdict_even_with_full_coverage() {
        // The goal is checked once, on the food-derived row. A second
        // verdict for the same goal is redundant when the two agree and
        // misleading when they don't — so full coverage, where that row
        // annotates from a complete measurement, is the case the rule is
        // most clearly right about. (It does depend on coverage: with none,
        // the food-derived row has no claim on the goal at all.)
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 2.5,
                unknown: 0,
            },
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Salt".into(),
            unit: Some("g".into()),
            value: Some(4.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert_eq!(
            out.matches("under maximum").count(),
            1,
            "only the food-derived row annotates:\n{out}"
        );
        assert!(out.contains("Salt: 2.5 / ≤6 g"), "got:\n{out}");
        assert!(out.contains("Salt: 4 / ≤6 g"), "got:\n{out}");
    }

    #[test]
    fn shadowing_custom_row_keeps_its_verdict_when_the_food_row_measured_nothing() {
        // The upgrade path: `[metrics.salt]` was the only way to track
        // salt before this feature, and on a day logged that way and no
        // other, the manual figure is the day's entire salt record. `main`
        // showed it in red against the cap; withholding here would replace
        // a warning with silence — and pair it with a green check the food
        // row has no measurement behind.
        let mut s = summary_with(NutrientTotal::default(), NutrientTotal::default(), 0);
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Salt".into(),
            unit: Some("g".into()),
            value: Some(8.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert!(out.contains("Salt: 8 / ≤6 g"), "got:\n{out}");
        assert!(
            out.contains("(2 above max)"),
            "the day's only salt figure went unchecked:\n{out}"
        );
        assert_eq!(
            out.matches("under maximum").count(),
            0,
            "no row may claim the cap is met on zero measurements:\n{out}"
        );
        // Still exactly one verdict, as on every other day.
        assert_eq!(
            out.matches("above max").count(),
            1,
            "one goal, one verdict:\n{out}"
        );
    }

    #[test]
    fn shadowing_custom_row_keeps_its_verdict_when_no_entry_carried_the_nutrient() {
        // The other shape of "nothing is known": three food entries, none
        // of which carried salt. The food-derived row prints `0.0+` and no
        // verdict, so deferring to it would again leave the manual figure
        // unchecked.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 0.0,
                unknown: 3,
            },
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Salt".into(),
            unit: Some("g".into()),
            value: Some(8.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert!(out.contains("(2 above max)"), "got:\n{out}");
        assert_eq!(
            out.matches("above max").count(),
            1,
            "one goal, one verdict:\n{out}"
        );
    }

    #[test]
    fn shadowing_custom_row_still_withholds_on_a_single_measured_entry() {
        // The exception is "measured nothing", not "measured little". One
        // entry out of three carrying salt is enough for the food-derived
        // row to own the goal check, so the round-2 rule applies in full:
        // its withheld `✓ under maximum` must not come back via the manual
        // row. This is the guard against over-applying the fix above.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 2.5,
                unknown: 2,
            },
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Salt".into(),
            unit: Some("g".into()),
            value: Some(4.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert_eq!(
            out.matches("under maximum").count(),
            0,
            "one measured entry still hands the goal check to the food row:\n{out}"
        );
        assert!(out.contains("Salt: 4 / ≤6 g"), "got:\n{out}");
    }

    #[test]
    fn shadowing_custom_row_still_warns_when_the_food_row_could_not_rule() {
        // The other half of the previous test, and the state the rule kept
        // missing: one measured entry out of two, so the food-derived row
        // owns the goal — but its 2.5 g lower bound is under the cap, so it
        // has no verdict to print. Withholding here too left the screen
        // with no verdict at all on a manually logged 8 g, dropping the red
        // `(2 above max)` `main` showed. Suppression exists to stop a
        // second row handing back *reassurance*; a warning is not that.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 2.5,
                unknown: 1,
            },
            2,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Salt".into(),
            unit: Some("g".into()),
            value: Some(8.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert!(out.contains("Salt: 2.5+ / ≤6 g"), "got:\n{out}");
        assert!(
            out.contains("Salt: 8 / ≤6 g     (2 above max)"),
            "the day's manual figure went unchecked:\n{out}"
        );
        assert_eq!(
            out.matches("above max").count(),
            1,
            "one goal, one verdict:\n{out}"
        );
        assert_eq!(
            out.matches("under maximum").count(),
            0,
            "neither row may claim the cap is met:\n{out}"
        );
    }

    #[test]
    fn full_coverage_withholds_a_check_the_logged_figure_contradicts() {
        // The case the rule was rewritten for. Full coverage summing to
        // 3.5 g under `salt_max: 6` used to print `✓ under maximum` and
        // silence a manually logged 8 g — reassurance the day's own data
        // denies. Full coverage does not mean all the salt is accounted
        // for: salt added while cooking or at the table never reaches the
        // food-derived total, and a restaurant meal logged as one entry
        // under-captures seasoning. The food total is a lower bound even
        // here, so the 8 g may be the *more* complete figure.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 3.5,
                unknown: 0,
            },
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Logged salt".into(),
            unit: Some("g".into()),
            value: Some(8.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert_eq!(
            row(&out, "Salt:"),
            "Salt: 3.5 / ≤6 g  ⚠ logged 8 g vs 3.5 g measured — cannot reconcile",
            "got:\n{out}"
        );
        assert_eq!(
            row(&out, "Logged salt:"),
            "Logged salt: 8 / ≤6 g     (2 above max)",
            "got:\n{out}"
        );
        assert_eq!(
            out.matches("under maximum").count(),
            0,
            "a contradicted `✓` survived:\n{out}"
        );

        // `--json` says the same, so a consumer cannot read the 3.5 as
        // approved.
        let v = render_json(&s, &g);
        assert_eq!(v["metrics"]["salt"]["verdict"], serde_json::Value::Null);
        assert_eq!(v["metrics"]["salt"]["logged_verdict"], "warn");
        assert_eq!(
            v["metrics"]["salt"]["verdict_note"],
            "logged 8 g vs 3.5 g measured — cannot reconcile"
        );
    }

    #[test]
    fn agreeing_figures_print_one_verdict_and_no_note() {
        // The other side of the same rule, and why it keys on the verdicts
        // rather than on the gap between the figures: 3.4 against 3.5 is
        // noise under a cap of 6, and a numeric threshold that fired here
        // would be arbitrary and need re-tuning per goal. Both agree the
        // cap is met, so the food-derived row checks it once and the day
        // stays quiet.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 3.5,
                unknown: 0,
            },
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Logged salt".into(),
            unit: Some("g".into()),
            value: Some(3.4),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert_eq!(row(&out, "Salt:"), "Salt: 3.5 / ≤6 g     ✓ under maximum");
        assert_eq!(row(&out, "Logged salt:"), "Logged salt: 3.4 / ≤6 g");
        assert!(!out.contains("cannot reconcile"), "got:\n{out}");

        let v = render_json(&s, &g);
        assert_eq!(v["metrics"]["salt"]["verdict"], "ok");
        assert_eq!(
            v["metrics"]["salt"]["logged_verdict"],
            serde_json::Value::Null
        );
        assert_eq!(
            v["metrics"]["salt"]["verdict_note"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn disagreement_the_other_way_withholds_the_logged_check() {
        // Same rule, mirrored: the measurement is the one over the cap and
        // the logged figure is the one claiming the day was fine. The
        // warning stays, the contradicted `✓` does not, and the note reads
        // the same either way because it names both numbers.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 7.0,
                unknown: 0,
            },
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Logged salt".into(),
            unit: Some("g".into()),
            value: Some(4.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert_eq!(
            row(&out, "Salt:"),
            "Salt: 7.0 / ≤6 g     (1 above max)  \
             ⚠ logged 4 g vs 7.0 g measured — cannot reconcile",
            "got:\n{out}"
        );
        assert_eq!(row(&out, "Logged salt:"), "Logged salt: 4 / ≤6 g");
        assert_eq!(out.matches("under maximum").count(), 0, "got:\n{out}");
    }

    #[test]
    fn partial_coverage_never_claims_the_figures_cannot_be_reconciled() {
        // It takes two verdicts to disagree, and while the food-derived
        // total is an open lower bound it deliberately gives none. `2.5+`
        // with an entry still unmeasured is not contradicted by a logged
        // 8 g — the missing entry could carry the other 5.5 g — so
        // "cannot reconcile" would be false. Nothing is lost by staying
        // quiet: the food row has already withheld its `✓` for the same
        // incompleteness, and the manual row already prints the warning.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 2.5,
                unknown: 1,
            },
            2,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Logged salt".into(),
            unit: Some("g".into()),
            value: Some(8.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert_eq!(
            row(&out, "Salt:"),
            "Salt: 2.5+ / ≤6 g  (1 unknown)",
            "got:\n{out}"
        );
        assert_eq!(
            row(&out, "Logged salt:"),
            "Logged salt: 8 / ≤6 g     (2 above max)",
            "got:\n{out}"
        );
        assert!(!out.contains("cannot reconcile"), "got:\n{out}");

        let v = render_json(&s, &g);
        assert_eq!(v["metrics"]["salt"]["verdict"], serde_json::Value::Null);
        assert_eq!(v["metrics"]["salt"]["logged_verdict"], "warn");
        assert_eq!(
            v["metrics"]["salt"]["verdict_note"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn a_shortfall_off_a_lower_bound_is_display_not_evidence() {
        // The mirror image of the test above, and the same predicate: a
        // lower bound proves only lower-bound claims. `8.4+` with nine of
        // twelve entries unmeasured under `fiber_min: 35` prints
        // `(27 below min)` — of what was measured, the day is short — but
        // proves nothing about the day's true total, which those nine
        // entries can carry well past 35. Reading the shortfall as a firm
        // verdict made `⚠ … cannot reconcile` fire against a logged 40 g
        // the data agrees with completely, and deleted the `✓` that figure
        // had earned.
        //
        // Both halves matter and they sit on the same screen: the food row
        // keeps the shortfall (suppressing it was the previous round's
        // regression), and the logged row — the only figure that can rule —
        // keeps its check.
        let mut s = summary_with(
            NutrientTotal {
                sum: 8.4,
                unknown: 9,
            },
            NutrientTotal::default(),
            12,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "fiber".into(),
            display: "Logged fiber".into(),
            unit: Some("g".into()),
            value: Some(40.0),
        }];
        let g = goals_with(
            "fiber",
            Threshold {
                min: Some(35.0),
                max: None,
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert_eq!(
            row(&out, "Fiber:"),
            "Fiber: 8.4+ / ≥35 g     (27 below min)  (9 unknown)",
            "got:\n{out}"
        );
        assert_eq!(
            row(&out, "Logged fiber:"),
            "Logged fiber: 40 / ≥35 g     ✓ over minimum",
            "got:\n{out}"
        );
        assert!(
            !out.contains("cannot reconcile"),
            "nine unmeasured entries can hold the missing 31.6 g:\n{out}"
        );

        let v = render_json(&s, &g);
        assert_eq!(v["metrics"]["fiber"]["verdict"], "warn");
        assert_eq!(v["metrics"]["fiber"]["logged_verdict"], "ok");
        assert_eq!(
            v["metrics"]["fiber"]["verdict_note"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn zero_coverage_under_a_min_goal_leaves_one_verdict_on_the_logged_row() {
        // The shape where two verdicts used to coexist: three food entries,
        // none carrying fiber, under `fiber_min: 35`. The food-derived row
        // printed `(35 below min)` off its structural zero and the logged
        // row added its own — two warnings for one goal, one of them
        // computed from no measurement at all. The zero is not a
        // measurement, so it stands down here and the logged figure rules
        // alone. One line, not two.
        //
        // Only here. `zero_coverage_shortfall_is_kept_when_no_metric_
        // shadows_the_row` pins the other half: with nothing to rule in its
        // place, that shortfall stays.
        let mut s = summary_with(
            NutrientTotal {
                sum: 0.0,
                unknown: 3,
            },
            NutrientTotal::default(),
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "fiber".into(),
            display: "Logged fiber".into(),
            unit: Some("g".into()),
            value: Some(20.0),
        }];
        let g = goals_with(
            "fiber",
            Threshold {
                min: Some(35.0),
                max: None,
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert_eq!(
            row(&out, "Fiber:"),
            "Fiber: 0.0+ / ≥35 g  (3 unknown)",
            "got:\n{out}"
        );
        assert_eq!(
            row(&out, "Logged fiber:"),
            "Logged fiber: 20 / ≥35 g     (15 below min)",
            "got:\n{out}"
        );
        // (The fixture's calorie row carries a `below min` of its own, so
        // count only the two rows this goal owns.)
        assert_eq!(
            row(&out, "Fiber:").matches("below min").count()
                + row(&out, "Logged fiber:").matches("below min").count(),
            1,
            "one goal, one verdict:\n{out}"
        );
        assert!(
            !out.contains("cannot reconcile"),
            "a structural zero contradicts nothing:\n{out}"
        );
    }

    #[test]
    fn the_back_catalogue_shape_still_lets_the_logged_figure_rule() {
        // Every `## Food` line written before nutrients were tracked
        // carries none, so for the whole back-catalogue the food-derived
        // total is a structural `0.0+ (n unknown)` and a manually logged
        // figure is the only real number on the day. Whatever the collision
        // rule is, it has to stay sane for a history where the food row can
        // never rule — the manual capability is not deprecated by this.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 0.0,
                unknown: 5,
            },
            5,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Logged salt".into(),
            unit: Some("g".into()),
            value: Some(8.0),
        }];
        let g = goals_with(
            "salt",
            Threshold {
                min: None,
                max: Some(6.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        assert_eq!(
            row(&out, "Salt:"),
            "Salt: 0.0+ / ≤6 g  (5 unknown)",
            "got:\n{out}"
        );
        assert_eq!(
            row(&out, "Logged salt:"),
            "Logged salt: 8 / ≤6 g     (2 above max)",
            "got:\n{out}"
        );

        let v = render_json(&s, &g);
        assert_eq!(v["metrics"]["salt"]["verdict"], serde_json::Value::Null);
        assert_eq!(v["metrics"]["salt"]["logged_verdict"], "warn");
        assert_eq!(v["metrics"]["salt"]["logged_value"], 8.0);
    }

    #[test]
    fn goal_verdict_agrees_with_annotate_value() {
        // `goal_verdict` classifies; `annotate_value` words. The collision
        // rule reads the first and the rows print the second, so the two
        // have to partition the same way — a bound added to one and not the
        // other would silence a row that still prints, or note a
        // disagreement between two figures the screen shows agreeing.
        let bounds = [None, Some(5.0)];
        for min in bounds {
            for max in bounds {
                for target in bounds {
                    let t = Threshold { min, max, target };
                    for v in [0.0, 4.0, 5.0, 6.0, 9.0] {
                        let worded = annotate_value(v, &t, false);
                        let expected = match goal_verdict(v, &t) {
                            GoalVerdict::Reassuring => worded.starts_with('✓'),
                            GoalVerdict::Warning => worded.starts_with('('),
                            GoalVerdict::Silent => worded.is_empty(),
                        };
                        assert!(expected, "{v} vs {t:?} classified against `{worded}`");
                    }
                }
            }
        }
    }

    #[test]
    fn shadowed_row_verdicts_hold_across_the_coverage_matrix() {
        // The shadowing rule has been rewritten four times, each time
        // because one shape of day was left out of the reasoning. Sweep
        // every coverage state against every threshold shape, with the
        // manual figure below, inside, just under and above the bounds, and
        // check the properties the rule exists for:
        //
        //   1. no reassurance from an incomplete food total — while the
        //      food-derived row has measured something but cannot see the
        //      whole day, no row on the screen may print `✓ under maximum`
        //      or `✓ within range`;
        //   2. no warning goes missing — when the food-derived row prints
        //      no verdict at all, a manual figure that breaks the goal must
        //      still say so;
        //   5. the reconciliation note fires on exactly the cells where two
        //      verdicts exist and disagree, and nowhere else — not on a
        //      near-miss like 29 against 30, which is what keying on the
        //      verdicts rather than on the gap buys, and not while the food
        //      total is an open lower bound, where a higher logged figure
        //      is not in conflict with it at all;
        //   6. the figure the disagreement contradicts loses its check, and
        //      the warning is never the one dropped;
        //   7. `--json` blesses exactly what the text surface blessed.
        //
        // The manual row is given a distinct label so the two rows can be
        // told apart; only `id` drives the rule.
        //
        // The food-derived sum is an axis of its own rather than a constant
        // baked into each coverage shape. Holding it at 30 inside a 25–45
        // band leaves the food side never a `Warning`, so half the verdict
        // pairs simply never occur and the sweep covers well under the
        // space it appears to — a green grid proving nothing about the
        // cells it cannot reach. Below min, inside the band, and above max
        // are all reachable now.
        let shapes: [(&str, usize, usize, usize); 5] = [
            // (name, unknown, entry_count, skipped_lines)
            ("no food entries", 0, 0, 0),
            ("entries, none carrying fiber", 3, 3, 0),
            ("partial coverage", 2, 3, 0),
            ("full coverage", 0, 3, 0),
            ("full coverage plus a dropped line", 0, 3, 1),
        ];
        let thresholds = [
            ("min only", Some(25.0), None),
            ("max only", None, Some(45.0)),
            ("both bounds", Some(25.0), Some(45.0)),
        ];
        let reassuring_verdicts = ["under maximum", "within range"];
        let warnings = ["below min", "above max"];
        // Both branches have to be reached, or the sweep proves nothing
        // about either. The food side's own verdicts are counted too,
        // because a sweep that never produces one is the failure mode this
        // grid exists to rule out.
        let mut notes = 0;
        let mut silent = 0;
        let mut food_warned = 0;
        let mut food_reassured = 0;
        let mut unproven = 0;

        for (cov_name, unknown, entries, skipped) in shapes {
            // Nothing measured means nothing summed: a non-zero sum with no
            // measured entry is not a state the parser can produce.
            let measured = entries > unknown;
            let sums: &[f64] = if measured {
                &[10.0, 30.0, 60.0]
            } else {
                &[0.0]
            };
            for &sum in sums {
                let total = NutrientTotal { sum, unknown };
                for (t_name, min, max) in thresholds {
                    for manual in [10.0, 29.0, 30.0, 60.0] {
                        let t = Threshold {
                            min,
                            max,
                            target: None,
                        };
                        let mut s = summary_with(total, NutrientTotal::default(), entries);
                        s.food.skipped_lines = skipped;
                        s.custom_metrics = vec![CustomMetric {
                            id: "fiber".into(),
                            display: "Logged fiber".into(),
                            unit: Some("g".into()),
                            value: Some(manual),
                        }];
                        let g = goals_with("fiber", t.clone());
                        let out = render_text(&s, &g, false);
                        let case =
                            format!("{cov_name} / sum {sum} / {t_name} / manual {manual}:\n{out}");

                        let food_row = row(&out, "Fiber:");
                        let manual_row = row(&out, "Logged fiber:");
                        let food_silent = warnings
                            .iter()
                            .chain(reassuring_verdicts.iter())
                            .chain(["over minimum"].iter())
                            .all(|v| !food_row.contains(v));
                        let printed = |r: &str| {
                            warnings
                                .iter()
                                .chain(reassuring_verdicts.iter())
                                .chain(["over minimum"].iter())
                                .any(|v| r.contains(v))
                        };

                        // 1. Reassurance is never printed off a total that
                        //    measured the day but cannot account for all of it.
                        if total.is_lower_bound(skipped) && entries > total.unknown {
                            for v in reassuring_verdicts {
                                assert!(!out.contains(v), "`{v}` on an incomplete total — {case}");
                            }
                        }

                        // 2. A verdict the food-derived row never gave is not
                        //    the shadowing row's to lose.
                        let manual_verdict = annotate_value(manual, &t, false);
                        if food_silent {
                            for w in warnings {
                                if manual_verdict.contains(w) {
                                    assert!(
                                        manual_row.contains(w),
                                        "manual `{w}` dropped with no verdict anywhere — {case}"
                                    );
                                }
                            }
                        }

                        // 3. One goal never collects two reassuring verdicts.
                        let reassurances: usize = reassuring_verdicts
                            .iter()
                            .map(|v| out.matches(v).count())
                            .sum();
                        assert!(reassurances <= 1, "two reassuring verdicts — {case}");

                        // 4. Only the verdict is ever withheld: the figure and
                        //    its threshold always render.
                        assert!(
                            manual_row.starts_with(&format!("Logged fiber: {}", trim_num(manual))),
                            "manual figure hidden — {case}"
                        );
                        assert!(
                            manual_row.contains(&format_threshold_inline(&t, "g")),
                            "manual threshold hidden — {case}"
                        );

                        // 5. The note tracks the verdicts, not the gap, and it
                        //    takes two verdicts to disagree. The food-derived
                        //    side contributes one only where its total
                        //    *proves* one: never off a structural zero, and
                        //    never while the total is an open lower bound
                        //    whose verdict claims an upper bound on the day.
                        //
                        //    Read off the wording rather than by calling the
                        //    production predicate — a sweep that restates the
                        //    implementation checks nothing about it. `(n above
                        //    max)` and `✓ over minimum` are the two verdicts
                        //    that claim only a lower bound.
                        let worded = annotate_value(total.sum, &t, false);
                        let proven =
                            worded.contains("above max") || worded.contains("over minimum");
                        let food_alone = if !measured || (total.is_lower_bound(skipped) && !proven)
                        {
                            GoalVerdict::Silent
                        } else {
                            goal_verdict(total.sum, &t)
                        };
                        let food_verdict = goal_verdict(total.sum, &t);
                        let logged_verdict = goal_verdict(manual, &t);
                        let expect_note = food_alone != GoalVerdict::Silent
                            && logged_verdict != GoalVerdict::Silent
                            && food_alone != logged_verdict;
                        if expect_note {
                            notes += 1;
                        } else {
                            silent += 1;
                        }
                        match food_alone {
                            GoalVerdict::Warning => food_warned += 1,
                            GoalVerdict::Reassuring => food_reassured += 1,
                            GoalVerdict::Silent => {}
                        }
                        assert_eq!(
                            food_row.contains("cannot reconcile"),
                            expect_note,
                            "note fired on the wrong cell — {case}"
                        );
                        if expect_note {
                            let plus = if total.is_lower_bound(skipped) {
                                "+"
                            } else {
                                ""
                            };
                            assert!(
                                food_row.contains(&format!(
                                    "{} g vs {:.1}{plus} g",
                                    trim_num(manual),
                                    total.sum
                                )),
                                "note does not name both figures — {case}"
                            );
                        }

                        // 6. Of the two, the contradicted reassurance is what
                        //    goes; the warning always stays.
                        let (warned, reassured) = if food_alone == GoalVerdict::Warning {
                            (food_row, manual_row)
                        } else {
                            (manual_row, food_row)
                        };
                        if expect_note {
                            assert!(
                                warnings.iter().any(|w| warned.contains(w)),
                                "the warning was the verdict dropped — {case}"
                            );
                            assert!(
                                !reassured.contains('✓'),
                                "a contradicted `✓` was printed — {case}"
                            );
                        }

                        // 7. `--json` says the same. `verdict` is null on
                        //    exactly the rows the text surface left unblessed,
                        //    so a consumer cannot read a figure as approved
                        //    that the text output refused to approve.
                        let obj = &render_json(&s, &g)["metrics"]["fiber"];
                        let kind = |v: GoalVerdict| {
                            if v == GoalVerdict::Warning {
                                "warn"
                            } else {
                                "ok"
                            }
                        };
                        assert_eq!(
                            obj["verdict"],
                            if printed(food_row) {
                                serde_json::Value::from(kind(food_verdict))
                            } else {
                                serde_json::Value::Null
                            },
                            "json `verdict` disagrees with the food row — {case}"
                        );
                        assert_eq!(
                            obj["logged_verdict"],
                            if printed(manual_row) {
                                serde_json::Value::from(kind(logged_verdict))
                            } else {
                                serde_json::Value::Null
                            },
                            "json `logged_verdict` disagrees with the manual row — {case}"
                        );
                        assert_eq!(
                            obj["verdict_note"].is_null(),
                            !expect_note,
                            "json `verdict_note` disagrees with the text note — {case}"
                        );

                        // 8. Round 5's defect, as a property. A shortfall the
                        //    lower bound cannot prove is display and nothing
                        //    more: the row keeps printing it, it never claims
                        //    a disagreement, and it never takes the goal check
                        //    away from the row that can rule.
                        if measured && total.is_lower_bound(skipped) && worded.contains("below min")
                        {
                            unproven += 1;
                            assert!(
                                food_row.contains("below min"),
                                "the shortfall was suppressed in display — {case}"
                            );
                            assert!(
                                !food_row.contains("cannot reconcile"),
                                "an unproven shortfall claimed a disagreement — {case}"
                            );
                            if annotation_survives_unknowns(manual, &t) {
                                assert!(
                                    printed(manual_row),
                                    "the logged row lost its verdict to a shortfall that \
                                     proves nothing — {case}"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert!(notes > 0 && silent > 0, "sweep hit only one branch");
        // Guards the axis itself: with the food-derived sum pinned inside
        // the goal band, `food_warned` is zero and half the verdict pairs
        // are unreachable, so the grid would pass while testing half of
        // what it claims.
        assert!(
            food_warned > 0 && food_reassured > 0 && unproven > 0,
            "sweep never reached one of the food-side verdicts \
             (warned {food_warned}, reassured {food_reassured}, unproven {unproven})"
        );
    }

    #[test]
    fn skipped_lines_are_reported_in_json() {
        // `unknown_entries == entry_count == 0` is documented to mean an
        // exact total. It isn't one when food lines were dropped, so the
        // count has to be in the object the rule is stated about.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 2.0,
                unknown: 0,
            },
            1,
        );
        s.food.skipped_lines = 2;
        let v = render_json(&s, &fixture_goals());
        assert_eq!(v["metrics"]["salt"]["unknown_entries"], 0);
        assert_eq!(v["metrics"]["salt"]["entry_count"], 1);
        assert_eq!(v["metrics"]["salt"]["skipped_lines"], 2);
        assert_eq!(v["metrics"]["fiber"]["skipped_lines"], 2);
    }

    #[test]
    fn builtin_nutrient_metrics_all_resolve_to_a_total() {
        // The shadowing rule reads the food-derived total through
        // `builtin_nutrient_total`. An id added to the constant but not to
        // that match would shadow nothing and silently keep both verdicts.
        let food = FoodTotals::default();
        for id in BUILTIN_NUTRIENT_METRICS {
            assert!(
                builtin_nutrient_total(id, &food).is_some(),
                "`{id}` is in BUILTIN_NUTRIENT_METRICS with no food-derived total"
            );
        }
    }

    #[test]
    fn builtin_nutrient_metrics_all_have_a_json_object() {
        // `render_json` hand-writes the `fiber` and `salt` objects, then
        // treats every `BUILTIN_NUTRIENT_METRICS` id as one it has already
        // written — a custom metric with such an id is folded into the
        // existing object as `logged_value` instead of being inserted. An
        // id added to the constant without an object here would therefore
        // drop the user's logged metric from `--json` entirely.
        let s = summary_with(NutrientTotal::default(), NutrientTotal::default(), 0);
        let v = render_json(&s, &fixture_goals());
        for id in BUILTIN_NUTRIENT_METRICS {
            assert!(
                v["metrics"][id].is_object(),
                "`{id}` is in BUILTIN_NUTRIENT_METRICS with no `metrics.{id}` object"
            );
        }
    }

    #[test]
    fn builtin_nutrient_metrics_are_all_known_goal_keys() {
        // `KNOWN_GOAL_METRICS` is what stops `fiber_min: 35` in `goals.md`
        // being reported as `unknown metric \`fiber\``. An id added to
        // `BUILTIN_NUTRIENT_METRICS` and not there would get a dashboard
        // row and a `metrics.<id>` object while its own goal key was
        // reported as a config error on every run.
        for id in BUILTIN_NUTRIENT_METRICS {
            assert!(
                KNOWN_GOAL_METRICS.contains(id),
                "`{id}` has a food-derived row but is not a known goal key"
            );
        }
    }

    #[test]
    fn builtin_nutrient_metrics_all_render_a_text_row() {
        // The two pins above cover the total and the JSON object; the text
        // row is named literally in `render_text` and is covered by
        // neither. An id added to the constant, to `builtin_nutrient_total`
        // and to `render_json` passes both of them while producing no row
        // at all — and the collision rule would then withhold a
        // `[metrics.*]` row's verdict in favour of a row that is not on
        // screen.
        let s = summary_with(NutrientTotal::default(), NutrientTotal::default(), 0);
        let out = render_text(&s, &fixture_goals(), false);
        for id in BUILTIN_NUTRIENT_METRICS {
            let mut chars = id.chars();
            let label: String = match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            };
            assert!(
                out.lines().any(|l| l.starts_with(&format!("{label}:"))),
                "`{id}` is in BUILTIN_NUTRIENT_METRICS with no `{label}:` row in:\n{out}"
            );
        }
    }

    #[test]
    fn unrelated_custom_row_annotation_is_never_withheld() {
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 2.5,
                unknown: 1,
            },
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "resting_hr".into(),
            display: "Resting HR".into(),
            unit: Some("bpm".into()),
            value: Some(52.0),
        }];
        let g = goals_with(
            "resting_hr",
            Threshold {
                min: None,
                max: Some(65.0),
                target: None,
            },
        );
        let out = render_text(&s, &g, false);
        let r = row(&out, "Resting HR:");
        assert!(r.contains("under maximum"), "got: {r}");
    }

    #[test]
    fn shadowing_custom_metric_value_is_reachable_in_json() {
        // The built-in keeps the `metrics.salt` slot so `unknown_entries` /
        // `entry_count` are always present, but the manually logged figure
        // must not be dropped on the floor — it was the only salt number
        // this config had before the feature existed.
        let mut s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 2.5,
                unknown: 1,
            },
            3,
        );
        s.custom_metrics = vec![CustomMetric {
            id: "salt".into(),
            display: "Salt".into(),
            unit: Some("g".into()),
            value: Some(4.0),
        }];
        let v = render_json(&s, &fixture_goals());
        assert_eq!(v["metrics"]["salt"]["value"], 2.5);
        assert_eq!(v["metrics"]["salt"]["unknown_entries"], 1);
        assert_eq!(v["metrics"]["salt"]["entry_count"], 3);
        assert_eq!(v["metrics"]["salt"]["logged_value"], 4.0);
        assert_eq!(v["metrics"]["salt"]["logged_unit"], "g");
    }

    #[test]
    fn json_nutrient_object_has_no_logged_value_without_a_collision() {
        let s = summary_with(
            NutrientTotal::default(),
            NutrientTotal {
                sum: 2.5,
                unknown: 1,
            },
            3,
        );
        let v = render_json(&s, &fixture_goals());
        assert!(v["metrics"]["salt"]["logged_value"].is_null());
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
    fn the_skip_diagnostics_name_the_dropped_lines() {
        // The hint and the `--json` warning render the same sentence as
        // `Today so far:` does, from `FoodTotals::skipped_note`. Asserted
        // on both surfaces here because three hand-written copies of one
        // sentence is what drifts.
        let mut s = fixture_summary();
        s.food.skipped_lines = 2;
        s.food.skipped_times = vec!["12:00".into(), "19:30".into()];
        let g = fixture_goals();
        let out = render_text(&s, &g, false);
        assert!(
            out.contains("(2 food lines couldn't be parsed (12:00, 19:30))"),
            "got:\n{out}"
        );
        let v = render_json(&s, &g);
        assert!(
            v["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str() == Some("2 food lines couldn't be parsed (12:00, 19:30)")),
            "got: {}",
            v["warnings"]
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
    fn render_json_includes_fiber_and_salt_with_unknown_counts() {
        let s = summary_with(
            NutrientTotal {
                sum: 8.4,
                unknown: 9,
            },
            NutrientTotal {
                sum: 5.6,
                unknown: 0,
            },
            12,
        );
        let g = goals_with(
            "fiber",
            Threshold {
                min: Some(35.0),
                max: None,
                target: None,
            },
        );
        let v = render_json(&s, &g);
        assert_eq!(v["metrics"]["fiber"]["value"], 8.4);
        assert_eq!(v["metrics"]["fiber"]["min"], 35.0);
        assert_eq!(v["metrics"]["fiber"]["unknown_entries"], 9);
        assert_eq!(v["metrics"]["salt"]["value"], 5.6);
        assert_eq!(v["metrics"]["salt"]["unknown_entries"], 0);
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

    fn due_reminder(
        display: &str,
        days_since: i64,
        last_done: &str,
        streak: Option<u32>,
        days_past_due: Option<i64>,
    ) -> crate::reminders::EvaluatedReminder {
        crate::reminders::EvaluatedReminder {
            id: display.to_lowercase().replace(' ', "_"),
            display: display.into(),
            interval_days: 2,
            last_done: Some(chrono::NaiveDate::parse_from_str(last_done, "%Y-%m-%d").unwrap()),
            days_since: Some(days_since),
            due: true,
            not_before: None,
            not_after: None,
            streak,
            days_past_due,
        }
    }

    #[test]
    fn reminders_block_shows_alive_streak_on_due_line() {
        let r = due_reminder("Lactic acid training", 2, "2026-05-05", Some(6), Some(0));
        let out = render_reminders_block(&[r], false);
        assert!(
            out.contains("Lactic acid training — due today · 🔥 6-day streak (keep it alive)"),
            "got: {out}"
        );
    }

    #[test]
    fn reminders_block_shows_days_past_due_when_enabled() {
        // Broken streak (Some(0)), past due by 3, toggle on.
        let r = due_reminder("Deadlifts", 5, "2026-05-01", Some(0), Some(3));
        let out = render_reminders_block(&[r], false);
        assert!(out.contains("Deadlifts — 3 days past due"), "got: {out}");
    }

    #[test]
    fn reminders_block_shows_singular_day_past_due() {
        // Broken streak (Some(0)), past due by exactly 1 → singular wording.
        let r = due_reminder("Deadlifts", 3, "2026-05-01", Some(0), Some(1));
        let out = render_reminders_block(&[r], false);
        assert!(out.contains("Deadlifts — 1 day past due"), "got: {out}");
        assert!(!out.contains("1 days past due"), "got: {out}");
    }

    #[test]
    fn reminders_block_falls_back_to_overdue_when_toggles_off() {
        // No streak, no days_past_due → existing wording.
        let r = due_reminder("Zone 2", 4, "2026-05-05", None, None);
        let out = render_reminders_block(&[r], false);
        assert!(
            out.contains("Zone 2 — overdue (4 days ago, 2026-05-05)"),
            "got: {out}"
        );
    }

    #[test]
    fn reminders_block_never_logged_unchanged() {
        let r = crate::reminders::EvaluatedReminder {
            id: "weigh_in".into(),
            display: "Daily weigh-in".into(),
            interval_days: 1,
            last_done: None,
            days_since: None,
            due: true,
            not_before: None,
            not_after: None,
            streak: Some(0),
            days_past_due: None,
        };
        let out = render_reminders_block(&[r], false);
        assert!(out.contains("Daily weigh-in — never logged"), "got: {out}");
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
