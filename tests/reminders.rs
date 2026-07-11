//! End-to-end tests for the [reminders] feature.

use chrono::{NaiveDate, NaiveTime};
use vitalog::cli::status_cmd;
use vitalog::cli::today_cmd::{
    assemble, render_json_with_reminders, render_reminders_block, render_text,
};
use vitalog::config::Config;
use vitalog::db;
use vitalog::goals::load_goals;
use vitalog::modules;
use vitalog::reminders::{evaluate, load_reminders, to_json};

fn setup_with_reminders(reminders_toml: &str) -> (tempfile::TempDir, Config) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().display().to_string().replace('\\', "/");
    let toml_str = format!(
        r#"
notes_dir = "{path}"
time_format = "24h"
weight_unit = "kg"

[metrics]
la_min = {{ display = "Lactic acid (min)", color = "red", unit = "min" }}

[exercises]
deadlift = {{ display = "Deadlift", color = "yellow" }}

{reminders_toml}
"#
    );
    let config: Config = toml::from_str(&toml_str).unwrap();
    (dir, config)
}

fn write_note(notes_dir: &std::path::Path, date: &str, body: &str) {
    std::fs::write(notes_dir.join(format!("{date}.md")), body).unwrap();
}

#[test]
fn today_text_shows_overdue_lactic_acid_reminder() {
    let (dir, config) = setup_with_reminders(
        r#"
[reminders.lactic_acid]
display = "Lactic acid training"
interval_days = 2
watch = "metric"
target = "la_min"
"#,
    );

    // Last LA session was 3 days before our test "today".
    write_note(
        dir.path(),
        "2026-05-09",
        "---\ndate: 2026-05-09\nla_min: 15\n---\n\n## Food\n",
    );

    let registry = modules::build_registry(&config);
    let conn = db::open_rw(&config.db_path()).unwrap();
    db::init_db(&conn, &registry).unwrap();
    modules::validate_module_tables(&registry).unwrap();
    vitalog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry).unwrap();

    let date = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
    let summary = assemble(date, &config, &conn).unwrap();
    let goals = load_goals(&config.notes_dir_path()).unwrap();
    let reminders = load_reminders(&config).unwrap();
    let eval = evaluate(
        &conn,
        date,
        NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        &reminders,
        &config,
    )
    .unwrap();

    let block = render_reminders_block(&eval.reminders, false);
    let body = render_text(&summary, &goals, false);

    assert!(block.contains("Reminders"), "got:\n{block}");
    assert!(block.contains("Lactic acid training"), "got:\n{block}");
    assert!(block.contains("3 days ago"), "got:\n{block}");
    assert!(block.contains("2026-05-09"), "got:\n{block}");

    // Combined: reminder block precedes date header.
    let combined = format!("{block}{body}");
    let header_idx = combined.find("2026-05-12 — Daily summary").unwrap();
    let rem_idx = combined.find("Lactic acid training").unwrap();
    assert!(rem_idx < header_idx, "got:\n{combined}");
}

#[test]
fn today_text_silent_when_no_reminder_due() {
    let (dir, config) = setup_with_reminders(
        r#"
[reminders.lactic_acid]
display = "Lactic acid training"
interval_days = 2
watch = "metric"
target = "la_min"
"#,
    );

    // Logged today → not due.
    write_note(
        dir.path(),
        "2026-05-12",
        "---\ndate: 2026-05-12\nla_min: 15\n---\n\n## Food\n",
    );

    let registry = modules::build_registry(&config);
    let conn = db::open_rw(&config.db_path()).unwrap();
    db::init_db(&conn, &registry).unwrap();
    modules::validate_module_tables(&registry).unwrap();
    vitalog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry).unwrap();

    let date = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
    let reminders = load_reminders(&config).unwrap();
    let eval = evaluate(
        &conn,
        date,
        NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        &reminders,
        &config,
    )
    .unwrap();

    let block = render_reminders_block(&eval.reminders, false);
    assert!(block.is_empty(), "expected empty block, got:\n{block:?}");
}

#[test]
fn today_json_includes_all_reminders_including_not_due() {
    let (dir, config) = setup_with_reminders(
        r#"
[reminders.lactic_acid]
display = "Lactic acid training"
interval_days = 2
watch = "metric"
target = "la_min"

[reminders.weigh_in]
display = "Daily weigh-in"
interval_days = 1
watch = "day_field"
target = "weight"
"#,
    );

    // LA done today (not due), weight never logged (due).
    write_note(
        dir.path(),
        "2026-05-12",
        "---\ndate: 2026-05-12\nla_min: 15\n---\n\n## Food\n",
    );

    let registry = modules::build_registry(&config);
    let conn = db::open_rw(&config.db_path()).unwrap();
    db::init_db(&conn, &registry).unwrap();
    modules::validate_module_tables(&registry).unwrap();
    vitalog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry).unwrap();

    let date = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
    let summary = assemble(date, &config, &conn).unwrap();
    let goals = load_goals(&config.notes_dir_path()).unwrap();
    let reminders = load_reminders(&config).unwrap();
    let eval = evaluate(
        &conn,
        date,
        NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        &reminders,
        &config,
    )
    .unwrap();

    let v = render_json_with_reminders(&summary, &goals, &eval.reminders, &eval.warnings);
    let arr = v["reminders"].as_array().unwrap();
    assert_eq!(arr.len(), 2);

    let la = arr.iter().find(|r| r["id"] == "lactic_acid").unwrap();
    assert_eq!(la["due"], false);
    assert_eq!(la["last_done"], "2026-05-12");
    assert_eq!(la["days_since"], 0);

    let weigh = arr.iter().find(|r| r["id"] == "weigh_in").unwrap();
    assert_eq!(weigh["due"], true);
    assert!(weigh["last_done"].is_null());
}

