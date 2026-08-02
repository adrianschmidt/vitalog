# Salt and fiber reporting — design

Issue: [#39](https://github.com/adrianschmidt/vitalog/issues/39)
Date: 2026-07-31

## Problem

`salt` and `fiber` are parsed from `nutrition-db.md` and persisted in every
column of the `foods` table, but no read path surfaces them. Logging food
prints `1077 kcal, 88g protein, 38g carbs, 62g fat`; tracking either nutrient
means re-deriving it by hand.

Both matter clinically for this user: hydrochlorothiazide makes sodium load
relevant, and measured fiber intake is ~10 g/day against a 35 g
recommendation (NNR 2023).

This is a plumbing gap, not a data-model change. The missing links are
`RenderedEntry`, `format_nutrient_segment`, `FoodTotals`,
`format_food_totals`, and the `vitalog today` renderer.

## Constraint that shapes everything: the markdown line is the source of truth

`food_sum::sum_food_section` is the inverse of `food_cmd::format_line`. Daily
totals are recovered by re-parsing the `## Food` section, not from the
database. A nutrient can therefore only appear in a running total if it is
written into the markdown line first.

Two consequences:

1. This is a file-format change. Every `## Food` line written before it
   lacks both tokens.
2. Coverage in `nutrition-db.md` is partial — 83 of 106 entries carry
   `salt:`, only 29 carry `fiber:`. For fiber, a missing value is the
   majority case, not a corner case.

A missing value must therefore never read as `0.0`.

## Design

### 1. Lower bound plus unknown count

`src/food_sum.rs` gains a type, and `FoodTotals` two fields:

```rust
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct NutrientTotal {
    /// Sum over entries that carried a value for this nutrient.
    pub sum: f64,
    /// Entries counted in the day that had no value for it.
    pub unknown: usize,
}

pub struct FoodTotals {
    pub kcal: f64,
    pub protein: f64,
    pub carbs: f64,
    pub fat: f64,
    pub fiber: NutrientTotal,
    pub salt: NutrientTotal,
    pub entry_count: usize,
    pub skipped_lines: usize,
}
```

`parse_food_line` returns `Option<f64>` for fiber and salt instead of
defaulting to `0.0` the way the four macros do. A parsed line missing the
token increments `unknown`; it never contributes to `sum`.

The count of entries that *did* carry the value is `entry_count - unknown`,
so no third field is needed.

The `Today so far:` line renders three states:

| state | rendering |
|---|---|
| `unknown == 0` | `12.4g fiber` |
| `0 < unknown < entry_count` | `8.4g+ fiber (9 unknown)` |
| `unknown == entry_count` (and `entry_count > 0`) | `fiber unknown (9 entries)` |

A day with no food entries at all falls in the first state and renders
`0.0g fiber`, which is correct — no intake.

The `vitalog today` rows use only the first two: a dashboard row sits
opposite a goal bound (`/ ≥35 g`), so it stays numeric even when nothing is
known, rendering `0.0+` with the unknown count rather than the word
"unknown".

`kcal`, `protein`, `carbs` and `fat` keep their existing `f64` +
treat-missing-as-zero behavior. Changing them is out of scope and would
alter output for every existing day.

### 2. Write path

`RenderedEntry` and `CustomNutrients` in `src/cli/food_cmd.rs` gain
`fiber: Option<f64>` and `salt: Option<f64>`.

- `render_with_amount` scales both by the same `factor` already applied to
  protein, off whichever panel (`per_100g` / `per_100ml`) was selected,
  including the density-bridge path.
- `render_total_only` reads `total.fiber` / `total.salt` unscaled.
- `format_nutrient_segment` appends them after fat, fiber before salt
  (salt last matches nutrition-label convention). Fiber uses one decimal;
  salt uses two, with a single trailing zero trimmed so the common case
  still reads `2.2g salt`. The markdown line is the only storage the daily
  total is re-derived from, and salt is the one nutrient whose interesting
  range sits inside the one-decimal band — at `{:.1}` a 0.02 g entry is
  written as `0.0g salt` and read back as a *known* zero:

```
- **15:13** ICA Salsiccia (100g) (251 kcal, 12.0g protein, 3.4g carbs, 21.0g fat, 0.0g fiber, 2.2g salt)
```

A token is omitted entirely when the source has no value. That omission is
what makes the value parse back as unknown rather than zero, so it is
load-bearing, not cosmetic.

Because the token may be absent, `rfind` alone is not enough to keep the
food name out of the number: it protects a token that is *present*, but
when the token is omitted the rightmost match is whatever the free-text
name happens to contain, and the forged value would then be counted as a
measurement rather than as unknown.

The rule that resolves this is deliberately not a judgement about the text.
`parse_food_line` narrows to the innermost `(…)` enclosing the rightmost of
the six nutrient tokens, then accepts that group **only if it matches what
`format_nutrient_segment` writes, exactly** — the same items, in the
writer's order, each carrying the digits `format!` would have produced.
Anything else yields unknown fiber and salt. `machine_nutrients`' doc
comment is the normative statement of the grammar; this is a summary.

Five earlier revisions tried to draw the line somewhere softer: top-level
versus innermost group, anchoring on `" kcal"` versus all six tokens,
checking the group's opening item, then every item for a leading quantity.
Each closed one reported shape and opened another, because the underlying
question is not decidable from the text. `Lightly salted chips 0.1g salt
per bag` inside a product name and `60g kolhydrater varav ~7g fiber` inside
a hand-written panel are the same shape, and a requirement to read the
second while refusing the first is self-contradictory.

Counting the corpus settled it. Of 1279 food entries, 1012 are machine-
written and 245 hand-written; exactly **four** hand-written lines carry a
fiber or salt figure at all — all four from two consecutive days in April,
totalling 17 g of fiber, with no salt figure anywhere in the corpus. That
was the entire prize the heuristics were chasing. They are gone, and those
four lines now read as unknown, which is also the truthful answer: nothing
measured salt or fiber on those days.

What this buys is that there is no rule to tune, nothing to classify, and
no path by which text a human typed becomes a number in a total. The four
macros are untouched — they still come off the whole line for any line that
names them — so the parse stays byte-identical to `main` on every entry in
the corpus, and only fiber and salt are held to the stricter standard.

The group is located by a depth-tracking scan rather than by taking the
first `)` after the token. `format_line` never nests parentheses inside the
group, but a hand-edited line can: `(350 kcal (uppskattat), 7.0g protein,
…)` would otherwise end the segment at `uppskattat`, dropping
protein/carbs/fat to zero on a line that still counts as parsed — a wrong
number with no warning, which is worse than a skipped line.

A line with no such group at all is *not* skipped. Hand-written entries
exist (`- **09:00** Banan 90 kcal`, or a line whose closing paren was
edited away), and dropping them from the day's totals is a regression
against every earlier version. `parse_food_line` falls back to the whole
line for the four macros — restoring the previous behavior exactly — but
leaves fiber and salt `None`. The forgery this narrowing exists to prevent
lives entirely in the fiber/salt path, because those are the only tokens
whose absence is meaningful; a missing macro token has always read as 0.0,
so an unanchored macro match forges nothing that was distinguishable.

### 3. New CLI flags

`--fiber` and `--salt` join `--gi` / `--gl` / `--ii` as optional extras on
`vitalog food`. They are *not* part of the required
`--kcal/--protein/--carbs/--fat` quartet — `require_custom_complete` is
unchanged. Without them, every one-off custom entry would be permanently
unknown for both nutrients.

They behave like `--gi` / `--gl` / `--ii` in **both** modes: in lookup mode
`apply_lookup_overrides` writes them onto the `RenderedEntry` after the
panel has been scaled. Supplying a missing value for a food that *is* in
the db is the main real use — 77 of 106 db entries have no `fiber:` key —
and the only alternative would be retyping all four macros to force custom
mode, bypassing the db. The value is absolute (the grams in this entry),
not a per-100 g figure, so the amount factor is not applied to it; that
matches custom mode, where `--fiber` is likewise the entry's own total.

Both flags are validated at the top of `execute`: non-finite and negative
values are rejected, mirroring `parse_amount`. A negative value would
otherwise write `-3.5g fiber` and read back as a *positive* 3.5 marked
complete, since the backward digit walk stops at the minus sign. Zero is
accepted — an explicit `0.0g salt` is a measurement, not a gap.

### 4. `Today so far:` line

Single line, appended to the existing comma list:

```
Today so far: 1077 kcal, 88g protein, 38g carbs, 62g fat, 8.4g+ fiber (9 unknown), 5.6g+ salt (2 unknown)
```

Macros keep integer rounding. Fiber and salt use one decimal — integer
rounding would destroy salt, where the interesting range is 0.4–8 g.

### 5. `vitalog today`

Two rows after Fat in the food block, always present:

```
Fiber: 8.4+ / ≥35 g      (27 below min)  (9 unknown)
Salt:  5.6+ / ≤6 g                       (2 unknown)
```

- Value uses one decimal and a trailing `+` whenever the total understates
  the day: `unknown > 0`, or a food line the parser dropped
  (`skipped_lines > 0`), whose nutrients are missing from the sum in
  exactly the same sense. `NutrientTotal::is_lower_bound` owns that test
  for every surface that renders it.
- `"fiber"` and `"salt"` are added to the hardcoded `known` metric set in
  `today_cmd::execute`, so `fiber_min: 35` / `salt_max: 6` in `goals.md` do
  not raise `unknown metric` warnings.
- A dim `(n unknown)` suffix is appended whenever `unknown > 0`.

Goal annotations are filtered by whether the unknowns could invalidate them:

| annotation | with unknowns | why |
|---|---|---|
| `✓ over minimum` | keep | true total ≥ lower bound, so passing is proven |
| `(n above max)` | keep | unknowns only add; exceeding is proven |
| `(n below min)` | keep, as display only | may overstate the shortfall, but `+` and `(n unknown)` sit beside it, and it is the signal the user wants for fiber; it is *not* proof — see below |
| `✓ under maximum` | **suppress** | unknowns could push the true total over |
| `✓ within range` | **suppress** | same, for the upper bound |

The table is one predicate rather than five cases, and it is worth stating
that way because the enumeration is what keeps getting a case wrong. A
lower bound establishes only that the true total is *at least* the sum, so
it can prove only a claim of that shape. `✓ over minimum` and
`(n above max)` both assert "the true value is at least X" and survive;
`✓ under maximum` and `✓ within range` both assert "the true value is at
most X" and cannot, whichever way the goal points. **A lower bound can
only prove a lower-bound claim.** `lower_bound_proves` is that predicate,
and every path from a total to a verdict routes through it.

`(n below min)` is an at-most claim too, which is why it is kept for
*display* and nothing more. Beside `8.4+` and `(9 unknown)` it reads "of
what has been measured, you are short", which is true and is the signal
issue #39 asked for; as evidence about the day's true total it proves
nothing, and section 6 may not treat it as a verdict.

On a total that is both exact and *measured* the rows annotate exactly
like existing food rows. Exactness alone is not enough: a day with no food
entries has no unknowns either, and the row would collect `✓ under
maximum` off a structural zero. Both halves — no gaps, and at least one
entry that carried the nutrient — are required before a verdict the
unknowns could invalidate is printed.

The `(n below min)` row of the table survives a structural zero, and that
is deliberate rather than an oversight. Zero coverage is the common case
while most of the food db carries no `fiber:` key, and a running total
that cannot say whether the day is short is the gap issue #39 asked to
close; the `+` and the `(n unknown)` count beside it mark how much the
number is worth. Section 6 stands that shortfall down in exactly one
place — a `[metrics.*]` row logging the same nutrient, which can report it
from a real figure instead — and nowhere else.

### 6. JSON

`render_json` emits `metrics.fiber` and `metrics.salt` alongside the
existing macros, with `value` set to the lower-bound sum plus five extra
fields:

```json
"fiber": { "value": 8.4, "min": 35, "max": null, "target": null, "unknown_entries": 9, "entry_count": 12, "skipped_lines": 0, "verdict": "warn", "verdict_note": null }
```

They are added only for these two metrics; the four macros keep their
current shape. Completeness is derivable (`unknown_entries == 0 &&
skipped_lines == 0`), so no separate boolean — `skipped_lines` is in the
object because a dropped food line is counted in neither of the other two,
so without it `{"unknown_entries": 0, "entry_count": 1}` reads as exact on
a day two further lines went unparsed. `entry_count` is what makes the
third state reconstructible — `unknown_entries == entry_count` means nothing is known,
which `{"value": 0.0, "unknown_entries": 3}` alone cannot be told apart
from a partial total whose known entries summed to zero. Without it an
agent reading `--json` would draw exactly the conclusion `render_text`
suppresses.

The counts make that conclusion *reconstructible*, which is not the same as
stating it, so `verdict` and `verdict_note` carry the goal check itself:
`"ok"` when the text surface printed a green check for the figure,
`"warn"` when it printed a shortfall or overage, and `null` whenever it
printed neither — no goal, a target-only goal, or a verdict the rules
below withhold. All three read the same to a consumer: vitalog declines to
rule, so do not rule for it. Text and JSON disagreeing here is a worse
failure than either being wrong alone, because a JSON consumer has no way
to tell it is holding a number the text surface deliberately refused to
bless. Both renderers therefore read one `nutrient_verdicts` value, and the
single-verdict rule is a property of the data rather than of one renderer.

`fiber` and `salt` are now built-in ids. A config that predates the feature
may define `[metrics.fiber]` / `[metrics.salt]` — before this, a custom
metric was the only way to track salt. The two are different measurements
of the same quantity (a manual daily estimate versus a partial sum over
logged entries), so vitalog does not silently pick a winner: the text
output shows both rows, the JSON keeps the built-in object in the
`metrics.<id>` slot (it is the one whose documented shape carries
`unknown_entries` / `entry_count`) and hangs the manually logged figure off
it as `logged_value` / `logged_unit` / `logged_verdict`, and
`detect_config_warnings` emits a warning naming the collision on both
surfaces. Nothing is dropped on either surface.

Nor is the manual capability deprecated by the built-in totals. Every
`## Food` line written before this feature carries no nutrients, so for the
entire back-catalogue the food-derived total is a structural `0.0+ (n
unknown)` and a manually logged figure is the only real number those days
have. The collision rule has to stay sane for a history where the
food-derived row can never rule.

Two consequences fall out of showing both text rows, and both are handled
rather than assumed away:

- The rows carry the same label and the same threshold, so annotating both
  puts two verdicts on screen for one goal, and they contradict each other
  precisely when it matters: a `✓ under maximum` on the manually logged row
  hands back the reassurance the food-derived row deliberately withheld,
  and a manual estimate that disagrees with the partial food sum produces
  the same clash from the other side (`(0.3 above max)` above `✓ under
  maximum`). The goal is therefore checked once, on the row whose
  provenance vitalog controls; the shadowing row keeps its number and the
  inline threshold and drops only the verdict. The warning states the rule,
  so the missing verdict is explained on the surface it goes missing from.

  Deferring to that row is worth it only when it produced a figure. On a
  day it measured nothing — no food entries at all, or none carrying the
  nutrient — it has no claim on the goal, and suppressing there would
  withhold the verdict on the day's *only* salt figure while the
  food-derived row collected a `✓ under maximum` off a structural zero
  (`is_complete()` is vacuously true when nothing was counted). That is the
  pre-feature shape exactly: `[metrics.salt]` was the only way to track
  salt, so a day logged that way and no other would turn a red
  `(2 above max)` into a green check on the wrong row.

  Measuring the day is therefore necessary for the food-derived row to own
  the goal, but not sufficient to silence the other one. What the shadowing
  row is denied is *reassurance* the food-derived row refused: it goes
  quiet when that row's total *proved* a verdict of its own (one is
  enough), and when that row could prove none only because a
  `✓ under maximum` / `✓ within range` cannot be claimed off a partial
  total and the manual figure would claim exactly that. Proved, not
  printed: the row may be showing `(n below min)` off an open lower bound,
  and a claim that proves nothing does not stand another row down. A
  warning survives both cases —
  `(n above max)` on a manually logged 8 g says something the food-derived
  row's silence never did, and withholding it left a partial-coverage day
  with no verdict anywhere, dropping the red `main` printed.

  The last piece is what happens when the two figures do not merely differ
  but land on opposite sides of the goal. Full coverage does not mean all
  the salt is accounted for: salt added while cooking or at the table never
  reaches the food-derived total, and a restaurant meal logged as one entry
  systematically under-captures seasoning. The food-derived total is a
  lower bound *even at full coverage*, so a manual figure above it may be
  the more complete number rather than a contradiction of the measurement.
  Printing `✓ under maximum` off a complete 3.5 g while a deliberately
  logged 8 g sits below it under `salt_max: 6` is reassurance the day's own
  data denies — the exact failure the `+`, the unknown counts and the
  withheld `✓` all exist to prevent. Stated as one rule: **never show a
  reassuring verdict that another logged figure on the same day
  contradicts.** The `✓` is withheld, the warning stands, and a note naming
  both figures (`logged 8 g vs 3.5 g measured — cannot reconcile`) gives
  the missing check a stated reason.

  That is keyed strictly on the two verdicts and never on the gap between
  the figures. A numeric threshold would be arbitrary and would need
  re-tuning per goal; keying on the verdicts is self-limiting and fires
  only where the discrepancy changes what the day calls for, so 3.4 against
  3.5 stays silent and 3.5 against 8 under a cap of 6 does not. It also
  needs no special-casing per goal direction: under-reporting is the
  dangerous error under a `_max` goal and over-reporting under a `_min`
  one, and withholding the contradicted reassurance is correct for both.

  Disagreement takes *two* verdicts, so the food-derived side counts only
  where its total **proves** one — `lower_bound_proves` from section 5, not
  what the row printed. The two part company on exactly one shape, the
  `(n below min)` the table keeps as display: `8.4+` with nine of twelve
  entries unmeasured under `fiber_min: 35` shows `(27 below min)` and
  proves nothing, because those nine entries can carry the true total well
  past 35. A logged 40 g there is not contradicted by anything, and the
  mirror case is the same predicate rather than a second rule — `2.5+`
  against a logged 8 g under `salt_max: 6` is no contradiction either,
  since the unmeasured entry could carry the missing 5.5 g. Saying either
  pair "cannot reconcile" would be false.

  Nothing is lost by staying quiet. The food-derived row keeps whatever the
  table lets it print, and the logged figure — which it has not
  contradicted — rules on the goal in its place, exactly as it does when
  the food-derived row measured nothing.

  The guarantee this leaves. On **the food-derived row**, no reassuring
  verdict an incomplete total cannot *support* is ever printed — `✓ over
  minimum` is kept off a partial total, because more entries can only make
  it truer; `✓ under maximum` and `✓ within range` are the two the bound
  cannot back, and those are the two withheld (§5's annotation table). The
  logged row inherits that veto on every day the food-derived row measured
  something, so there the two are withheld from both rows. It does not
  inherit it at zero coverage, and that is the deliberate exception rather
  than an oversight: `Salt: 0.0+ / ≤6 g  (12 unknown)` above `Logged salt:
  3 / ≤6 g     ✓ under maximum` does print. It is the same reasoning the
  carve-out below states for a `_min` goal, in the `_max` direction — the
  structural zero measured nothing, so it contradicts nothing, and the only
  real figure on the day rules. Beyond that: no warning is ever
  suppressed while the food-derived row is silent; and
  no reassuring verdict is printed that
  a second figure on the same day contradicts. The one shape where two
  verdicts used to coexist — entries but zero coverage under a `_min` goal,
  where the food-derived row reported a shortfall from its structural zero
  and the manual row added its own — is resolved by preferring the real
  figure: the zero is not a measurement, so the shortfall computed from it
  stands down and the manual verdict is the day's only one. One line, not
  two. Standing it down is worth it only because the manual row replaces
  it, which is why the whole of this bullet is scoped to days a
  `[metrics.*]` row logged a figure; on every other day the annotation
  table of section 5 governs alone.
- The warning does **not** suggest renaming the metric. The config id
  doubles as the note frontmatter key that `materializer::daily` reads
  values from, so `[metrics.salt]` → `[metrics.salt_manual]` orphans every
  `salt:` value already written in past notes. Keeping the duplicate is
  usually the right call, and the warning says so.

The rule stops at `fiber` and `salt` because the criterion is a JSON object
shape a plain custom metric would break — those two are the only
`metrics.*` objects carrying `unknown_entries` / `entry_count` /
`skipped_lines`. Other
built-in ids (`kcal`, `weight`, …) emit the same `metric_obj` shape a
custom metric does, so a `[metrics.kcal]` collision loses no keys, is
unchanged by this feature, and is left alone rather than given new
behavior here.

### 7. Targeted cleanup: `NutrientArgs`

`food_cmd::execute` takes 13 positional arguments under
`#[allow(clippy::too_many_arguments)]`; `main.rs::cmd_food` mirrors it. Two
more would make a 15-argument call site of mostly `None`, where a
transposition is silent.

The nine nutrient flags fold into one struct owned by `food_cmd`, which
also derives `clap::Args` so `Commands::Food` can `#[command(flatten)]` it:

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, clap::Args)]
pub struct NutrientArgs {
    pub kcal: Option<f64>,
    pub protein: Option<f64>,
    pub carbs: Option<f64>,
    pub fat: Option<f64>,
    pub fiber: Option<f64>,
    pub salt: Option<f64>,
    pub gi: Option<f64>,
    pub gl: Option<f64>,
    pub ii: Option<f64>,
}

