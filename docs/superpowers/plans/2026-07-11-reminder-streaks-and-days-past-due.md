# Reminder Streaks & Days-Past-Due Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-reminder `streak` and `days_past_due` signals to vitalog's reminder system, exposed in the reminders JSON and the `vitalog today` text block, configurable globally and per-reminder.

**Architecture:** Streak and days-past-due are derived on the fly from existing completion dates — no new DB state. `load_reminders` resolves the two config toggles (global default + per-reminder override) once; `evaluate` fetches the full descending date history per reminder, computes the streak via a pure function, and populates two new `Option` fields on `EvaluatedReminder`. The shared `to_json` builder and `render_reminders_block` surface them.

**Tech Stack:** Rust, rusqlite (bundled SQLite), serde/toml, chrono, color-eyre.

**Design spec:** `docs/superpowers/specs/2026-07-11-reminder-streaks-and-days-past-due-design.md` — read the "Streak semantics" section before Task 2.

## Global Constraints

- No `.unwrap()` / `.expect()` in library code (`src/**`, excluding `#[cfg(test)]`). Use `color_eyre::Result`. `.unwrap()` is fine inside tests.
- `rustfmt` + `clippy` clean. Verify with `just lint` (`cargo fmt --check && cargo clippy`).
- Programming in American English (identifiers, comments).
- Streak semantics (verbatim from spec): `streak = min(today, last_done + interval_days − 1) − run_start + 1`, valid while `days_since ≤ interval_days`, else `0`. `run_start` = start of the maximal completion chain where every consecutive gap is ≤ `interval_days`.
- `days_past_due = max(0, days_since − interval_days)`; `null` when the toggle is off or the reminder was never logged.
- Config defaults: `show_streak` ON, `show_days_past_due` OFF, both when `[reminder_defaults]` is absent and when a field within it is absent.
- Per-reminder toggle wins over the global default; absent per-reminder key falls back to the global default.

---

## Task 1: Config structs for the toggles

**Files:**
- Modify: `src/config.rs` (add `ReminderDefaultsConfig`; add `reminder_defaults` to `Config`; add two `Option<bool>` fields to `ReminderConfig`)

**Interfaces:**
- Produces:
  - `pub struct ReminderDefaultsConfig { pub show_streak: bool, pub show_days_past_due: bool }` with a manual `Default` (`show_streak: true`, `show_days_past_due: false`).
  - `Config.reminder_defaults: ReminderDefaultsConfig`
  - `ReminderConfig.show_streak: Option<bool>`, `ReminderConfig.show_days_past_due: Option<bool>`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `src/config.rs`:

```rust
#[test]
fn reminder_defaults_absent_section_is_streak_on_past_due_off() {
    let cfg: Config = toml::from_str(r#"notes_dir = "/tmp/x""#).unwrap();
    assert!(cfg.reminder_defaults.show_streak);
    assert!(!cfg.reminder_defaults.show_days_past_due);
}

#[test]
fn reminder_defaults_and_overrides_parse() {
    let cfg: Config = toml::from_str(
        r#"
notes_dir = "/tmp/x"

[reminder_defaults]
show_streak = false
show_days_past_due = true

[reminders.foo]
display = "Foo"
interval_days = 1
watch = "day_field"
target = "weight"
show_streak = true
"#,
    )
    .unwrap();
    assert!(!cfg.reminder_defaults.show_streak);
    assert!(cfg.reminder_defaults.show_days_past_due);
    let foo = &cfg.reminders["foo"];
    assert_eq!(foo.show_streak, Some(true));
    assert_eq!(foo.show_days_past_due, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib config::tests::reminder_defaults 2>&1 | tail -20`
Expected: FAIL — compile error, `no field reminder_defaults on type Config` (or `no field show_streak on ReminderConfig`).

- [ ] **Step 3: Add the struct and fields**

In `src/config.rs`, add the new struct next to the other config structs (e.g. after `ReminderConfig`):

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ReminderDefaultsConfig {
    #[serde(default = "default_true")]
    pub show_streak: bool,
    #[serde(default)]
    pub show_days_past_due: bool,
}

