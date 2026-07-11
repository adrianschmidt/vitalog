# Reminder streaks & days-past-due

## Summary

Add two per-reminder motivational signals to vitalog's reminder system:

- **Streak** — how many days in a row you've kept up a habit's cadence, in the
  spirit of Duolingo ("you've been doing great, don't break your streak now").
- **Days past due** — how far behind you are on a habit you've let slip.

Both are computed by vitalog and exposed in the reminders JSON (consumed by the
external ntfy push-notification script) and reflected in the `vitalog today`
text block. Both are configurable, globally and per-reminder.

There is **no ntfy code in this repo** — push notifications are an external
script that pipes the `reminders` array from `vitalog status` / `vitalog today
--json` into ntfy. This feature's job is to *compute and expose* the numbers;
the external script decides how to phrase them.

## Motivation

Habits with rhythms already fit vitalog's reminder model (`interval_days`).
Surfacing a running streak turns "you haven't done X" (a nag) into "you're on a
6-day streak, keep it alive" (an incentive). Days-past-due is the honest mirror
for when a habit has lapsed — but it can demotivate, so it is opt-in.

## Streak semantics

A reminder fires on a cadence of `interval_days`. The streak generalizes
Duolingo's day-count to that cadence.

**Rule:** each time you do the thing, you earn `interval_days` days of streak
credit — the day you did it plus the next `interval_days − 1` days. The streak
value is the number of credited days in the current unbroken run, counted only
up to today (never into the future).

Worked example — lactic-acid reminder, `interval_days = 2`, done on May 1/3/5:

| Today | Last done          | Streak | Why                                    |
|-------|--------------------|--------|----------------------------------------|
| 5th   | 5th                | 5      | days 1–5 credited                      |
| 6th   | 5th                | 6      | the 5th bought the 5th *and* 6th       |
| 7th   | 5th                | 6      | 7th not yet credited — plateaus at 6   |
| 7th   | 7th (just logged)  | 7      | logging the 7th credits it             |
| 6th   | 6th (logged early) | 6      | over-doing earns nothing extra         |
| 8th   | 5th                | 0      | streak broke (`days_since` 3 > 2)      |

**Alive vs. plateau vs. broken:**

- The streak is **alive** while `days_since ≤ interval_days`. On the last such
  day (`days_since == interval_days`) the streak is alive but its value has
  **plateaued** — that is exactly the day the reminder is "due" and
  `days_past_due` is still 0. Doing the work that day converts the plateau into
  the next day's value.
- The streak is **broken** once `days_since > interval_days`; the value resets
  to 0.

**Daily habits** (`interval_days = 1`) get no free day — each completion credits
only its own day — so the streak behaves like classic Duolingo.

**Formula:**

```
streak = min(today, last_done + interval_days − 1) − run_start + 1
         (valid while days_since ≤ interval_days; otherwise 0)
```

where `run_start` is the beginning of the maximal chain of completion dates in
which every consecutive gap is ≤ `interval_days`. Two consecutive completions
`a < b` stay in the same chain iff `b − a ≤ interval_days` (`b` falls within the
alive window `a` opened).

Reference algorithm (dates are distinct qualifying completion dates, descending):

```
fn compute_streak(dates_desc, today, interval) -> u32 {
    let last = dates_desc.first()?;              // none → 0
    let days_since = (today - last).num_days();
    if days_since < 0 || days_since > interval { return 0; }  // future / broken
    let mut run_start = last;
    let mut prev = last;
    for d in &dates_desc[1..] {
        if (prev - d).num_days() <= interval { run_start = d; prev = d; }
        else { break; }
    }
    let credited_end = min(today, last + (interval - 1) days);
    (credited_end - run_start).num_days() as u32 + 1
}
```

## Days-past-due semantics

`days_past_due = max(0, days_since − interval_days)`.

It is the mirror image of the streak: 0 while the streak is alive, and
`1, 2, 3…` exactly when the streak has broken. The two numbers never disagree —
one is always zero.

**Never-logged** reminders have no baseline completion date, so
`days_past_due` is `null` even when the feature is enabled. (The `days` a
never-done habit is "past due" is undefined; the existing "never logged"
messaging covers that case.) The streak of a never-logged reminder is `0`.

## Configuration

`[reminders]` is already a map of reminder-id → definition, so global scalar
defaults cannot live under it (TOML would read them as a reminder named
`show_streak`). Global defaults get their own section.

```toml
# Global defaults — apply to every reminder unless overridden
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

**Resolution:** a per-reminder key wins; if absent, the `[reminder_defaults]`
value applies; if `[reminder_defaults]` is absent entirely, `show_streak`
defaults ON and `show_days_past_due` defaults OFF.

`show_streak = false` → that reminder computes/exposes no streak (`null` in
JSON, nothing in the `today` text). Same for `show_days_past_due`.

## Architecture

### `config.rs`

- New struct:
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  pub struct ReminderDefaultsConfig {
      #[serde(default = "default_true")]
      pub show_streak: bool,
      #[serde(default)]
      pub show_days_past_due: bool,
  }
  impl Default for ReminderDefaultsConfig {
      fn default() -> Self { Self { show_streak: true, show_days_past_due: false } }
  }
  ```
  Manual `Default` so an absent section yields streak-on / past-due-off (a
  derived `Default` would give `false` for `show_streak`).