pub fn execute(
    name: &str,
    amount: Option<&str>,
    nutrients: NutrientArgs,
    date_flag: Option<&str>,
    time_flag: Option<&str>,
    config: &Config,
    quiet: bool,
) -> Result<()>
```

`execute` drops to 7 arguments, the clippy allow goes away, and the ~10 test
call sites become named fields instead of positional `None` runs. Scope is
limited to code this change already touches.

Flattening matters beyond tidiness. The field list otherwise exists three
times — the clap `Food` variant, the `main.rs` destructure, and the
`NutrientArgs` literal — and none of the three copies is compiler-enforced
against the others, so a flag can be accepted and never read. That is
exactly how `--fiber` / `--salt` first shipped: parsed, carried into
`NutrientArgs`, and then consulted only on the custom branch. With
`#[command(flatten)]` there is one definition and one value threaded to
`execute`; the CLI surface is unchanged (`vitalog food --help` is
byte-identical).

## Out of scope

- **Ingredient rollup.** The issue assumes composite entries sum kcal from
  their `ingredients:` list. They do not: `food_ingredients` rows are
  written by the materializer and never read back — `FoodLookup` has no
  ingredients field. Composites carry their own `total:` / `per_100g:`
  panel, and kcal comes from that. A composite whose panel lacks `salt:` or
  `fiber:` counts as one unknown, which is the honest result. Ingredient
  rollup (resolution, cycles, missing components, unit mismatches) deserves
  its own issue.