impl Default for ReminderDefaultsConfig {
    fn default() -> Self {
        Self {
            show_streak: true,
            show_days_past_due: false,
        }
    }
}
```

Add the field to `Config` (next to `reminders`):

```rust
    #[serde(default)]
    pub reminder_defaults: ReminderDefaultsConfig,
```

Add the two override fields to `ReminderConfig` (after `not_after`):

```rust
    #[serde(default)]
    pub show_streak: Option<bool>,
    #[serde(default)]
    pub show_days_past_due: Option<bool>,
```

(`default_true` already exists in this file.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib config::tests::reminder_defaults 2>&1 | tail -20`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(reminders): config toggles for streak and days-past-due"
```

---

## Task 2: `compute_streak` pure function

**Files:**
- Modify: `src/reminders.rs` (add `compute_streak` + unit tests)

**Interfaces:**
- Produces: `pub fn compute_streak(dates_desc: &[NaiveDate], today: NaiveDate, interval_days: u32) -> u32`
  - `dates_desc`: distinct qualifying completion dates, most-recent first.
  - Returns 0 when there is no live streak (empty, most-recent in the future, or broken).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `src/reminders.rs`:

```rust
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
    let dates = vec![d("2026-05-07"), d("2026-05-05"), d("2026-05-03"), d("2026-05-01")];
    assert_eq!(compute_streak(&dates, d("2026-05-07"), 2), 7);
}

#[test]
fn streak_logging_early_earns_nothing_extra() {
    // interval 2, done May 1/3/5/6, today 6th → still 6.
    let dates = vec![d("2026-05-06"), d("2026-05-05"), d("2026-05-03"), d("2026-05-01")];
    assert_eq!(compute_streak(&dates, d("2026-05-06"), 2), 6);
}

#[test]
fn streak_daily_behaves_like_duolingo() {
    // interval 1, done May 1..5.
    let dates = vec![d("2026-05-05"), d("2026-05-04"), d("2026-05-03"), d("2026-05-02"), d("2026-05-01")];
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib reminders::tests::streak 2>&1 | tail -20`
Expected: FAIL — compile error, `cannot find function compute_streak`.

- [ ] **Step 3: Implement `compute_streak`**

Add this public function in `src/reminders.rs` (place it near `evaluate`, before the `#[cfg(test)]` module):

```rust
/// Length of the current streak in *days*, per the cadence model in
/// `docs/superpowers/specs/2026-07-11-reminder-streaks-and-days-past-due-design.md`.
///
/// `dates_desc` are the distinct qualifying completion dates, most-recent
/// first. Returns 0 when there is no live streak: empty history, the most
/// recent completion is in the future, or the streak has broken
/// (`today - last_done > interval_days`).
pub fn compute_streak(dates_desc: &[NaiveDate], today: NaiveDate, interval_days: u32) -> u32 {
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
    // capped at today — the streak never counts into the future.
    let credited_end = std::cmp::min(today, last + chrono::Duration::days(interval - 1));
    ((credited_end - run_start).num_days() + 1) as u32
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib reminders::tests::streak 2>&1 | tail -20`
Expected: PASS (all seven tests).

- [ ] **Step 5: Commit**

```bash
git add src/reminders.rs
git commit -m "feat(reminders): compute_streak cadence-aware day counter"
```

---

## Task 3: Resolve toggles onto `Reminder`

**Files:**
- Modify: `src/reminders.rs` (`Reminder` struct, `load_reminders`, and all existing `Reminder { .. }` literals in the test module)

**Interfaces:**
- Consumes: `ReminderDefaultsConfig`, `ReminderConfig.show_streak/show_days_past_due` (Task 1).
- Produces: `Reminder.show_streak: bool`, `Reminder.show_days_past_due: bool` (resolved).

- [ ] **Step 1: Write the failing test**

Add to `src/reminders.rs` tests:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib reminders::tests::load_resolves_streak 2>&1 | tail -20`
Expected: FAIL — compile error, `missing fields show_streak, show_days_past_due in initializer of Reminder` (the struct has no such fields yet).

- [ ] **Step 3: Add the fields and resolution**

In `src/reminders.rs`, add to the `Reminder` struct (after `not_after`):

```rust
    pub show_streak: bool,
    pub show_days_past_due: bool,
```

In `load_reminders`, resolve before `out.push(Reminder { .. })` and add the two fields to the initializer:

```rust
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
```

- [ ] **Step 4: Fix every existing `Reminder { .. }` literal in the test module**

Adding required fields breaks all test constructors. Add `show_streak: false, show_days_past_due: false,` to each `Reminder { .. }` literal in the `#[cfg(test)]` module. There are two kinds:

- The helper builders: `metric_reminder`, `session_text_reminder`, `session_num_reminder`, `lift_reminder`, `day_field_reminder`.
- The inline literal in `evaluate_metric_zero_value_counts_when_opted_in`.

For example, `metric_reminder` becomes:

```rust
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
```

Apply the same two-field addition to the other four helpers and the inline literal.

Verify none are missed:

Run: `cargo test --lib reminders:: 2>&1 | grep -E "missing field|error\[" | head`
Expected: no output (all literals fixed).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib reminders:: 2>&1 | tail -20`
Expected: PASS (new resolution test plus all pre-existing reminder tests).

- [ ] **Step 6: Commit**

```bash
git add src/reminders.rs
git commit -m "feat(reminders): resolve streak toggles onto Reminder"
```

---

## Task 4: History query + evaluate populates streak & days_past_due

**Files:**
- Modify: `src/reminders.rs` (replace `query_last_done` with `query_logged_dates`; add fields to `EvaluatedReminder`; wire `evaluate`)

**Interfaces:**
- Consumes: `compute_streak` (Task 2), `Reminder.show_streak/show_days_past_due` (Task 3).
- Produces: `EvaluatedReminder.streak: Option<u32>`, `EvaluatedReminder.days_past_due: Option<i64>`; private `fn query_logged_dates(conn, watch) -> Result<Vec<NaiveDate>>` (descending).

- [ ] **Step 1: Write the failing tests**

Add to `src/reminders.rs` tests (these reuse existing helpers `make_test_db`, `insert_metric`, `metric_reminder`, `empty_config`, `noon`):

```rust
fn metric_reminder_with_toggles(
    id: &str,
    interval_days: u32,
    metric: &str,
    show_streak: bool,
    show_days_past_due: bool,
) -> Reminder {
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
        show_streak,
        show_days_past_due,
    }
}