- `Config` gains `#[serde(default)] pub reminder_defaults: ReminderDefaultsConfig`.
- `ReminderConfig` gains `#[serde(default)] pub show_streak: Option<bool>` and
  `#[serde(default)] pub show_days_past_due: Option<bool>`.

### `reminders.rs`

- `Reminder` gains resolved `show_streak: bool` and `show_days_past_due: bool`.
  `load_reminders` combines `config.reminder_defaults` with each reminder's
  optional override, so the fallback logic lives in one place and `evaluate`
  just reads booleans.
- Replace `query_last_done` (returns `MAX(date)`) with `query_logged_dates`
  returning `Vec<NaiveDate>` sorted **descending**, reusing the exact same
  per-watch `WHERE` clauses (the closed-enum column whitelist is unchanged, so
  the SQL-injection-safety argument still holds). `last_done` becomes
  `dates.first().copied()`. Datasets are personal-scale, so fetching full
  history per reminder is cheap; the streak walk stops at the first
  chain-breaking gap regardless.
- `EvaluatedReminder` gains:
  - `streak: Option<u32>` — `Some(compute_streak(...))` when `show_streak`,
    else `None`. `Some(0)` for enabled-but-broken/never-logged.
  - `days_past_due: Option<i64>` — `Some(max(0, days_since − interval))` when
    `show_days_past_due` **and** `last_done.is_some()`, else `None`.
- `evaluate` computes both from the fetched date list; `days_since`, `due`, and
  the time-window gate are unchanged.

### JSON (`reminders::to_json`)

Single shared builder used by both `today --json` and `status`. Each reminder
object gains:

```json
"streak": <int|null>,
"days_past_due": <int|null>
```

Consumers show the streak when `> 0` and days-past-due when `> 0`; `null` means
the feature is disabled for that reminder (or, for `days_past_due`, no baseline).

### `today` text (`render_reminders_block`, option A)

The block stays **silent unless something is due**. Due lines are enriched:

- Alive streak (streak `Some(n)`, `n ≥ 1`) → e.g.
  `- Lactic acid training — due today · 🔥 6-day streak (keep it alive)`.
- Past due with `show_days_past_due` on → e.g. `- Deadlifts — 3 days past due`.
- Past due with the toggle off → existing `overdue (N days ago, <date>)` wording.
- Never-logged → existing `never logged` wording (unchanged).

Exact strings are pinned by tests. Ordering of the due block is unchanged
(never-logged first, then `days_since` descending).

## Data flow

```
config.toml
  [reminder_defaults] + [reminders.*].show_* overrides
        │  load_reminders resolves → Reminder{ show_streak, show_days_past_due }
        ▼
reminders::evaluate(conn, today, now, reminders, config)
        │  query_logged_dates (DISTINCT date DESC per watch)
        │  → last_done, days_since, due, streak, days_past_due
        ▼
EvaluatedReminder
    ├── reminders::to_json → "streak" / "days_past_due"  → status / today --json → ntfy script
    └── render_reminders_block (option A)                → vitalog today text
```

## Testing

- **`reminders.rs` unit tests:**
  - Streak formula: every row of the worked-example table (interval 2) plus the
    daily (interval 1) cases; broken, never-logged, single-completion,
    logged-early, and gap-just-breaks (`gap == interval` chains,
    `gap == interval + 1` breaks) cases.
  - Config resolution: global default applied; per-reminder override wins;
    absent `[reminder_defaults]` → streak-on/past-due-off.
  - `days_past_due`: on-track → 0, past due → positive, never-logged → `None`,
    disabled → `None`.
  - `streak`/`days_past_due` null semantics when toggles are off.
- **`tests/reminders.rs` integration:** end-to-end evaluate over a seeded DB
  across the four watch kinds, asserting streak and days-past-due.
- **`today` rendering:** assert the enriched due-line strings for alive-streak,
  past-due-on, past-due-off, and never-logged.

## Docs

- README "Reminders" section: document `[reminder_defaults]`, the per-reminder
  `show_streak` / `show_days_past_due` overrides, the streak-semantics table,
  and the new `streak` / `days_past_due` JSON fields.
- No manual CHANGELOG edit — semantic-release derives it from the
  `feat(reminders): …` commit.

## Out of scope / YAGNI

- No "you lost your N-day streak" transition event (would require persisting the
  previous streak; the reset to 0 plus `days_past_due` already convey the lapse).
- No streak display for reminders that are on-track in the `today` text
  (option A keeps the block a to-do list, not a status panel).
- No changes to the ntfy script itself (external to this repo).
- No new streak/past-due state stored in the DB — both are derived on the fly
  from existing completion dates.
```