- **Backfilling `nutrition-db.md`.** 29/106 fiber coverage is worth fixing
  but must not block this; the unknown-count design exists precisely so
  partial coverage is usable and visible. Its own issue.
- **Changing macro semantics.** `protein`/`carbs`/`fat` keep
  treat-missing-as-zero.

## Testing

Unit tests, extending the existing modules:

- `food_sum`: fiber/salt parsed from a full line; a line with neither
  increments both unknown counts; a line with fiber but not salt splits
  correctly; `unknown == entry_count` case; round-trip against
  `format_line`; existing macro behavior unchanged.
- `food_cmd`: per_100g and per_100ml scaling; density-bridge path;
  total-panel path; `format_line` token order and omission; the three
  `format_food_totals` states; `--fiber`/`--salt` custom flags; custom mode
  without them still succeeds.
- `today_cmd`: both rows render; `+` appears only when incomplete;
  each row of the annotation-filter table; `fiber`/`salt` goal keys produce
  no unknown-metric warning; JSON shape including `unknown_entries`.
- `tests/today.rs`: end-to-end — write a note with mixed known/unknown
  entries, assert the rendered rows.

## Files

| file | change |
|---|---|
| `src/food_sum.rs` | `NutrientTotal`, two `FoodTotals` fields, parse |
| `src/cli/food_cmd.rs` | entry fields, scaling, line format, totals format, `NutrientArgs` |
| `src/cli/mod.rs` | `--fiber` / `--salt` flags |
| `src/main.rs` | dispatch through `NutrientArgs` |
| `src/cli/today_cmd.rs` | two rows, `known` set, JSON |
| `README.md` | food examples, `today` output, goal keys |
| `tests/today.rs` | end-to-end coverage |