#[test]
fn evaluate_populates_streak_when_enabled() {
    let conn = make_test_db();
    // interval 2, logged May 1/3/5.
    for date in ["2026-05-01", "2026-05-03", "2026-05-05"] {
        insert_metric(&conn, date, "la_min", 15.0);
    }
    let today = NaiveDate::from_ymd_opt(2026, 5, 6).unwrap();
    let r = metric_reminder_with_toggles("la", 2, "la_min", true, false);
    let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
    assert_eq!(result.reminders[0].streak, Some(6));
    assert_eq!(result.reminders[0].days_past_due, None); // toggle off
}

#[test]
fn evaluate_streak_none_when_disabled() {
    let conn = make_test_db();
    insert_metric(&conn, "2026-05-05", "la_min", 15.0);
    let today = NaiveDate::from_ymd_opt(2026, 5, 5).unwrap();
    let r = metric_reminder_with_toggles("la", 2, "la_min", false, false);
    let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
    assert_eq!(result.reminders[0].streak, None);
}

#[test]
fn evaluate_days_past_due_positive_when_overdue() {
    let conn = make_test_db();
    insert_metric(&conn, "2026-05-05", "la_min", 15.0);
    // interval 2, today is 4 days later → days_since 4, past due by 2.
    let today = NaiveDate::from_ymd_opt(2026, 5, 9).unwrap();
    let r = metric_reminder_with_toggles("la", 2, "la_min", true, true);
    let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
    assert_eq!(result.reminders[0].days_past_due, Some(2));
    assert_eq!(result.reminders[0].streak, Some(0)); // broken
}

#[test]
fn evaluate_days_past_due_zero_when_on_track() {
    let conn = make_test_db();
    insert_metric(&conn, "2026-05-05", "la_min", 15.0);
    let today = NaiveDate::from_ymd_opt(2026, 5, 6).unwrap(); // days_since 1, interval 2
    let r = metric_reminder_with_toggles("la", 2, "la_min", true, true);
    let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
    assert_eq!(result.reminders[0].days_past_due, Some(0));
}