#[test]
fn status_json_includes_reminders_after_sync() {
    let (dir, config) = setup_with_reminders(
        r#"
[reminders.lactic_acid]
display = "Lactic acid training"
interval_days = 2
watch = "metric"
target = "la_min"
"#,
    );

    write_note(
        dir.path(),
        "2026-05-09",
        "---\ndate: 2026-05-09\nla_min: 15\n---\n\n## Food\n",
    );

    let registry = modules::build_registry(&config);
    let conn = db::open_rw(&config.db_path()).unwrap();
    db::init_db(&conn, &registry).unwrap();
    modules::validate_module_tables(&registry).unwrap();
    vitalog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry).unwrap();
    let v = status_cmd::assemble_status(&conn, &config, &registry).unwrap();

    let arr = v["reminders"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let r = &arr[0];
    assert_eq!(r["id"], "lactic_acid");
    assert_eq!(r["display"], "Lactic acid training");
    assert_eq!(r["interval_days"], 2);
    assert!(r["due"].is_boolean(), "due should be a boolean, got: {r}");
    assert!(
        r["days_since"].is_number() || r["days_since"].is_null(),
        "days_since should be number or null, got: {r}"
    );
    assert!(
        r["last_done"].is_string() || r["last_done"].is_null(),
        "last_done should be string or null, got: {r}"
    );
    // reminder_warnings is always an array
    assert!(v["reminder_warnings"].is_array(), "got:\n{v}");
}

#[test]
fn today_text_silent_before_gate_visible_inside_window() {
    let (dir, config) = setup_with_reminders(
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

    // Logged 3 days ago — data-overdue.
    write_note(
        dir.path(),
        "2026-05-09",
        "---\ndate: 2026-05-09\nla_min: 1\n---\n\n## Food\n",
    );

    let registry = modules::build_registry(&config);
    let conn = db::open_rw(&config.db_path()).unwrap();
    db::init_db(&conn, &registry).unwrap();
    modules::validate_module_tables(&registry).unwrap();
    vitalog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry).unwrap();

    let date = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap();
    let reminders = load_reminders(&config).unwrap();

    // Before window — 10:00 wall-clock.
    let eval_before = evaluate(
        &conn,
        date,
        NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
        &reminders,
        &config,
    )
    .unwrap();
    let block_before = render_reminders_block(&eval_before.reminders, false);
    assert!(
        block_before.is_empty(),
        "block should be silent before gate; got:\n{block_before}"
    );

    // Inside window — 19:00 wall-clock.
    let eval_in = evaluate(
        &conn,
        date,
        NaiveTime::from_hms_opt(19, 0, 0).unwrap(),
        &reminders,
        &config,
    )
    .unwrap();
    let block_in = render_reminders_block(&eval_in.reminders, false);
    assert!(
        block_in.contains("Brush teeth (evening)"),
        "block should show inside gate; got:\n{block_in}"
    );
}

#[test]
fn streak_and_days_past_due_flow_end_to_end() {
    let (dir, config) = setup_with_reminders(
        r#"
[reminder_defaults]
show_streak = true
show_days_past_due = true

[reminders.lactic_acid]
display = "Lactic acid training"
interval_days = 2
watch = "metric"
target = "la_min"
"#,
    );

    // interval 2, logged May 1/3/5 (an unbroken every-other-day chain).
    for date in ["2026-05-01", "2026-05-03", "2026-05-05"] {
        write_note(
            dir.path(),
            date,
            &format!("---\ndate: {date}\nla_min: 15\n---\n\n## Food\n"),
        );
    }

    let registry = modules::build_registry(&config);
    let conn = db::open_rw(&config.db_path()).unwrap();
    db::init_db(&conn, &registry).unwrap();
    modules::validate_module_tables(&registry).unwrap();
    vitalog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry).unwrap();

    let reminders = load_reminders(&config).unwrap();
    // "today" = May 6 → done yesterday, streak alive and credited through the 6th.
    let today = NaiveDate::from_ymd_opt(2026, 5, 6).unwrap();
    let noon = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
    let result = evaluate(&conn, today, noon, &reminders, &config).unwrap();

    let r = &result.reminders[0];
    assert_eq!(r.streak, Some(6));
    assert_eq!(r.days_past_due, Some(0));

    let (arr, _) = to_json(&result.reminders, &result.warnings);
    let obj = &arr.as_array().unwrap()[0];
    assert_eq!(obj["streak"], serde_json::json!(6));
    assert_eq!(obj["days_past_due"], serde_json::json!(0));
}
