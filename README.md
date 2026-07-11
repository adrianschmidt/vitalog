# vitalog

[![CI](https://github.com/adrianschmidt/vitalog/actions/workflows/ci.yml/badge.svg)](https://github.com/adrianschmidt/vitalog/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/vitalog.svg)](https://crates.io/crates/vitalog)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

> Originally forked from [tfolkman/daylog](https://github.com/tfolkman/daylog).

A terminal dashboard that tracks your life from markdown notes.

## Install

```bash
cargo install vitalog
```

Or download a pre-built binary from [GitHub Releases](https://github.com/adrianschmidt/vitalog/releases).

## Quick Start

```bash
vitalog init
vitalog
```

Two commands to a working dashboard. No API keys, no Docker, no config files to write.

## What It Does

vitalog reads your daily markdown notes (one per day, `YYYY-MM-DD.md`) and renders a live terminal dashboard. Edit a note, save it, see the TUI update in real time.

```yaml
---
date: 2026-03-28
sleep: "10:30pm-6:15am"
weight: 173.4
mood: 4
energy: 3
type: lifting
lifts:
  squat: 185x5, 205x3, 225x1
  pullup: BWx8, BWx6
resting_hr: 52
---

## Notes

Hit a squat PR today.
```

## Three Tiers of Extensibility

### Tier 1: Track any number (config only)

```toml
[metrics]
resting_hr = { display = "Resting HR", color = "red", unit = "bpm" }
```

Add a YAML field, get a sparkline. Zero code.

### Tier 2: Track any exercise (config only)

```toml
[exercises]
turkish_getup = { display = "Turkish Getup", color = "cyan" }
```

Training tab shows it. Trends tab shows 1RM progression. Zero code.

### Tier 3: Build a module (code required)

For domains needing custom tables and visualization. The climbing module is the reference implementation — one directory, one trait, one line in the registry.

## Reminders

Habits with rhythms ("do X every other day") don't fit phone alarms — skip a day and the alarms drift out of phase. Vitalog can watch the data you already log and remind you at the top of `vitalog today` when something hasn't been done recently.

```toml
[reminders.lactic_acid]
display       = "Lactic acid training"
interval_days = 2                                       # every other day
watch         = "metric"
target        = "la_min"

[reminders.zone2]
display       = "Zone 2 cardio"
interval_days = 3
watch         = "session"
target        = { field = "zone2_min", min_value = 1 }

[reminders.deadlifts]
display       = "Heavy deadlifts"
interval_days = 7
watch         = "lift"
target        = { exercise = "deadlift", min_weight = 200 }

[reminders.weigh_in]
display       = "Daily weigh-in"
interval_days = 1
watch         = "day_field"
target        = "weight"
```

Each reminder picks one of four `watch` kinds:

- **`metric`** — a custom metric from `[metrics]`. By default `value > 0` counts as "logged"; set `count_zero_as_logged = true` if 0 is a real reading you want to count.
- **`session`** — a row in the training-sessions table. Text columns (`type`, `block`, `vo2_intervals`) use `equals = "..."`; numeric columns (`duration`, `rpe`, `zone2_min`, `hr_avg`, `week`) use `min_value = N`.
- **`lift`** — a row in `lift_sets`. Requires `exercise`; optional `min_weight` (lbs) and `min_reps` narrow the match.
- **`day_field`** — one of `weight`, `sleep_hours`, `mood`, `energy`, `sleep_start`, `sleep_end`. Any non-null value counts as "logged".

**Time-of-day gates.** Each reminder accepts optional `not_before` and `not_after` fields (24-hour `"HH:MM"`). When set, the reminder only counts as due inside the `[not_before, not_after]` window — so an evening-task reminder doesn't nag at breakfast. Both bounds are independently optional. For overnight reminders, split into two reminders on the same metric (one with `not_after`, the other with `not_before`) — explicit wrap-around windows are rejected at config-load.

```toml
[reminders.brush_evening]
display       = "Brush teeth (evening)"
interval_days = 1
not_before    = "18:00"
not_after     = "23:00"
watch         = "metric"
target        = "brushed_evening"
```

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

A reminder fires when the most recent matching date is either absent or at least `interval_days` calendar days before today (respecting `day_start_hour`). The block is silent when nothing is due. Both `vitalog today --json` and `vitalog status` always include a `reminders` array (every configured reminder, due or not) plus a `reminder_warnings` sibling — handy for piping into a notification script.

## CLI

```bash
vitalog                          # Launch the TUI
vitalog log weight 173.4         # Log a value (no quotes needed)
vitalog log lift squat 185x5     # Log a lift
vitalog log sleep 10:30pm-6:15am # Log sleep
vitalog log metric resting_hr 52 # Log a custom metric
vitalog sleep-start              # Record bedtime (uses now, or pass a time)
vitalog sleep-end                # Finalize sleep entry on today's note
vitalog status --json            # Today's data as JSON
vitalog today                    # Compact daily summary (food, weight, sleep, BP, metrics)
vitalog today 2026-04-29 --json  # Summary for a past date as JSON
vitalog edit                     # Open today's note in $EDITOR
vitalog sync                     # Sync DB without launching TUI
vitalog rebuild                  # Rebuild DB from all notes
```

### Sleep across midnight

`vitalog sleep-start` and `vitalog sleep-end` automate the past-midnight
date math (sleep is recorded on the file for the day you wake up):

```bash
vitalog sleep-start              # before bed (or: vitalog sleep-start 22:30)
vitalog sleep-end                # after waking (or: vitalog sleep-end 06:15)
# → writes `sleep: "10:30pm-6:15am"` to today's note
```

The pending bedtime lives in a `.vitalog-state.toml` sidecar next to the
database (in `notes_dir`). If you sync `notes_dir` across machines via
git/Dropbox/iCloud, add `.vitalog-state.toml` to your ignore list — the
sidecar is per-machine state and is not designed for cross-device sync.

### Logging food, notes, and BP from the CLI

Three top-level subcommands append timestamped entries to the
`## Food`, `## Notes`, and `## Vitals` sections of the day's note,
auto-inserting the section if it's missing.

```bash
# Food — nutrition-db lookup with gram or ml amount
vitalog food "tomato soup" 500g
vitalog food "whole milk" 250ml

# Food — total-panel foods need no amount
vitalog food tea
vitalog food protein-shake

# Food — one-off custom item, all four macros required together;
# --gi / --gl / --ii independently optional. GL auto-computes when
# GI and carbs are both known.
vitalog food --kcal 350 --protein 7 --carbs 24 --fat 25 \
            --gi 50 "Random pasta dish" 500g

# Note — literal text or a [notes.aliases] key
vitalog note "Adderall 10mg"
vitalog note med-morning

# BP — sys dia pulse; auto-picks bp_morning_* or bp_evening_*
# based on the measurement time vs. the 14:00 cutoff. --morning /
# --evening override.
vitalog bp 141 96 70
vitalog bp --evening 133 73 62

# Shared flags: --date YYYY-MM-DD and --time HH:MM (or H:MMam/pm)
# for retroactive entries.
vitalog note --date 2026-04-29 --time 23:30 "Allegra 10mg"
vitalog bp --time 08:00 141 96 70   # logged at 14:30 — still morning
```

After each successful logging command, vitalog echoes the line that was
written and (for `food`) the day's running macro totals, so you can
verify the right alias matched and see your remaining budget without
running `vitalog today`:

```
$ vitalog food "protein shake" 462g
Food logged: 2026-05-02 12:02
  - **12:02** Protein shake (462g) (231 kcal, 47.0g protein, 2.4g carbs, 3.6g fat)

Today so far: 1340 kcal, 95g protein, 50g carbs, 60g fat
```

Use `--quiet` (or `-q`) for a single-line confirmation, e.g. when
bulk-logging from scripts:

```
$ vitalog -q food tea
Food logged: 2026-05-02 14:30 Te, Earl Grey, hot
```

`[notes.aliases]` in `config.toml` lets you map short keys to
longer note text:

```toml
[notes.aliases]
med-morning = "Morning meds (Vyvanse 70mg, Lexapro 20mg, Losartan/HCTZ 100/12.5mg, Allegra 10mg)"
```

These commands write the markdown only; the watcher re-materializes
the database within ~500 ms.

### Daily summary

`vitalog today [date]` prints a compact summary for the day — food
totals (kcal/protein/carbs/fat from the `## Food` section), morning
weight, sleep, morning BP, and any custom metrics — with optional
goal comparison from `goals.md`. Add `--json` for machine-readable
output suitable for AI agents and scripts.

```bash
vitalog today                    # today's summary
vitalog today 2026-04-29         # any past date
vitalog today --json             # JSON for tooling
```

### Trend charts

`vitalog trend <field> [days]` prints a chart of recent values for any
DB-resident field. Useful when daily fluctuation hides the underlying trend.

```bash
vitalog trend weight              # 14-day ASCII chart
vitalog trend weight 30           # 30-day window
vitalog trend weight --compact    # one-line sparkline
vitalog trend resting_hr --json   # structured output
```

Built-in fields: `weight`, `sleep_hours`, `mood`, `energy`. Anything in your
`[metrics]` config also works.

## Goals

Goals live in `goals.md` in your notes directory. The body is
free-form (notes, derivations, history); the YAML frontmatter at the
top defines the numeric thresholds that `vitalog today` compares
against:

```yaml
---
kcal_min: 1900
kcal_max: 2200
protein_min: 140
weight_target: 110
---
```

Suffixes recognized: `_min`, `_max`, `_target`. Any frontmatter key
matching `<metric>_<suffix>` is grouped by `<metric>`. Non-matching
keys are silently ignored, so the file can also hold commentary keys
(e.g., `last_review: 2026-04-30`). Suffix matching is case-sensitive.

## Tabs

- **Dashboard**: Today's vitals — sleep, weight, mood, energy, session context
- **Training**: Lifts, TSB gauge, session metrics
- **Trends**: 42-day sparklines for weight, exercises, and custom metrics
- **Climbing** (opt-in): Grade pyramid, weekly progression, session summary

## Config

`~/.config/vitalog/config.toml`:

```toml
notes_dir = "~/vitalog-notes"
# refresh_secs = 15
# time_format = "12h"  # or "24h" — controls how times are written to
                      # markdown and rendered in the TUI. The database
                      # always stores canonical 24h regardless of this.

[modules]
# dashboard = true
# training = true
# trends = true
# climbing = false

[exercises]
squat = { display = "Squat", color = "cyan" }
bench = { display = "Bench", color = "green" }
deadlift = { display = "Deadlift", color = "yellow" }
ohp = { display = "OHP", color = "magenta" }
pullup = { display = "Pullup", color = "blue" }
rdl = { display = "RDL", color = "red" }

[metrics]
# resting_hr = { display = "Resting HR", color = "red", unit = "bpm" }
```

Exercises, metrics, and colors hot-reload without restart. Module enable/disable requires restart.

### Overriding the config path

Set `$VITALOG_CONFIG` to a config.toml of your choice and `vitalog` will read from that file (and write to its `notes_dir`) instead of the platform default. Useful for sandbox/testing setups, or for running multiple parallel installs:

```bash
VITALOG_CONFIG=~/.vitalog-sandbox/config.toml vitalog bp 138 88 65 --morning
```

If the env var points to a file that doesn't exist, vitalog errors out rather than silently falling back to the default config — so a typo can't accidentally write to your real notes.

### Upgrading

After upgrading vitalog, run `vitalog rebuild` to re-materialize all notes
into canonical form in the database. New releases occasionally tighten
parsing or change canonical storage; rebuilding ensures `vitalog status
--json` and the TUI see consistent values across historical days.

## AI-Native

vitalog is designed for AI agents:

- `vitalog log` lets your AI assistant track your workout
- `vitalog status --json` provides structured data for AI analysis
- SQLite DB is directly queryable for complex questions
- Ships with a Claude Code skill for seamless integration
- `AGENTS.md` documents the full AI interface
- `vitalog readme` prints the README embedded in the binary, so an agent that only has the installed binary can still discover the full convention without network access or a separate clone

## Nutrition database

Daylog reads `{notes_dir}/nutrition-db.md` (if present) and materializes it into a `foods` table that other tooling can query. The file is the source of truth — SQLite is a derived cache.

Each entry is one `## Heading` followed by a fenced ` ```yaml ` block. Freeform prose under the block is preserved as `notes`.

`````markdown
## Tomato Soup

```yaml
per_100g:
  kcal: 70
  protein: 1.4
  carbs: 4.8
  fat: 5.0
gi: 40
gl_per_100g: 2
ii: 35
aliases: [tomato-soup]
```

Contains tomatoes — high acidity, sometimes triggers reflux.

## protein-shake

```yaml
description: 62g powder + 400ml water
total:
  weight_g: 462
  kcal: 234
  protein: 48
ingredients:
  - food: Whey
    amount_g: 62
gi: 5
ii: 85
```
`````

### Recognized fields

At least one of `per_100g`, `per_100ml`, or `total` must be present. Everything else is optional.

| Field | Meaning |
|---|---|
| `per_100g` / `per_100ml` | Nutrient panel: `kcal`, `protein`, `carbs`, `fat`, `sat_fat`, `sugar`, `salt`, `fiber` |
| `density_g_per_ml` | Conversion between weight and volume |
| `gi` | Glycemic index |
| `gl_per_100g` / `gl_per_100ml` | Glycemic load |
| `ii` | Insulin index |
| `aliases` | Lowercased lookup names. The heading is auto-added. |
| `description` | Free-text composition (e.g. "62g powder + 400ml water") |
| `ingredients` | List of `{food, amount_g}` for composite recipes |
| `total` | Composite recipe totals (`weight_g`, `kcal`, ... ) |

### Convention: raw vs. cooked

When a food has materially different nutritional values raw vs. cooked (chicken, lentils, ground meat), record one entry per state, named distinctly: `Chicken Patties (raw)` and `Chicken Patties (cooked)`. The schema stores one panel per row; multi-state foods are split.

### Watcher and rebuild

The file is parsed live by the watcher on every save, and re-parsed from scratch by `vitalog rebuild`. Per-entry parse failures warn to stderr; other entries still get loaded. Deleting the file is a no-op — the `foods` table retains its last successful state. `vitalog status --json` reports `nutrition_db.foods_count` and `nutrition_db.last_synced`.

## Architecture

Two threads, one SQLite database (WAL mode), no async runtime.

- **Watcher thread**: Detects file changes, parses YAML, writes to SQLite
- **TUI thread**: Reads from SQLite, renders with ratatui

The file is the source of truth. The database is a materialized view.

## Contributing

- **Submit your preset**: Use a different exercise set? Share your `config.toml`
- **Build a module**: See `AGENTS.md` for the scaffolding guide

## License

MIT