#[test]
fn evaluate_days_past_due_none_when_never_logged() {
    let conn = make_test_db();
    let today = NaiveDate::from_ymd_opt(2026, 5, 6).unwrap();
    let r = metric_reminder_with_toggles("la", 2, "la_min", true, true);
    let result = evaluate(&conn, today, noon(), &[r], &empty_config()).unwrap();
    assert_eq!(result.reminders[0].last_done, None);
    assert_eq!(result.reminders[0].days_past_due, None); // no baseline
    assert_eq!(result.reminders[0].streak, Some(0));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib reminders::tests::evaluate_populates_streak 2>&1 | tail -20`
Expected: FAIL — compile error, `no field streak on EvaluatedReminder`.

- [ ] **Step 3: Add fields to `EvaluatedReminder`**

In `src/reminders.rs`, add to the `EvaluatedReminder` struct (after `not_after`):

```rust
    pub streak: Option<u32>,
    pub days_past_due: Option<i64>,
```

- [ ] **Step 4: Replace `query_last_done` with `query_logged_dates`**

Delete the existing `fn query_last_done(..)` and replace it with:

```rust
/// All distinct dates on which the watched thing was logged, most-recent
/// first. Column names come from the closed watch enum (never user input),
/// so `format!`-ing them into SQL is safe — the same guarantee the previous
/// `query_last_done` relied on.
fn query_logged_dates(conn: &Connection, watch: &WatchSource) -> Result<Vec<NaiveDate>> {
    use color_eyre::eyre::WrapErr;

    let rows: Vec<String> = match watch {
        WatchSource::Metric {
            id,
            count_zero_as_logged,
        } => {
            let zero_flag: i64 = if *count_zero_as_logged { 1 } else { 0 };
            let mut stmt = conn.prepare(
                "SELECT DISTINCT date FROM metrics \
                 WHERE name = ?1 AND (value > 0 OR ?2 = 1) ORDER BY date DESC",
            )?;
            let it = stmt
                .query_map(rusqlite::params![id, zero_flag], |row| {
                    row.get::<_, String>(0)
                })?;
            it.collect::<rusqlite::Result<Vec<String>>>()
                .wrap_err("Failed to query metrics for reminder")?
        }
        WatchSource::Session(SessionMatch::TextEquals { column, value }) => {
            let sql = format!(
                "SELECT DISTINCT date FROM sessions WHERE {} = ?1 ORDER BY date DESC",
                column.sql_column()
            );
            let mut stmt = conn.prepare(&sql)?;
            let it = stmt.query_map([value], |row| row.get::<_, String>(0))?;
            it.collect::<rusqlite::Result<Vec<String>>>()
                .wrap_err("Failed to query sessions for reminder (text-equals)")?
        }
        WatchSource::Session(SessionMatch::NumericAtLeast { column, min }) => {
            let sql = format!(
                "SELECT DISTINCT date FROM sessions \
                 WHERE {col} IS NOT NULL AND {col} >= ?1 ORDER BY date DESC",
                col = column.sql_column()
            );
            let mut stmt = conn.prepare(&sql)?;
            let it = stmt.query_map(rusqlite::params![min], |row| row.get::<_, String>(0))?;
            it.collect::<rusqlite::Result<Vec<String>>>()
                .wrap_err("Failed to query sessions for reminder (numeric-at-least)")?
        }
        WatchSource::Lift {
            exercise,
            min_weight,
            min_reps,
        } => {
            let mut sql = String::from("SELECT DISTINCT date FROM lift_sets WHERE exercise = ?1");
            let mut params: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::Text(exercise.clone())];
            if let Some(w) = min_weight {
                sql.push_str(&format!(" AND weight_lbs >= ?{}", params.len() + 1));
                params.push(rusqlite::types::Value::Real(*w));
            }
            if let Some(rp) = min_reps {
                sql.push_str(&format!(" AND reps >= ?{}", params.len() + 1));
                params.push(rusqlite::types::Value::Integer(*rp as i64));
            }
            sql.push_str(" ORDER BY date DESC");
            let params_refs: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
            let mut stmt = conn.prepare(&sql)?;
            let it = stmt.query_map(params_refs.as_slice(), |row| row.get::<_, String>(0))?;
            it.collect::<rusqlite::Result<Vec<String>>>()
                .wrap_err("Failed to query lift_sets for reminder")?
        }
        WatchSource::DayField(col) => {
            let sql = format!(
                "SELECT DISTINCT date FROM days WHERE {} IS NOT NULL ORDER BY date DESC",
                col.sql_column()
            );
            let mut stmt = conn.prepare(&sql)?;
            let it = stmt.query_map([], |row| row.get::<_, String>(0))?;
            it.collect::<rusqlite::Result<Vec<String>>>()
                .wrap_err("Failed to query days for reminder")?
        }
    };
    Ok(rows
        .iter()
        .filter_map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .collect())
}
```

- [ ] **Step 5: Wire `evaluate` to use it and populate the new fields**

In `evaluate`, replace the body of the `for r in reminders` loop. The current version starts with `let last_done = query_last_done(conn, &r.watch)?;`. Replace that line and the `out.push(EvaluatedReminder { .. })` initializer so the loop reads:

```rust
    for r in reminders {
        let dates = query_logged_dates(conn, &r.watch)?;
        let last_done = dates.first().copied();
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
        let streak = if r.show_streak {
            Some(compute_streak(&dates, today, r.interval_days))
        } else {
            None
        };
        let days_past_due = if r.show_days_past_due {
            days_since.map(|n| (n - r.interval_days as i64).max(0))
        } else {
            None
        };
        out.push(EvaluatedReminder {
            id: r.id.clone(),
            display: r.display.clone(),
            interval_days: r.interval_days,
            last_done,
            days_since,
            due,
            not_before: r.not_before,
            not_after: r.not_after,
            streak,
            days_past_due,
        });
    }
