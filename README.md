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
# --fiber / --salt / --gi / --gl / --ii independently optional. GL
# auto-computes when GI and carbs are both known.
vitalog food --kcal 350 --protein 7 --carbs 24 --fat 25 \
            --fiber 4.2 --salt 1.1 --gi 50 "Random pasta dish" 500g

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
  - **12:02** Protein shake (462g) (231 kcal, 47.0g protein, 2.4g carbs, 3.6g fat, 1.4g fiber, 0.5g salt)

Today so far: 1340 kcal, 95g protein, 50g carbs, 60g fat, 8.4g+ fiber (9 unknown), 5.6g salt
```

Fiber and salt are reported as lower bounds. A `nutrition-db.md` entry
without a `fiber:` or `salt:` key contributes nothing to the total and is
counted instead, so `8.4g+ fiber (9 unknown)` means nine of the day's
entries had no fiber value and the true total is at least 8.4 g. A
nutrient no entry supplied at all reads `fiber unknown (9 entries)` rather
than `0.0g` — an absent measurement is never reported as zero intake.

A missing `nutrition-db.md` key is not the only way to land in that count.
Fiber and salt are read **only from a nutrient group vitalog wrote itself**.
A group is accepted only if it matches what `vitalog food` emits exactly —
the same items, in the same order, with the same number of decimals — and
anything else reads as unknown.

That strictness is the whole rule, and it is deliberate. Whether a number
in a hand-written line is a measurement or part of a food name cannot be
decided from the text: `Lightly salted chips 0.1g salt per bag` and
`60g kolhydrater varav ~7g fiber` are the same shape. So neither is read,
and `unknown` is the honest answer for a day on which nothing recorded the
value. The four macros are unaffected — they still come off any line that
names them, exactly as they always have, so nothing that used to count
stops counting.

Most ways of editing a line by hand will not make fiber or salt count.
`- **09:00** Knäckebröd 90 kcal, 6.0g fiber` (no group at all),
`(90 kcal, ca 6.0g fiber)` (a prose-led item), `(~90 kcal, ~6.0g fiber)`
(estimate markers) and `( 90 kcal, 6.0g fiber)` (one extra space) all read
as unknown. Every one of them still keeps the entry and its calories in the
day's total — a rejected group costs you the two nutrients, never the meal.

The one hand edit that *does* count is appending the value as its own
group: `- **09:00** Knäckebröd (90 kcal) (6.0g fiber)` records 6.0 g of
fiber. That is not a special case — a lone `(6.0g fiber)` is exactly what
vitalog writes for an entry that has fiber and nothing else, so the two are
indistinguishable and no rule could separate them.

The reliable route is still to log the value rather than write it: `--fiber`
/ `--salt` work in both custom and lookup mode, or add the key to
`nutrition-db.md` and log the entry again.

A food line vitalog cannot parse is missing from every number on that
line, so it is called out after them, named by its timestamp — and it makes
fiber and salt lower bounds as well, even where every entry that *did* parse
supplied a value (hence the `+` on salt here, which has no unknown entries of
its own):

```
Today so far: 1340 kcal, 95g protein, 50g carbs, 60g fat, 8.4g+ fiber (9 unknown), 5.6g+ salt — 1 food line couldn't be parsed (08:00)
```

The same sentence is the `--json` `warnings` entry, so a consumer that
matched it exactly should match on the prefix instead.

`--fiber` and `--salt` also work in lookup mode, where they override
whatever the `nutrition-db.md` panel produced. That is how you fill the gap
for a food that *is* in the db but carries no `fiber:` key, without
retyping all four macros to force custom mode:

```bash
vitalog food "havregryn" 80g --fiber 8
```

Both are the amount in *this entry*, not a per-100 g figure — the amount
factor is not applied to them.

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
totals (kcal/protein/carbs/fat, plus fiber and salt with their
unknown-coverage counts, from the `## Food` section), morning
weight, sleep, morning BP, and any custom metrics — with optional
goal comparison from `goals.md`. Add `--json` for machine-readable
output suitable for AI agents and scripts.

```bash
vitalog today                    # today's summary
vitalog today 2026-04-29         # any past date
vitalog today --json             # JSON for tooling
```

In `--json`, `metrics.fiber` and `metrics.salt` carry five extra keys
alongside the usual `value` / `min` / `max` / `target`:

```json
"salt": {
  "value": 5.6, "max": 6.0,
  "unknown_entries": 2, "entry_count": 12, "skipped_lines": 0,
  "verdict": null, "verdict_note": null
}
```

`value` is a **lower bound** unless `unknown_entries` and `skipped_lines`
are both zero — the same test the text surfaces mark with a trailing `+`.
Here two of the day's twelve entries had no salt figure, so the true total
is at least 5.6 g. Do not conclude `value <= max` from such an object; the
text output deliberately withholds its `✓ under maximum` for the same
reason. An exact total is not automatically a measured one:
`entry_count == unknown_entries` (including `entry_count == 0`) means
nothing about the nutrient is known, so that zero never earns a green
check. A `_min` shortfall is still reported off it — `verdict` is
`"warn"` — unless a `[metrics.*]` row logs the same nutrient and can
report the shortfall from a real figure instead.

The three counts describe different gaps and none of them substitutes for
another:

- `unknown_entries` — entries vitalog parsed that carried no value for
  this nutrient.
- `entry_count` — food entries parsed on the day. `unknown_entries ==
  entry_count` means nothing at all is known, which is why the count is
  reported: without it, `{"value": 0.0, "unknown_entries": 3}` is
  indistinguishable from a partial total whose known entries summed to
  zero. This includes `entry_count == 0`, a day with no food logged: the
  zero there is structural, not measured, so vitalog never issues a
  *reassuring* verdict on it — a `_min` shortfall is still reported, as
  above.
- `skipped_lines` — food lines vitalog could not parse at all. They are
  counted in neither of the other two, so an object with
  `unknown_entries == 0` is still a lower bound when this is non-zero. It
  is a day-scoped count, repeated on both nutrient objects.

The remaining two keys report the goal check itself rather than the inputs
to it:

- `verdict` — `"ok"` when `vitalog today` prints a green check for this
  figure, `"warn"` when it prints a shortfall or overage, and `null` when
  it prints neither. `null` covers every reason for that: no goal, a
  target-only goal, and each case below where vitalog declines to rule.
  Treat all three the same way — **do not compute a verdict of your own
  from `value` and `max` when this is `null`.** That is the conclusion the
  text output deliberately withheld, and the counts above are there to
  explain why, not to license reaching it anyway.
- `verdict_note` — set only when the day's two figures for this nutrient
  disagree about the goal (see below); otherwise `null`.

If your config also defines `[metrics.fiber]` or `[metrics.salt]`, the two
are different measurements of the same quantity and vitalog reports both.
`vitalog today` prints a row for each and warns about the duplicate. The
goal is checked once, on the food-derived row — a second verdict for the
same goal is redundant when the two rows agree and misleading when they
don't. Three things fall outside that rule:

- On a day the food-derived row measured nothing (no food entries, or none
  carrying the nutrient) it has no claim on the goal, so your logged row —
  the day's only figure — keeps its own verdict, and the shortfall the
  food-derived row would otherwise report off its structural zero steps
  aside for it. This is the shape of every note written before vitalog
  tracked nutrients, so it is the normal case for your back-catalogue
  rather than an edge one. It applies only where a logged row exists to
  rule instead; without one the food-derived row reports the shortfall as
  usual.
- When that row measured only part of the day, its lower bound settles
  some verdicts and not others: it proves `✓ over minimum` and an
  over-maximum warning, since more entries can only add, but it cannot
  prove `✓ under maximum`, `✓ within range` or a shortfall — more entries
  could undo any of those. Where it proves nothing, your logged row
  reports its own verdict instead; what is withheld from it is
  reassurance the food-derived row refused as unprovable, never a warning.
  So `Fiber: 8.4+ / ≥35 g  (27 below min)` beside a logged 40 g keeps the
  shortfall on the food-derived row — of what was measured, the day is
  short — while the logged row, the only figure that can rule, keeps its
  `✓ over minimum`.

  This is a change from how vitalog behaved before fiber and salt were
  reported, and worth knowing if you already track one of them by hand: a
  `[metrics.salt]` row logging 3.4 against `salt_max: 6` used to print
  `✓ under maximum` on every day, and now goes without it on days the
  food-derived total measured part of the day and could not prove the same
  thing (in `--json`, `logged_verdict` is `null` there rather than `"ok"`).
  Only reassurance is affected; no warning is ever withheld. Note the
  direction, which is the surprising half: with *nothing* measured your row
  keeps its check (the bullet above), and one measured entry — strictly
  more evidence — takes it away. The check is withheld exactly when there
  is a partial measurement that cannot back it up.

  One smaller pre-existing-behavior change comes with the same release, on
  rows that have nothing to do with fiber or salt. The `(n below min)` /
  `(n above max)` distance no longer rounds to a whole unit when that would
  round to zero; it falls back to one decimal, then two. A weight of
  110.4 kg against `weight_max: 110` now prints `(0.4 above max)` where it
  used to print `(0 above max)`, and the weight row, every custom metric
  row and the four macro rows all share that formatting. The old text was
  degenerate — it announced a miss and reported the distance as nothing —
  but if you script on that string, it changed.