```

- [ ] **Step 6: Fix the module doc-comment reference to `query_last_done`**

The module header comment (top of `src/reminders.rs`) says "`evaluate` returns the most recent date the watched thing was logged". Leave the behavior description, but if any comment names `query_last_done` specifically, update it to `query_logged_dates`. Search:

Run: `grep -n "query_last_done" src/reminders.rs`
Expected: no output after the rename (fix any stragglers).

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test --lib reminders:: 2>&1 | tail -20`
Expected: PASS (new evaluate tests plus all pre-existing reminder tests — `last_done`/`days_since`/`due` behavior is unchanged).

- [ ] **Step 8: Commit**

```bash
git add src/reminders.rs
git commit -m "feat(reminders): evaluate streak and days-past-due from history"
```

---

## Task 5: Expose streak & days_past_due in JSON

**Files:**
- Modify: `src/reminders.rs` (`to_json`)

**Interfaces:**
- Consumes: `EvaluatedReminder.streak/days_past_due` (Task 4).
- Produces: `"streak"` and `"days_past_due"` keys in each reminder JSON object (integer or `null`).

- [ ] **Step 1: Write the failing test**

Add to `src/reminders.rs` tests:

```rust
#[test]
fn to_json_includes_streak_and_days_past_due() {
    let r = EvaluatedReminder {
        id: "la".into(),
        display: "LA".into(),
        interval_days: 2,
        last_done: Some(NaiveDate::from_ymd_opt(2026, 5, 5).unwrap()),
        days_since: Some(1),
        due: false,
        not_before: None,
        not_after: None,
        streak: Some(6),
        days_past_due: Some(0),
    };
    let (arr, _warns) = to_json(&[r], &[]);
    let obj = &arr.as_array().unwrap()[0];
    assert_eq!(obj["streak"], serde_json::json!(6));
    assert_eq!(obj["days_past_due"], serde_json::json!(0));
}

#[test]
fn to_json_null_when_toggles_off() {
    let r = EvaluatedReminder {
        id: "la".into(),
        display: "LA".into(),
        interval_days: 2,
        last_done: Some(NaiveDate::from_ymd_opt(2026, 5, 5).unwrap()),
        days_since: Some(1),
        due: false,
        not_before: None,
        not_after: None,
        streak: None,
        days_past_due: None,
    };
    let (arr, _warns) = to_json(&[r], &[]);
    let obj = &arr.as_array().unwrap()[0];
    assert!(obj["streak"].is_null());
    assert!(obj["days_past_due"].is_null());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib reminders::tests::to_json_includes 2>&1 | tail -20`
Expected: FAIL — assertion failure: `obj["streak"]` is `null` (key absent) instead of `6`.

- [ ] **Step 3: Add the keys to `to_json`**

In `to_json`, extend the per-reminder `serde_json::json!({ .. })` object with two keys (after `"not_after"`):

```rust
                "streak": r.streak,
                "days_past_due": r.days_past_due,
```

(`serde_json` serializes `Option::None` as `null` and `Some(n)` as the number.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib reminders::tests::to_json 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/reminders.rs
git commit -m "feat(reminders): emit streak and days-past-due in reminders JSON"
```

---

## Task 6: Enrich the `today` reminders text block

**Files:**
- Modify: `src/cli/today_cmd.rs` (`render_reminders_block`)

**Interfaces:**
- Consumes: `EvaluatedReminder.streak/days_past_due` (Task 4).
- Produces: enriched due-line strings (behavior described below). Block still returns `""` when nothing is due.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `src/cli/today_cmd.rs`. If a helper to build an `EvaluatedReminder` already exists there, reuse it; otherwise add this local builder plus the tests:

```rust
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
    fn reminders_block_falls_back_to_overdue_when_toggles_off() {
        // No streak, no days_past_due → existing wording.
        let r = due_reminder("Zone 2", 4, "2026-05-05", None, None);
        let out = render_reminders_block(&[r], false);
        assert!(out.contains("Zone 2 — overdue (4 days ago, 2026-05-05)"), "got: {out}");
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib today_cmd::tests::reminders_block 2>&1 | tail -20`
Expected: FAIL — the streak/past-due assertions fail (current code emits only "overdue (...)"), plus a compile error if the existing `EvaluatedReminder` literals in this test module lack the new fields.

- [ ] **Step 3: Fix any existing `EvaluatedReminder` literals in this test module**

If `src/cli/today_cmd.rs` tests already build `EvaluatedReminder { .. }` literals, add `streak: None, days_past_due: None,` to each so they compile.

Run: `grep -n "EvaluatedReminder {" src/cli/today_cmd.rs`
Expected: every literal listed here includes the two new fields after this step.

- [ ] **Step 4: Enrich the render loop**

In `render_reminders_block`, replace the `let line = match (r.days_since, r.last_done) { .. }` block with:

```rust
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
```

Note: a due reminder can only have an alive streak (`streak >= 1`) on the plateau day (`days_since == interval_days`); once broken the streak is `Some(0)` and this falls through to the days-past-due or overdue wording — which is why the streak branch comes first.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib today_cmd::tests::reminders_block 2>&1 | tail -20`
Expected: PASS (all four).

- [ ] **Step 6: Commit**

```bash
git add src/cli/today_cmd.rs
git commit -m "feat(reminders): show streak and days-past-due in today block"
```

---

## Task 7: Integration tests + README docs

**Files:**
- Modify: `tests/reminders.rs` (end-to-end assertion over a seeded note)
- Modify: `README.md` (Reminders section)

**Interfaces:**
- Consumes: everything above (config parse → evaluate → JSON → text).

- [ ] **Step 1: Write the failing integration test**

Add to `tests/reminders.rs` (reuses the file's `setup_with_reminders`, `write_note`, and existing imports):

```rust
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
    for m in &registry {
        m.schema(&conn).unwrap();
    }
    db::sync_all(&conn, &config, &registry).unwrap();

    let reminders = load_reminders(&config).unwrap();
    // "today" = May 6 → done yesterday, streak alive and credited through the 6th.
    let today = NaiveDate::from_ymd_opt(2026, 5, 6).unwrap();
    let noon = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
    let result = evaluate(&conn, today, noon, &reminders, &config).unwrap();

    let r = &result.reminders[0];
    assert_eq!(r.streak, Some(6));
    assert_eq!(r.days_past_due, Some(0));

    let (arr, _) = vitalog::reminders::to_json(&result.reminders, &result.warnings);
    let obj = &arr.as_array().unwrap()[0];
    assert_eq!(obj["streak"], serde_json::json!(6));
    assert_eq!(obj["days_past_due"], serde_json::json!(0));
}
```

Note: mirror the exact DB-setup calls (`registry`, `schema`, `sync_all`, `open_rw`) from a nearby existing test in `tests/reminders.rs` — if the helper names differ there, copy that test's setup verbatim and only change the assertions. Add `use vitalog::reminders::to_json;` to the imports if you prefer the unqualified form.

- [ ] **Step 2: Run test to verify it fails, then passes**

Run: `cargo test --test reminders streak_and_days_past_due_flow_end_to_end 2>&1 | tail -20`
Expected: after Tasks 1–6 are implemented this should PASS immediately (it exercises already-built behavior). If it fails on setup-helper names, align them with the neighboring test as noted, then re-run to green.

- [ ] **Step 3: Update the README Reminders section**

In `README.md`, in the "## Reminders" section, after the time-of-day-gates paragraph and before the "A reminder fires when..." paragraph, add:

````markdown
**Streaks & days past due.** Vitalog tracks how many days in a row you've kept a
reminder's cadence (a *streak*), and — opt-in — how far behind you are when you
slip (*days past due*). Both are configurable globally and per-reminder:

```toml
[reminder_defaults]
show_streak        = true      # default ON  (the motivating feature)
show_days_past_due = false     # default OFF (opt-in; can demotivate)

[reminders.lactic_acid]
display            = "Lactic acid training"
interval_days      = 2
watch              = "metric"
target             = "la_min"
show_days_past_due = true       # per-reminder override wins over the default
```

Each completion earns `interval_days` days of streak credit (the day you did it
plus the next `interval_days − 1` days), counted up to today. For an
every-other-day habit done on the 1st/3rd/5th, the streak reads 5 on the 5th and
6 on the 6th; it plateaus at 6 on the 7th until you log again (which makes it 7),
and resets to 0 once you're more than `interval_days` behind. Daily habits get no
free day, so they behave like a classic day-streak. `days_past_due` is
`max(0, days_since − interval_days)`.

Both `vitalog today --json` and `vitalog status` include `streak` and
`days_past_due` on every reminder object (integer, or `null` when the toggle is
off — and `days_past_due` is `null` for a never-logged reminder, which has no
baseline). Handy for a notification script that phrases "🔥 6-day streak — don't
break it now."
````

- [ ] **Step 4: Full verification**

Run: `just lint && cargo test 2>&1 | tail -25`
Expected: fmt clean, clippy clean, all tests pass (unit + `tests/reminders.rs`).

- [ ] **Step 5: Commit**

```bash
git add tests/reminders.rs README.md
git commit -m "test(reminders): end-to-end streak flow + document the feature"
```

---

## Self-Review Notes

- **Spec coverage:** config defaults/overrides (Tasks 1, 3) · streak formula & table (Task 2) · days_past_due incl. never-logged null (Task 4) · history query (Task 4) · JSON fields incl. null semantics (Task 5) · `today` option-A rendering (Task 6) · README + integration (Task 7). No manual CHANGELOG (semantic-release), matching the spec.
- **Type consistency:** `compute_streak(&[NaiveDate], NaiveDate, u32) -> u32`; `EvaluatedReminder.streak: Option<u32>` / `days_past_due: Option<i64>`; `Reminder.show_streak/show_days_past_due: bool`; `ReminderConfig.show_streak/show_days_past_due: Option<bool>`; `ReminderDefaultsConfig.show_streak/show_days_past_due: bool`. Names identical across tasks.
- **Compile-break call-outs:** adding required fields to `Reminder` (Task 3 Step 4) and `EvaluatedReminder` (Task 6 Step 3) breaks existing test literals — both are explicitly enumerated with a grep check.
- **Behavior preservation:** `last_done`, `days_since`, `due`, and warnings are computed exactly as before; `query_logged_dates` reuses the same `WHERE` clauses and column-whitelist safety argument as the removed `query_last_done`.
```