- When the two figures land on **different sides of the goal**, the
  reassuring one is withheld and the row says why:

  ```
  Salt: 3.5 / ≤6 g  ⚠ logged 8 g vs 3.5 g measured — cannot reconcile
  ...
  Salt: 8 / ≤6 g     (2 above max)
  ```

  Full coverage does not mean all the salt is accounted for — salt added
  while cooking or at the table never reaches the food-derived total, and a
  restaurant meal logged as one entry under-captures seasoning. The
  food-derived total is a lower bound even at full coverage, so a logged
  figure above it may be the *more* complete number. The rule is: never
  show a reassuring verdict that another logged figure on the same day
  contradicts. It keys on the two verdicts and never on the gap between the
  numbers, so 3.4 against 3.5 stays silent while 3.5 against 8 under a cap
  of 6 does not, and it needs no per-goal tuning to work in both directions
  — under-reporting is the dangerous error under a `_max` goal and
  over-reporting under a `_min` one.

  It takes two verdicts to disagree, so this needs the food-derived row to
  have *proved* one — not merely printed one. While its total is an open
  lower bound it proves only what more entries cannot undo, and a logged
  figure on the other side of anything else contradicts nothing, because
  the unmeasured entries could account for the whole difference.
  `Salt: 2.5+` beside a logged 8 g is the previous case, not this one: no
  note, and the logged row's `(2 above max)` stands. So is
  `Fiber: 8.4+  (27 below min)` beside a logged 40 g, where the shortfall
  is what the day measured rather than what it proves.

In `--json` the food-derived total keeps the `metrics.<id>` slot, so
`unknown_entries` / `entry_count` / `skipped_lines` / `verdict` /
`verdict_note` are always present, and your manually logged figure is
reported alongside it as `logged_value` / `logged_unit` /
`logged_verdict`. All three are conditional: `logged_value` and
`logged_verdict` appear only on days you actually logged the metric, and
`logged_unit` only when the metric defines a `unit`. `logged_verdict`
takes the same `"ok"` / `"warn"` / `null` values as `verdict`, so at most
one of the pair is non-null on a day the two rows disagree, and the note
explaining it is at `verdict_note`. Note also that `metrics.<id>.unit` is
absent for a shadowed metric — the food-derived object does not carry one,
and the manual metric's unit moves to `logged_unit`.

Do not rename the metric to resolve the duplicate: the config id doubles
as the note frontmatter key, so a rename orphans every `salt:` value
already written in past notes.

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
fiber_min: 35
salt_max: 6
weight_target: 110
---
```

Suffixes recognized: `_min`, `_max`, `_target`. Any frontmatter key
matching `<metric>_<suffix>` is grouped by `<metric>`. Non-matching
keys are silently ignored, so the file can also hold commentary keys
(e.g., `last_review: 2026-04-30`). Suffix matching is case-sensitive.

Fiber and salt goals are checked against a lower bound when some entries
lack the nutrient or a food line failed to parse, so `vitalog today`
withholds a `✓ under maximum` mark until coverage is complete — the
missing entries could still push the total past the cap. A `✓ over
minimum` or an over-maximum warning is shown regardless, since neither can
be undone by adding more. On a day where nothing was measured at all — no
food entries, or none carrying the nutrient — the food-derived row is
never given a check mark: one earned by an empty sum would be the same
false reassurance from the other direction. A shortfall against a `_min`
goal is still reported there, from a zero as from anything else, so
`fiber_min: 35` on a day with no food logged reads `Fiber: 0.0 / ≥35 g
(35 below min)`. (The one exception is a day where a `[metrics.*]` row
logged the same nutrient and can report the shortfall itself — see
above.)

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
