//! Parse the `## Food` section of a daily note back into aggregate
//! macro totals. Inverse of `cli::food_cmd::format_line`.

/// A nutrient whose per-entry value may be absent from the markdown line.
///
/// `sum` covers only the entries that carried a value, so it is a lower
/// bound whenever `unknown > 0`. Coverage in `nutrition-db.md` is partial
/// (29 of 106 entries carry `fiber:`), so a missing token must never be
/// summed as zero — that would report an intake the data doesn't support.
/// Entries that did carry a value are `FoodTotals::entry_count - unknown`.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct NutrientTotal {
    pub sum: f64,
    pub unknown: usize,
}

impl NutrientTotal {
    /// True when every counted entry supplied a value, so `sum` is exact.
    ///
    /// Private on purpose. It is the weaker half of the exactness test —
    /// vacuously true on a day with no entries, and blind to a food line
    /// the parser dropped — and renderers reaching for it instead of
    /// `is_lower_bound` is what earlier attempts at this got wrong. Off the
    /// public surface, the next surface added cannot make that mistake.
    fn is_complete(&self) -> bool {
        self.unknown == 0
    }

    /// True when `sum` understates the day — the single definition every
    /// surface renders from.
    ///
    /// A per-entry token the line never carried and a food line the parser
    /// dropped cost the sum in exactly the same way, but only the first is
    /// nutrient-scoped: `skipped_lines` is a property of the day, so it has
    /// to be handed in. Keeping the test here rather than spelled out at
    /// each call site is what stops `Today so far:`, the dashboard row and
    /// `--json` from disagreeing about whether a figure is exact.
    pub fn is_lower_bound(&self, skipped_lines: usize) -> bool {
        !self.is_complete() || skipped_lines > 0
    }

    /// Whether at least one of the day's entries supplied this nutrient.
    ///
    /// Kept here for the same reason as `is_lower_bound`: the running total
    /// and the dashboard both need it, and two independent spellings of the
    /// same predicate is how they drift into disagreeing about whether a
    /// zero means "measured none" or "measured nothing".
    ///
    /// Note this is not `!is_complete()` — a day with no entries at all has
    /// nothing unknown *and* nothing measured, so the two questions have
    /// different answers exactly where it matters most.
    pub fn is_measured(&self, entry_count: usize) -> bool {
        entry_count > self.unknown
    }

    fn add(&mut self, value: Option<f64>) {
        match value {
            Some(v) => self.sum += v,
            None => self.unknown += 1,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct FoodTotals {
    pub kcal: f64,
    pub protein: f64,
    pub carbs: f64,
    pub fat: f64,
    pub fiber: NutrientTotal,
    pub salt: NutrientTotal,
    pub entry_count: usize,
    pub skipped_lines: usize,
    /// The `**HH:MM**` stamp of each skipped line, in file order.
    ///
    /// Diagnostic only — `skipped_lines` stays the authority for every
    /// verdict, and this may be shorter than it, since a hand-composed
    /// line need not carry a stamp at all. Kept so the diagnostics can say
    /// *which* line was dropped rather than only how many: the count on
    /// its own leaves a user with a day's worth of entries to re-read.
    pub skipped_times: Vec<String>,
}

impl FoodTotals {
    /// `1 food line couldn't be parsed (12:00)` — the sentence all three
    /// skip diagnostics render, or `None` when nothing was skipped.
    ///
    /// Shared for the same reason `is_lower_bound` is: `Today so far:`,
    /// the `vitalog today` hint and the `--json` warning otherwise word
    /// the same fact three times and drift apart.
    pub fn skipped_note(&self) -> Option<String> {
        if self.skipped_lines == 0 {
            return None;
        }
        let plural = if self.skipped_lines == 1 { "" } else { "s" };
        let mut note = format!(
            "{} food line{plural} couldn't be parsed",
            self.skipped_lines
        );
        if !self.skipped_times.is_empty() {
            note.push_str(&format!(" ({})", self.skipped_times.join(", ")));
        }
        Some(note)
    }
}

pub fn sum_food_section(markdown: &str) -> FoodTotals {
    let mut totals = FoodTotals::default();
    let lines: Vec<&str> = markdown.lines().collect();

    let start = match lines.iter().position(|l| l.trim_end() == "## Food") {
        Some(i) => i + 1,
        None => return totals,
    };
    let end = lines
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(i, l)| l.starts_with("## ").then_some(i))
        .unwrap_or(lines.len());

    for line in &lines[start..end] {
        if !line.starts_with("- **") {
            continue;
        }
        match parse_food_line(line) {
            Some(p) => {
                totals.kcal += p.kcal;
                totals.protein += p.protein;
                totals.carbs += p.carbs;
                totals.fat += p.fat;
                totals.fiber.add(p.fiber);
                totals.salt.add(p.salt);
                totals.entry_count += 1;
            }
            None => {
                totals.skipped_lines += 1;
                if let Some(t) = line_time(line) {
                    totals.skipped_times.push(t.to_string());
                }
            }
        }
    }
    totals
}

/// The `HH:MM` inside the leading `- **…**`, when the line has one.
///
/// The caller has already checked the `- **` prefix, so this is the
/// closing `**`. Nothing validates the contents as a time: whatever stands
/// there is what the user would search their note for, which is the only
/// thing the diagnostic needs it for.
fn line_time(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("- **")?;
    let end = rest.find("**")?;
    Some(&rest[..end])
}

/// One parsed `## Food` line. The four macros default to 0.0 when their
/// token is missing (pre-existing behavior). Fiber and salt stay `None`
/// so the caller can count them as unknown instead.
struct ParsedLine {
    kcal: f64,
    protein: f64,
    carbs: f64,
    fat: f64,
    fiber: Option<f64>,
    salt: Option<f64>,
}

fn parse_food_line(line: &str) -> Option<ParsedLine> {
    match machine_segment(line) {
        // The formatter wrote this group, so the line is an entry — even
        // when the group carries no kcal. `format_nutrient_segment` emits
        // each token only when the entry has it, so a `salt:`-only food
        // (bouillon, soy sauce, a seasoning) produces a perfectly valid
        // `(2.28g salt)` group and nothing else. Requiring kcal here made
        // vitalog write a line it then refused to read, reporting `0.0g+
        // salt` for an entry whose salt it had just measured.
        //
        // The four macros fall back to the whole line when the group does
        // not carry them, because a hand edit can leave them outside it:
        // `(90 kcal) (6.0g fiber)` puts kcal in a sibling group, and
        // reading only the anchored one dropped the entry entirely where
        // every version before this counted it. Fiber and salt get no such
        // fallback — unanchored, they would forge a *measurement* out of a
        // food name (`Lightly salted chips 0.1g salt per bag`) where
        // unknown is the truthful answer. That asymmetry is the point:
        // macros behave exactly as they always have, and only the two new
        // nutrients are held to the stricter standard.
        Some(n) => {
            let macro_or_whole_line = |which: Nutrient| {
                let token = NUTRIENTS[which as usize].token;
                n[which as usize]
                    .or_else(|| extract_number_before(line, token))
                    .unwrap_or(0.0)
            };
            Some(ParsedLine {
                kcal: macro_or_whole_line(Nutrient::Kcal),
                protein: macro_or_whole_line(Nutrient::Protein),
                carbs: macro_or_whole_line(Nutrient::Carbs),
                fat: macro_or_whole_line(Nutrient::Fat),
                fiber: n[Nutrient::Fiber as usize],
                salt: n[Nutrient::Salt as usize],
            })
        }
        // No nutrient group at all — a hand-written line such as
        // `- **09:00** Banan 90 kcal`, or one whose closing paren was
        // edited away. Read the four macros off the whole line, which is
        // what every version before the anchoring did, so these keep
        // counting instead of vanishing from the day's totals.
        //
        // Fiber and salt stay `None` here even when the line names them.
        // They are the two nutrients whose token is legitimately absent
        // most of the time, so an unanchored match would forge a
        // *measurement* out of a food name (`Lightly salted chips 0.1g
        // salt per bag`) where "unknown" is the truthful answer. The
        // forgery this anchoring exists to prevent lives entirely in that
        // path; the macros were never distinguishable from unknown anyway,
        // since a missing macro token already reads as 0.0.
        None => {
            let whole_line =
                |which: Nutrient| extract_number_before(line, NUTRIENTS[which as usize].token);
            Some(ParsedLine {
                kcal: whole_line(Nutrient::Kcal)?,
                protein: whole_line(Nutrient::Protein).unwrap_or(0.0),
                carbs: whole_line(Nutrient::Carbs).unwrap_or(0.0),
                fat: whole_line(Nutrient::Fat).unwrap_or(0.0),
                fiber: None,
                salt: None,
            })
        }
    }
}

/// One nutrient, as the formatter writes it and the reader matches it.
///
/// The writer and the reader are inverses, and this table is the single
/// place that says so. `format_nutrient_segment` emits `render(value)`
/// followed by `token`; `machine_nutrients` strips `token` and asks `render`
/// itself whether those digits are what it would have written. Precision is
/// therefore stated once, in `render`, rather than restated as a range the
/// reader checks — nothing else knows the token strings, the precision, or
/// the order, so the two cannot drift apart.
///
/// They previously could, in four independent places: the formatter's
/// `format!` calls, a separate token list here, the reader's per-token
/// decimal counts, and an ordering expressed once as push order and once as
/// integer ranks. Nothing failed to compile when those disagreed and the
/// failure was silent — changing `render_salt_grams` to strip both trailing
/// zeros would have left `1.25` parsing while every whole-gram salt entry
/// read as unknown, switching the feature off for exactly the entries that
/// look most ordinary.
///
/// The order of this array *is* the order the formatter emits and the
/// reader requires.
pub(crate) struct NutrientSpec {
    pub token: &'static str,
    pub render: fn(f64) -> String,
}

/// Index into [`NUTRIENTS`], so the table and the values it describes cannot
/// be addressed inconsistently. `every_shape_the_formatter_writes_reads_back_unchanged`
/// fails if a variant and its spec ever fall out of step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nutrient {
    Kcal = 0,
    Protein = 1,
    Carbs = 2,
    Fat = 3,
    Fiber = 4,
    Salt = 5,
}

impl Nutrient {
    /// In the order the formatter emits them, which is [`NUTRIENTS`]' order.
    pub const ALL: [Nutrient; NUTRIENTS.len()] = [
        Nutrient::Kcal,
        Nutrient::Protein,
        Nutrient::Carbs,
        Nutrient::Fat,
        Nutrient::Fiber,
        Nutrient::Salt,
    ];

    pub fn spec(self) -> &'static NutrientSpec {
        &NUTRIENTS[self as usize]
    }
}

pub(crate) const NUTRIENTS: [NutrientSpec; 6] = [
    NutrientSpec {
        token: " kcal",
        render: render_kcal,
    },
    NutrientSpec {
        token: "g protein",
        render: render_grams,
    },
    NutrientSpec {
        token: "g carbs",
        render: render_grams,
    },
    NutrientSpec {
        token: "g fat",
        render: render_grams,
    },
    NutrientSpec {
        token: "g fiber",
        render: render_grams,
    },
    NutrientSpec {
        token: "g salt",
        render: render_salt_grams,
    },
];

/// kcal is written whole — the underlying figure is rounded on the way in.
fn render_kcal(v: f64) -> String {
    format!("{}", v.round() as i64)
}

/// One decimal, which is the resolution the macros and fiber are stored at.
fn render_grams(v: f64) -> String {
    format!("{v:.1}")
}

/// Salt keeps two decimals where fiber takes one, and drops a single
/// trailing zero so ordinary values still read as `2.0g salt`.
///
/// The extra digit is bought by relative error against each nutrient's goal:
/// 0.005 g is 0.014% of a 35 g fiber target but 0.083% of a 6 g salt cap, and
/// a salt figure is compared against a cap that a rounding error can push a
/// day across. Fiber entries that small are not decision-relevant.
fn render_salt_grams(v: f64) -> String {
    let s = format!("{v:.2}");
    match s.strip_suffix('0') {
        Some(trimmed) => trimmed.to_string(),
        None => s,
    }
}

/// The nutrient values in `line`, if it carries a group that
/// `format_nutrient_segment` could have written.
///
/// Locating the group is the straightforward half: the anchor is the
/// rightmost nutrient token, and the enclosing `(…)` is found by walking
/// out from it in both directions, so parentheses inside the food name are
/// harmless and an unbalanced one cannot swallow the group.
///
/// Deciding whether what it finds is a *measurement* is the half that
/// matters, and `machine_nutrients` answers it by exact match against the
/// formatter's own grammar rather than by judging the text. See there.
fn machine_segment(line: &str) -> Option<[Option<f64>; NUTRIENTS.len()]> {
    let anchor = NUTRIENTS.iter().filter_map(|n| line.rfind(n.token)).max()?;
    // The `(` the token is inside: walking left, each `)` claims the `(`
    // that matches it, so the first unclaimed `(` is the innermost opener
    // still holding the token.
    let mut depth = 0usize;
    let mut open = None;
    for (i, c) in line[..anchor].char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' if depth > 0 => depth -= 1,
            '(' => {
                open = Some(i);
                break;
            }
            _ => {}
        }
    }
    let open = open?;
    // …and the `)` that closes it: the mirror walk.
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in line[anchor..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            ')' => {
                close = Some(anchor + i);
                break;
            }
            _ => {}
        }
    }
    machine_nutrients(&line[open + 1..close?])
}

/// Parse `s` as a segment `format_nutrient_segment` could have written, or
/// return `None`.
///
/// The formatter joins `"{n} kcal"` / `"{v:.1}g protein"` / … with `", "`,
/// each token at most once and always in that order, so the grammar is
/// exact and this checks all of it: every item, its digits, and its
/// position. Nothing that is not the formatter's own output parses.
///
/// That strictness is the whole design. Deciding whether a number in a
/// hand-written line is a measurement or part of a food name is not
/// decidable from the text — `Lightly salted chips 0.1g salt per bag` and
/// `60g kolhydrater varav ~7g fiber` are the same shape — and five
/// successive attempts to draw that line each closed one case and opened
/// another. So it is not drawn: a line the formatter did not write yields
/// *unknown* fiber and salt, which is also the truthful answer, since the
/// value was never recorded on those days. The four macros still come off
/// the whole line in `parse_food_line`'s other arm, exactly as they did
/// before any of this, so nothing a legacy line used to count stops
/// counting.
fn machine_nutrients(s: &str) -> Option<[Option<f64>; NUTRIENTS.len()]> {
    let mut values: [Option<f64>; NUTRIENTS.len()] = [None; NUTRIENTS.len()];
    let mut next = 0usize;
    for item in s.split(", ") {
        // Only tokens at or after `next` are eligible, which enforces the
        // formatter's order and rejects a repeat in one test.
        let (i, spec) = NUTRIENTS
            .iter()
            .enumerate()
            .skip(next)
            .find(|(_, spec)| item.ends_with(spec.token))?;
        let digits = item.strip_suffix(spec.token)?;
        values[i] = Some(machine_digits(spec, digits)?);
        next = i + 1;
    }
    // `next` only advances when an item parsed, so this rejects a segment
    // that matched no token at all. It is not what stops `()` — an empty
    // segment yields one empty item, which matches nothing, so the `?` on
    // `find` has already returned.
    (next > 0).then_some(values)
}

/// Parse `digits` as exactly the string `spec.render` produces for the value
/// they denote, or return `None`.
///
/// Two tests, and both are load-bearing. The character test states one rule —
/// the digits must be digits — and rejects everything `format!` cannot emit:
/// a sign, whitespace, a `~`, an exponent, and the spellings of the two
/// values that would otherwise poison a total.
///
/// It cannot be dropped in favour of the re-render test alone, because
/// `render` is not injective over strings it never writes. `format!("{:.1}",
/// f64::NAN)` is `"NaN"` and `"NaN".parse()` succeeds, so `NaNg fiber`
/// re-renders equal to itself; the same holds for `infg salt`. A NaN that
/// reaches a total is worse than a wrong number, because every comparison
/// against it is false: `annotate_value` then finds the day neither below
/// its minimum nor above its maximum and prints a green `✓ over minimum`
/// for a nutrient nothing measured. A negative is refused by the same rule,
/// but nobody logs a negative quantity of food — that is a side effect, not
/// the reason this test is here.
///
/// The re-render test then requires the digits to be *precisely* what the
/// formatter writes for that value, which is what makes "the formatter wrote
/// this" literally true rather than approximately so. It is also the only
/// statement of precision anywhere: `render` is the single definition, and a
/// reader that asks it directly cannot disagree with it. Stating the decimal
/// counts separately, as this function used to, left `2.00g salt`, `0300 kcal`
/// and `300. kcal` parsing — none of which the formatter can produce — and
/// let a 400-digit kcal token through as `inf`.
fn machine_digits(spec: &NutrientSpec, digits: &str) -> Option<f64> {
    let (int, frac) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    let all_digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(int) || (!frac.is_empty() && !all_digits(frac)) {
        return None;
    }
    let value: f64 = digits.parse().ok()?;
    ((spec.render)(value) == digits).then_some(value)
}

/// Find the rightmost occurrence of `suffix` in `s`, then walk backwards
/// past whitespace to capture a number (digits + optional decimal point).
fn extract_number_before(s: &str, suffix: &str) -> Option<f64> {
    let pos = s.rfind(suffix)?;
    let before = &s.as_bytes()[..pos];
    let mut end = before.len();
    while end > 0 && before[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 {
        let c = before[start - 1];
        if c.is_ascii_digit() || c == b'.' {
            start -= 1;
        } else {
            break;
        }
    }
    if start == end {
        return None;
    }
    let value: f64 = std::str::from_utf8(&before[start..end])
        .ok()?
        .parse()
        .ok()?;
    // A run of digits long enough to overflow parses to `inf` rather than
    // failing, and an infinite macro poisons the whole day — `Today so far:
    // inf kcal`. No line the formatter wrote can reach this, so declining it
    // costs nothing and keeps the totals finite.
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_zeros() {
        assert_eq!(sum_food_section(""), FoodTotals::default());
    }

    #[test]
    fn sums_single_well_formed_line() {
        let md = "---\ndate: 2026-04-30\n---\n\n## Food\n- **12:42** Soup (500g) (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 350.0);
        assert!((r.protein - 7.0).abs() < 1e-6);
        assert!((r.carbs - 24.0).abs() < 1e-6);
        assert!((r.fat - 25.0).abs() < 1e-6);
        assert_eq!(r.entry_count, 1);
        assert_eq!(r.skipped_lines, 0);
    }

    #[test]
    fn sums_multiple_lines() {
        let md = "## Food\n- **08:00** A (100 kcal, 1.0g protein, 10.0g carbs, 2.0g fat)\n- **12:00** B (200 kcal, 5.0g protein, 20.0g carbs, 8.0g fat)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 300.0);
        assert_eq!(r.entry_count, 2);
    }

    #[test]
    fn line_missing_kcal_token_is_skipped() {
        let md = "## Food\n- **12:00** Hand-edited line with no nutrients\n";
        let r = sum_food_section(md);
        assert_eq!(r.entry_count, 0);
        assert_eq!(r.skipped_lines, 1);
    }

    #[test]
    fn line_with_only_kcal_treats_missing_macros_as_zero() {
        let md = "## Food\n- **08:00** Coffee (5 kcal)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 5.0);
        assert_eq!(r.protein, 0.0);
        assert_eq!(r.entry_count, 1);
        assert_eq!(r.skipped_lines, 0);
    }

    #[test]
    fn prose_lines_under_food_section_ignored() {
        let md = "## Food\nHad a great breakfast today.\n- **08:00** Eggs (200 kcal, 12.0g protein, 1.0g carbs, 15.0g fat)\n";
        let r = sum_food_section(md);
        assert_eq!(r.entry_count, 1);
        assert_eq!(r.skipped_lines, 0);
    }

    #[test]
    fn parses_fiber_and_salt_when_present() {
        let md = "## Food\n- **12:00** Bread (250 kcal, 9.0g protein, 45.0g carbs, 3.0g fat, 6.5g fiber, 1.2g salt)\n";
        let r = sum_food_section(md);
        assert!((r.fiber.sum - 6.5).abs() < 1e-9);
        assert!((r.salt.sum - 1.2).abs() < 1e-9);
        assert_eq!(r.fiber.unknown, 0);
        assert_eq!(r.salt.unknown, 0);
        assert!(r.fiber.is_complete());
    }

    #[test]
    fn missing_fiber_and_salt_count_as_unknown_not_zero() {
        // Every `## Food` line written before this feature looks like this.
        let md = "## Food\n- **12:42** Soup (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat)\n";
        let r = sum_food_section(md);
        assert_eq!(r.fiber.sum, 0.0);
        assert_eq!(r.fiber.unknown, 1);
        assert_eq!(r.salt.unknown, 1);
        assert!(!r.fiber.is_complete());
        assert!(!r.salt.is_complete());
    }

    #[test]
    fn fiber_present_salt_absent_tracked_separately() {
        let md = "## Food\n- **12:00** Oats (250 kcal, 9.0g protein, 45.0g carbs, 3.0g fat, 6.5g fiber)\n";
        let r = sum_food_section(md);
        assert!((r.fiber.sum - 6.5).abs() < 1e-9);
        assert_eq!(r.fiber.unknown, 0);
        assert_eq!(r.salt.sum, 0.0);
        assert_eq!(r.salt.unknown, 1);
    }

    #[test]
    fn mixed_coverage_sums_known_and_counts_unknown() {
        let md = "## Food\n\
                  - **08:00** A (100 kcal, 1.0g protein, 10.0g carbs, 2.0g fat, 2.0g fiber, 0.5g salt)\n\
                  - **12:00** B (200 kcal, 5.0g protein, 20.0g carbs, 8.0g fat)\n\
                  - **18:00** C (300 kcal, 5.0g protein, 20.0g carbs, 8.0g fat, 3.0g fiber, 1.5g salt)\n";
        let r = sum_food_section(md);
        assert_eq!(r.entry_count, 3);
        assert!((r.fiber.sum - 5.0).abs() < 1e-9);
        assert_eq!(r.fiber.unknown, 1);
        assert!((r.salt.sum - 2.0).abs() < 1e-9);
        assert_eq!(r.salt.unknown, 1);
    }

    #[test]
    fn explicit_zero_salt_is_known_not_unknown() {
        // `salt: 0` in nutrition-db.md is a measurement, not a gap.
        let md = "## Food\n- **09:00** Water (0 kcal, 0.0g protein, 0.0g carbs, 0.0g fat, 0.0g fiber, 0.0g salt)\n";
        let r = sum_food_section(md);
        assert_eq!(r.salt.sum, 0.0);
        assert_eq!(r.salt.unknown, 0);
        assert!(r.salt.is_complete());
    }

    #[test]
    fn food_name_containing_salt_does_not_shadow_the_token() {
        // The real token is inside the nutrient parenthetical, so it wins
        // over anything the free-text name happens to contain.
        let md = "## Food\n- **12:00** Chips 0.1g salt per bag (5 kcal, 0.0g protein, 0.0g carbs, 0.0g fat, 0.0g fiber, 3.0g salt)\n";
        let r = sum_food_section(md);
        assert!((r.salt.sum - 3.0).abs() < 1e-9);
        assert_eq!(r.salt.unknown, 0);
    }

    #[test]
    fn food_name_cannot_forge_an_absent_salt_token() {
        // The reachable half of the shadowing problem: the entry supplied
        // no salt, so it must count as unknown rather than pick up the
        // number sitting in the food name.
        let md = "## Food\n- **12:00** Lightly salted chips 0.1g salt per bag (500 kcal, 5.0g protein, 50.0g carbs, 30.0g fat, 40.0g fiber)\n";
        let r = sum_food_section(md);
        assert_eq!(r.salt.sum, 0.0);
        assert_eq!(r.salt.unknown, 1);
        assert!(!r.salt.is_complete());
        // Fiber was genuinely supplied and is unaffected.
        assert!((r.fiber.sum - 40.0).abs() < 1e-9);
        assert_eq!(r.fiber.unknown, 0);
    }

    #[test]
    fn food_name_cannot_forge_an_absent_fiber_token() {
        let md = "## Food\n- **12:00** Knäckebröd 6g fiber per slice (200 kcal, 4.0g protein, 35.0g carbs, 2.0g fat)\n";
        let r = sum_food_section(md);
        assert_eq!(r.fiber.sum, 0.0);
        assert_eq!(r.fiber.unknown, 1);
        assert_eq!(r.salt.unknown, 1);
    }

    #[test]
    fn parentheses_in_the_food_name_do_not_confuse_the_anchor() {
        let md = "## Food\n- **12:00** Chips (lightly salted, 0.1g salt) (500 kcal, 5.0g protein, 50.0g carbs, 30.0g fat)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 500.0);
        assert_eq!(r.entry_count, 1);
        assert_eq!(r.salt.unknown, 1);
    }

    #[test]
    fn an_unmatched_paren_in_the_name_does_not_void_the_nutrient_group() {
        // `Kvarg (Lindahls` is an ordinary product heading. Scanning left
        // to right, its `(` left the depth counter above zero for the rest
        // of the line, so the group's own `)` never closed the top level and
        // the whole group was discarded: macros still parsed off the
        // fallback, so nothing looked wrong, while that entry's fiber and
        // salt read as unknown on every day it appeared.
        let md = "## Food\n- **12:00** Kvarg (Lindahls (500g) (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat, 6.5g fiber, 1.2g salt)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 350.0);
        assert!((r.protein - 7.0).abs() < 1e-9, "got: {}", r.protein);
        assert!((r.fiber.sum - 6.5).abs() < 1e-9, "got: {:?}", r.fiber);
        assert!((r.salt.sum - 1.2).abs() < 1e-9, "got: {:?}", r.salt);
        assert_eq!(r.fiber.unknown, 0);
        assert_eq!(r.salt.unknown, 0);
        assert_eq!(r.skipped_lines, 0);
    }

    #[test]
    fn an_unmatched_paren_in_the_name_still_cannot_forge_a_token() {
        // The anchoring is what stops a food name from supplying a
        // *measurement*, and recovering the group under an unmatched `(`
        // must not weaken it: walking outward from the token stops at the
        // group's own opener, never at the one in the name.
        let md = "## Food\n- **12:00** Lightly salted chips (0.1g salt per bag (500 kcal, 5.0g protein, 50.0g carbs, 30.0g fat)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 500.0);
        assert!((r.protein - 5.0).abs() < 1e-9);
        assert_eq!(r.salt.sum, 0.0);
        assert_eq!(r.salt.unknown, 1);
    }

    #[test]
    fn an_unmatched_opener_and_a_stray_closer_cannot_forge_a_token_together() {
        // The failure the inward scan from the rightmost `)` left open: one
        // unmatched `(` in the name and one stray `)` to the right of the
        // group cancel each other, so the walk sailed past the group's own
        // `(` and anchored on the name's. `0.1g salt` — free text — was then
        // recorded as a *measurement*, with `unknown` not incremented, and a
        // `salt_max` row could print `✓ under maximum` off it. Walking
        // outward from the anchor cannot leave the group the anchor is in,
        // whatever the text on either side does. The anchor is the
        // rightmost nutrient token, which on this line is the `25.0g fat`
        // ending the group — not the ` kcal` it started out as.
        let md = "## Food\n- **12:00** Chips (0.1g salt per bag (500g) (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat) rester)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 350.0);
        assert!((r.protein - 7.0).abs() < 1e-9, "got: {}", r.protein);
        assert!((r.fat - 25.0).abs() < 1e-9, "got: {}", r.fat);
        assert_eq!(r.salt.sum, 0.0, "salt: {:?}", r.salt);
        assert_eq!(r.salt.unknown, 1, "salt: {:?}", r.salt);
        assert_eq!(r.fiber.unknown, 1);
    }

    #[test]
    fn an_unmatched_opener_and_a_stray_closer_do_not_void_a_real_group() {
        // The other half of the same shape: tightening the anchoring must
        // not cost a legitimate group its values. `Kvarg (Lindahls` in the
        // name and a hand-typed `)` after the group is exactly the pair that
        // cancelled above, and the tokens inside the group are real.
        let md = "## Food\n- **12:00** Kvarg (Lindahls (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat, 6.5g fiber, 1.2g salt) rester)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 350.0);
        assert!((r.fiber.sum - 6.5).abs() < 1e-9, "got: {:?}", r.fiber);
        assert!((r.salt.sum - 1.2).abs() < 1e-9, "got: {:?}", r.salt);
        assert_eq!(r.fiber.unknown, 0);
        assert_eq!(r.salt.unknown, 0);
        assert_eq!(r.skipped_lines, 0);
    }

    #[test]
    fn a_name_mentioning_kcal_cannot_hijack_a_group_that_has_no_kcal() {
        // kcal is the *first* token the formatter writes but not a
        // guaranteed one: a `nutrition-db.md` entry with `salt:` and no
        // `kcal:` makes vitalog itself write a kcal-less group. Anchoring
        // on ` kcal` alone walked out from the name, discarded the real
        // 5.76 g and recorded the name's 1.9 g as a measurement — a green
        // `✓ under maximum` off free text, on the number issue #39 makes
        // clinically load-bearing.
        //
        // The real group wins and the name contributes nothing: 5.76 is
        // read, 1.9 is not. kcal falls back to the whole line, which finds
        // the name's `0 kcal` — a macro read exactly as every version
        // before this one did it.
        let md = "## Food\n- **12:00** Bouillontärning (0 kcal, 1.9g salt per tärning) (12g) (5.76g salt)\n";
        let r = sum_food_section(md);
        assert!((r.salt.sum - 5.76).abs() < 1e-9, "salt: {:?}", r.salt);
        assert_eq!(r.salt.unknown, 0, "salt: {:?}", r.salt);
        assert_eq!(r.entry_count, 1);
        assert_eq!(r.skipped_lines, 0);

        // The name is what the two shapes differ by, and it does not change
        // which salt figure is read — only whether the whole-line kcal
        // fallback finds anything, which is the pre-existing macro rule.
        let control =
            "## Food\n- **12:00** Bouillontärning (1.9g salt per tärning) (12g) (5.76g salt)\n";
        let c = sum_food_section(control);
        assert!((c.salt.sum - 5.76).abs() < 1e-9, "salt: {:?}", c.salt);
        assert_eq!(c.salt.unknown, 0);
        assert_eq!(c.entry_count, 1);
        assert_eq!(c.skipped_lines, 0);
    }

    #[test]
    fn a_group_whose_own_opener_was_edited_away_cannot_forge_a_token() {
        // Position is the whole trust boundary between machine-written and
        // hand-written text on one line, and deleting the group's own `(`
        // moves it into the name: the token really is inside a group, and
        // the group really is the innermost one holding it, but its
        // contents start with free text. `0.1` was recorded as a
        // measurement. Requiring the group to open with a nutrient item —
        // which everything `format_line` writes does — rejects it.
        let md = "## Food\n- **12:00** Chips (lightly salted, 0.1g salt 350 kcal, 7.0g protein)\n";
        let r = sum_food_section(md);
        assert_eq!(r.salt.sum, 0.0, "salt: {:?}", r.salt);
        assert_eq!(r.salt.unknown, 1, "salt: {:?}", r.salt);
        // The macros still count off the whole-line fallback, as they do on
        // every other line with no readable group.
        assert_eq!(r.kcal, 350.0);
        assert!((r.protein - 7.0).abs() < 1e-9, "got: {}", r.protein);
        assert_eq!(r.entry_count, 1);
        assert_eq!(r.skipped_lines, 0);
    }

    #[test]
    fn hand_added_prose_inside_the_group_cannot_supply_a_nutrient() {
        // Every earlier round attacked the group's *boundary*. This is its
        // interior: the group really is the machine-written one, and the
        // number really is free text a hand added to it. `rfind` takes the
        // rightmost match anywhere in the segment, so checking only the
        // group's opening let both of these through as *measurements*.
        let md = "## Food\n- **08:00** Sallad (350 kcal, 7.0g protein, ca 2g salt)\n";
        let r = sum_food_section(md);
        assert_eq!(r.salt.sum, 0.0, "salt: {:?}", r.salt);
        assert_eq!(r.salt.unknown, 1, "salt: {:?}", r.salt);
        assert_eq!(r.kcal, 350.0);
        assert_eq!(r.entry_count, 1);

        let md = "## Food\n- **08:00** Gröt (350 kcal, 7.0g protein, ungefär 9g fiber)\n";
        let r = sum_food_section(md);
        assert_eq!(r.fiber.sum, 0.0, "fiber: {:?}", r.fiber);
        assert_eq!(r.fiber.unknown, 1, "fiber: {:?}", r.fiber);

        // The sharper half: a hand-added item does not merely add a value,
        // it *overrides* the machine-written one to its left. Withholding
        // both is the cost of rejecting the second, and it is the right way
        // round — 3 was never measured, and the row now says so.
        let md =
            "## Food\n- **08:00** Sallad (350 kcal, 7.0g protein, 1.2g salt, ca 3g salt extra)\n";
        let r = sum_food_section(md);
        assert_eq!(r.salt.sum, 0.0, "salt: {:?}", r.salt);
        assert_eq!(r.salt.unknown, 1, "salt: {:?}", r.salt);
        assert_eq!(r.skipped_lines, 0);
    }

    #[test]
    fn a_name_containing_kcal_does_not_move_the_anchor() {
        let md = "## Food\n- **12:00** Lo kcal bar (50g) (100 kcal, 5.0g protein, 1.5g salt)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 100.0);
        assert!((r.salt.sum - 1.5).abs() < 1e-9, "got: {:?}", r.salt);
        assert_eq!(r.salt.unknown, 0);
    }

    #[test]
    fn a_name_that_is_itself_a_valid_looking_group_does_not_win() {
        // The decoy carries every shape the real group has, including a
        // leading kcal item, and sits to the left of it.
        let md = "## Food\n- **12:00** (350 kcal, 9.9g salt) (50g) (100 kcal, 5.0g protein, 1.5g salt)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 100.0);
        assert!((r.salt.sum - 1.5).abs() < 1e-9, "got: {:?}", r.salt);
    }

    #[test]
    fn a_multibyte_name_does_not_shift_the_segment_or_panic() {
        // Every slice bound is an ASCII paren or an `rfind` result, so the
        // walks cannot land mid-character.
        let md = "## Food\n- **09:00** Knäckebröd med räksmörgås (två skivor) (210 kcal, 12.0g protein, 18.0g carbs, 9.0g fat, 3.5g fiber, 1.4g salt)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 210.0);
        assert!((r.fiber.sum - 3.5).abs() < 1e-9, "got: {:?}", r.fiber);
        assert!((r.salt.sum - 1.4).abs() < 1e-9, "got: {:?}", r.salt);
        assert_eq!(r.salt.unknown, 0);
    }

    #[test]
    fn a_trailing_aside_after_the_group_does_not_hide_it() {
        // A balanced group can sit entirely to the right of the nutrient
        // group when a note was appended by hand. The aside carries no
        // nutrient token, so the anchor stays inside the real group and the
        // outward walk closes at that group's own `)`.
        let md = "## Food\n- **12:00** Soppa (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat, 6.5g fiber, 1.2g salt) (rester)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 350.0);
        assert!((r.fiber.sum - 6.5).abs() < 1e-9, "got: {:?}", r.fiber);
        assert!((r.salt.sum - 1.2).abs() < 1e-9, "got: {:?}", r.salt);
    }

    #[test]
    fn a_stray_closing_paren_after_the_group_does_not_hide_it() {
        let md = "## Food\n- **12:00** Soppa (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat, 6.5g fiber, 1.2g salt) rester)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 350.0);
        assert!((r.salt.sum - 1.2).abs() < 1e-9, "got: {:?}", r.salt);
    }

    #[test]
    fn hand_written_line_without_a_nutrient_group_still_counts_its_macros() {
        let md = "## Food\n- **09:00** Banan 90 kcal, 1.1g protein\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 90.0);
        assert!((r.protein - 1.1).abs() < 1e-9);
        assert_eq!(r.entry_count, 1);
        assert_eq!(r.skipped_lines, 0);
    }

    #[test]
    fn line_whose_closing_paren_was_edited_away_still_counts() {
        // Shape of the one line in the real corpus that the anchoring
        // dropped: a hand-composed entry with an unclosed group and a
        // hand-written `| Totalt:` summary.
        let md = "## Food\n- **12:00** Kyckling + ris (140 kcal, 8.0g protein | Totalt: 248 kcal, 13.6g protein\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 248.0);
        assert!((r.protein - 13.6).abs() < 1e-9);
        assert_eq!(r.entry_count, 1);
    }

    #[test]
    fn unanchored_fallback_never_forges_fiber_or_salt() {
        // The fallback covers the macros only: with no nutrient group there
        // is nothing separating a measurement from the food name, and
        // "unknown" is the truthful answer for the two nutrients that are
        // legitimately absent most of the time.
        let md =
            "## Food\n- **12:00** Lightly salted chips 0.1g salt, 6g fiber per bag — 500 kcal\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 500.0);
        assert_eq!(r.salt.sum, 0.0);
        assert_eq!(r.salt.unknown, 1);
        assert_eq!(r.fiber.sum, 0.0);
        assert_eq!(r.fiber.unknown, 1);
    }

    #[test]
    fn a_macro_named_only_in_the_food_name_reads_as_it_always_has() {
        // The anchoring deliberately does *not* protect the four macros.
        // They are read off the whole line whenever the nutrient group
        // does not carry them, which is what every version before this
        // feature did, so no line that used to count stops counting and no
        // macro total moves. `20.0g protein` in the name is therefore read
        // — the same figure `main` reports for this line.
        //
        // Fiber and salt are the exception, and the reason the asymmetry
        // exists: their token is legitimately absent most of the time, so
        // an unanchored match would forge a measurement where unknown is
        // the truthful answer. See `machine_nutrients`.
        let md = "## Food\n- **08:00** Bar 20.0g protein per serving (5 kcal)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 5.0);
        assert!((r.protein - 20.0).abs() < 1e-9, "protein: {}", r.protein);
        assert_eq!(r.entry_count, 1);

        // …but a salt figure in the name still contributes nothing.
        let md = "## Food\n- **08:00** Chips 0.1g salt per bag (5 kcal)\n";
        let r = sum_food_section(md);
        assert_eq!(r.salt.sum, 0.0, "salt: {:?}", r.salt);
        assert_eq!(r.salt.unknown, 1, "salt: {:?}", r.salt);
    }

    #[test]
    fn amount_segment_is_not_mistaken_for_the_nutrient_segment() {
        let md = "## Food\n- **12:42** Soup (500g) (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat, 6.5g fiber, 1.2g salt)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 350.0);
        assert!((r.fiber.sum - 6.5).abs() < 1e-9);
        assert!((r.salt.sum - 1.2).abs() < 1e-9);
    }

    #[test]
    fn a_skipped_line_is_named_by_its_timestamp() {
        // Every skip diagnostic used to be a count and nothing else, which
        // leaves a user with a day's worth of entries to re-read to find
        // the one that was dropped. `sum_food_section` has the line in
        // hand, so the stamp costs nothing to carry.
        let md = "## Food\n- **08:00** A (100 kcal, 1.0g protein)\n- **12:00** Hand-edited, no nutrients\n- **19:30** Also broken\n";
        let r = sum_food_section(md);
        assert_eq!(r.skipped_lines, 2);
        assert_eq!(r.skipped_times, vec!["12:00".to_string(), "19:30".into()]);
        assert_eq!(
            r.skipped_note().as_deref(),
            Some("2 food lines couldn't be parsed (12:00, 19:30)")
        );

        // The count stays the authority: a line with no closing `**` has
        // no stamp to name, and the note still reports it.
        let md = "## Food\n- **12:00 unclosed and unparseable\n";
        let r = sum_food_section(md);
        assert_eq!(r.skipped_lines, 1);
        assert!(r.skipped_times.is_empty());
        assert_eq!(
            r.skipped_note().as_deref(),
            Some("1 food line couldn't be parsed")
        );

        let md = "## Food\n- **08:00** A (100 kcal, 1.0g protein)\n";
        assert_eq!(sum_food_section(md).skipped_note(), None);
    }

    #[test]
    fn skipped_lines_do_not_count_toward_unknown() {
        let md = "## Food\n- **12:00** Hand-edited line with no nutrients\n";
        let r = sum_food_section(md);
        assert_eq!(r.skipped_lines, 1);
        assert_eq!(r.fiber.unknown, 0);
        assert_eq!(r.salt.unknown, 0);
    }

    #[test]
    fn no_food_section_returns_zeros() {
        let md = "---\ndate: 2026-04-30\n---\n\n## Notes\n- Nothing\n";
        assert_eq!(sum_food_section(md), FoodTotals::default());
    }

    #[test]
    fn stops_at_next_section_heading() {
        let md = "## Food\n- **08:00** A (100 kcal, 1.0g protein, 10.0g carbs, 2.0g fat)\n## Notes\n- **09:00** B (999 kcal, 99.0g protein, 99.0g carbs, 99.0g fat)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 100.0);
        assert_eq!(r.entry_count, 1);
    }

    #[test]
    fn round_trip_with_format_line() {
        use crate::cli::food_cmd::{format_line, RenderedEntry};
        let entry = RenderedEntry {
            display_name: "Test".into(),
            amount_segment: Some((500.0, "g")),
            kcal: Some(350.0),
            protein: Some(7.0),
            carbs: Some(24.0),
            fat: Some(25.0),
            fiber: Some(6.0),
            salt: Some(4.5),
            gi: Some(40.0),
            gl: Some(10.0),
            ii: Some(35.0),
        };
        let line = format_line(&entry, "12:42");
        let md = format!("## Food\n{line}\n");
        let r = sum_food_section(&md);
        assert_eq!(r.kcal, 350.0);
        assert!((r.protein - 7.0).abs() < 1e-6);
        assert!((r.carbs - 24.0).abs() < 1e-6);
        assert!((r.fat - 25.0).abs() < 1e-6);
        assert!((r.fiber.sum - 6.0).abs() < 1e-6);
        assert!((r.salt.sum - 4.5).abs() < 1e-6);
        assert_eq!(r.fiber.unknown, 0);
        assert_eq!(r.salt.unknown, 0);
    }

    #[test]
    fn round_trip_omitted_nutrients_come_back_unknown() {
        use crate::cli::food_cmd::{format_line, RenderedEntry};
        let entry = RenderedEntry {
            display_name: "Test".into(),
            amount_segment: Some((500.0, "g")),
            kcal: Some(350.0),
            protein: Some(7.0),
            carbs: Some(24.0),
            fat: Some(25.0),
            fiber: None,
            salt: None,
            gi: None,
            gl: None,
            ii: None,
        };
        let md = format!("## Food\n{}\n", format_line(&entry, "12:42"));
        let r = sum_food_section(&md);
        assert_eq!(r.entry_count, 1);
        assert_eq!(r.fiber.unknown, 1);
        assert_eq!(r.salt.unknown, 1);
    }

    #[test]
    fn only_the_formatters_own_output_yields_fiber_and_salt() {
        // The acceptance rule in one place: a group parses iff
        // `format_nutrient_segment` could have written it — every item, its
        // digits, and its position. Salt carries one or two decimals
        // (`render_salt_grams` strips a single trailing zero); everything
        // else is fixed.
        for (label, line, fiber, salt) in [
            (
                "full panel, two-decimal salt",
                "- **08:00** Sallad (500g) (350 kcal, 7.0g protein, 24.0g carbs, 10.0g fat, 5.0g fiber, 1.25g salt)",
                Some(5.0),
                Some(1.25),
            ),
            (
                "one-decimal salt",
                "- **08:00** Sallad (350 kcal, 7.0g protein, 24.0g carbs, 10.0g fat, 1.2g salt)",
                None,
                Some(1.2),
            ),
            (
                "glycemic tail after the group",
                "- **08:00** Sallad (350 kcal, 7.0g protein, 24.0g carbs, 10.0g fat, 1.2g salt) | GI ~45, GL ~18",
                None,
                Some(1.2),
            ),
            (
                "legacy four-macro line predating the feature",
                "- **08:00** Sallad (350 kcal, 7.0g protein, 24.0g carbs, 10.0g fat)",
                None,
                None,
            ),
        ] {
            let r = sum_food_section(&format!("## Food\n{line}\n"));
            assert_eq!(r.entry_count, 1, "{label}");
            assert_eq!(r.skipped_lines, 0, "{label}");
            assert_eq!(r.kcal, 350.0, "{label}");
            match fiber {
                Some(v) => {
                    assert!((r.fiber.sum - v).abs() < 1e-9, "{label}: {:?}", r.fiber);
                    assert_eq!(r.fiber.unknown, 0, "{label}");
                }
                None => assert_eq!(r.fiber.unknown, 1, "{label}: {:?}", r.fiber),
            }
            match salt {
                Some(v) => {
                    assert!((r.salt.sum - v).abs() < 1e-9, "{label}: {:?}", r.salt);
                    assert_eq!(r.salt.unknown, 0, "{label}");
                }
                None => assert_eq!(r.salt.unknown, 1, "{label}: {:?}", r.salt),
            }
        }
    }

    #[test]
    fn a_line_the_formatter_did_not_write_keeps_its_macros_and_reports_unknown() {
        // The other half, and the reason the rule is worth its strictness:
        // no shape below can put a number the formatter did not write into
        // a total. Deciding which of these *meant* a measurement is not
        // possible from the text — `0.1g salt per bag` inside a product
        // name and `varav ~7g fiber` inside a hand-written panel are the
        // same shape — so none of them is read, and `unknown` is the
        // honest answer for a day on which nothing recorded the value.
        //
        // The four macros still come off the whole line exactly as they did
        // before any of this, so nothing that used to count stops counting.
        for (label, line, kcal, protein) in [
            (
                "prose qualifier repeating the token",
                "- **12:00** Sallad (350 kcal, 7.0g protein, 1.2g salt varav 0.2g salt fran sas)",
                350.0,
                7.0,
            ),
            (
                "prose-led item",
                "- **08:00** Sallad (350 kcal, 7.0g protein, ca 2g salt)",
                350.0,
                7.0,
            ),
            (
                "estimate markers and Swedish nutrient names",
                "- **19:30** Curry ~630g totalt (~880 kcal, 42g protein, 60g kolhydrater varav ~7g fiber, 49g fett)",
                880.0,
                42.0,
            ),
            (
                "hand-added aside inside the group",
                "- **08:00** Sallad (350 kcal (uppskattat), 7.0g protein, 24.0g carbs, 10.0g fat, 5.0g fiber, 2.0g salt)",
                350.0,
                7.0,
            ),
            (
                "nutrient named in the food name",
                "- **12:00** Chips (0.1g salt per bag (500g) (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat) rester)",
                350.0,
                7.0,
            ),
            (
                "appended second group",
                "- **12:00** Soppa (350 kcal, 7.0g protein) (0.1g salt per portion)",
                350.0,
                7.0,
            ),
            (
                "one inserted space after the opener",
                "- **12:00** Soppa ( 350 kcal, 7.0g protein, 1.2g salt)",
                350.0,
                7.0,
            ),
        ] {
            let r = sum_food_section(&format!("## Food\n{line}\n"));
            assert_eq!(r.entry_count, 1, "{label}");
            assert_eq!(r.skipped_lines, 0, "{label}");
            assert_eq!(r.kcal, kcal, "{label}");
            assert!((r.protein - protein).abs() < 1e-9, "{label}: {}", r.protein);
            assert_eq!(r.fiber.unknown, 1, "{label}: {:?}", r.fiber);
            assert_eq!(r.salt.unknown, 1, "{label}: {:?}", r.salt);
            assert_eq!(r.fiber.sum, 0.0, "{label}");
            assert_eq!(r.salt.sum, 0.0, "{label}");
        }
    }

    #[test]
    fn a_macro_in_a_sibling_group_still_counts() {
        // A hand edit can put the nutrient the formatter wrote in one group
        // and a macro in another: `(90 kcal) (6.0g fiber)`. The anchor lands
        // on the fiber group, which is a valid formatter shape, so it is
        // accepted — and reading macros only from the accepted group dropped
        // the entry entirely, taking 90 kcal out of a day that every version
        // before this counted. README documents this shape as one that keeps
        // its calories, so this pins the promise as well as the behavior.
        let md = "## Food\n- **09:00** Knäckebröd (90 kcal) (6.0g fiber)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 90.0, "the sibling group's kcal must still count");
        assert_eq!(r.entry_count, 1);
        assert_eq!(r.skipped_lines, 0);
        assert!((r.fiber.sum - 6.0).abs() < 1e-9, "fiber: {:?}", r.fiber);
        assert_eq!(r.fiber.unknown, 0);
    }

    #[test]
    fn a_kcal_less_entry_the_formatter_wrote_is_read_back() {
        // `format_nutrient_segment` emits a token only when the entry has
        // it, so a `nutrition-db.md` food with `salt:` and no `kcal:` —
        // bouillon, soy sauce, a seasoning — produces a group with salt
        // alone. Requiring kcal made vitalog write this line and then refuse
        // to read it, reporting `0.0g+ salt` for salt it had just measured,
        // and setting `skipped_lines` so every nutrient that day became a
        // lower bound.
        let entry = crate::cli::food_cmd::RenderedEntry {
            display_name: "Bouillontärning".into(),
            amount_segment: Some((12.0, "g")),
            kcal: None,
            protein: None,
            carbs: None,
            fat: None,
            fiber: Some(0.1),
            salt: Some(2.28),
            gi: None,
            gl: None,
            ii: None,
        };
        let line = crate::cli::food_cmd::format_line(&entry, "14:52");
        let r = sum_food_section(&format!("## Food\n{line}\n"));
        assert_eq!(r.entry_count, 1, "line was: {line}");
        assert_eq!(r.skipped_lines, 0, "line was: {line}");
        assert!((r.salt.sum - 2.28).abs() < 1e-9, "salt: {:?}", r.salt);
        assert_eq!(r.salt.unknown, 0);
        assert!((r.fiber.sum - 0.1).abs() < 1e-9, "fiber: {:?}", r.fiber);
        assert_eq!(r.kcal, 0.0, "no kcal was written, so none is read");
    }

    #[test]
    fn every_shape_the_formatter_writes_reads_back_unchanged() {
        // The load-bearing invariant, swept rather than sampled: whatever
        // `format_line` writes, `sum_food_section` must read back. Both
        // regressions this pins were shapes nobody had thought to write by
        // hand — a kcal-less group, and a nutrient subset — so the sweep
        // covers the option lattice instead of a few chosen lines.
        //
        // Salt is the interesting axis: `render_salt_grams` emits `{:.2}`
        // less a single stripped trailing zero, so it produces one *or* two
        // decimals and the reader has to accept both. 2.0 becomes "2.0";
        // 1.25 stays "1.25"; 0.05 stays "0.05".
        use crate::cli::food_cmd::{format_line, RenderedEntry};
        let proteins = [None, Some(0.0), Some(7.0)];
        let salts = [
            None,
            Some(0.0),
            Some(2.0),
            Some(1.25),
            Some(0.05),
            Some(10.0),
        ];
        let fibers = [None, Some(0.0), Some(6.0), Some(0.1)];
        let mut checked = 0usize;
        for kcal in [None, Some(0.0), Some(350.0)] {
            for &protein in &proteins {
                for &salt in &salts {
                    for &fiber in &fibers {
                        let e = RenderedEntry {
                            display_name: "Testmat".into(),
                            amount_segment: Some((500.0, "g")),
                            kcal,
                            protein,
                            carbs: None,
                            fat: None,
                            fiber,
                            salt,
                            gi: None,
                            gl: None,
                            ii: None,
                        };
                        let line = format_line(&e, "08:00");
                        let r = sum_food_section(&format!("## Food\n{line}\n"));
                        checked += 1;

                        // An entry with no nutrient at all writes no group,
                        // so there is nothing to read and nothing to count.
                        // `main` declines the same line for the same reason:
                        // it carries no data, rather than data that was lost.
                        if kcal.is_none() && protein.is_none() && fiber.is_none() && salt.is_none()
                        {
                            assert_eq!(r.entry_count, 0, "empty entry counted: {line}");
                            assert_eq!(r.skipped_lines, 1, "empty entry: {line}");
                            continue;
                        }

                        // Anything else the formatter wrote is an entry.
                        assert_eq!(r.entry_count, 1, "not counted: {line}");
                        assert_eq!(r.skipped_lines, 0, "skipped: {line}");
                        assert_eq!(r.kcal, kcal.unwrap_or(0.0), "kcal: {line}");
                        assert!(
                            (r.protein - protein.unwrap_or(0.0)).abs() < 1e-9,
                            "protein: {line}"
                        );

                        // Written values read back exactly; omitted ones read
                        // as unknown rather than as zero.
                        match fiber {
                            Some(v) => {
                                assert!((r.fiber.sum - v).abs() < 1e-9, "fiber: {line}");
                                assert_eq!(r.fiber.unknown, 0, "fiber unknown: {line}");
                            }
                            None => assert_eq!(r.fiber.unknown, 1, "fiber: {line}"),
                        }
                        match salt {
                            Some(v) => {
                                assert!((r.salt.sum - v).abs() < 1e-9, "salt: {line}");
                                assert_eq!(r.salt.unknown, 0, "salt unknown: {line}");
                            }
                            None => assert_eq!(r.salt.unknown, 1, "salt: {line}"),
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 3 * 3 * 6 * 4, "sweep did not cover the lattice");
    }

    #[test]
    fn salt_trims_a_single_trailing_zero() {
        assert_eq!(render_salt_grams(4.5), "4.5");
        assert_eq!(render_salt_grams(1.0), "1.0");
        assert_eq!(render_salt_grams(0.0), "0.0");
        assert_eq!(render_salt_grams(0.02), "0.02");
        assert_eq!(render_salt_grams(2.25), "2.25");
    }

    #[test]
    fn only_the_exact_digits_the_formatter_writes_are_accepted() {
        // Checking a decimal *count* rather than the rendered string left
        // three shapes parsing that `format_nutrient_segment` cannot emit,
        // so a hand-written token was read as a measurement. Asking `render`
        // directly is what makes "the formatter wrote this" literally true.
        //
        // The character test is still needed alongside it, and `NaN` is why:
        // it renders as "NaN" and parses back, so it survives a re-render
        // comparison unchanged. A NaN in a total makes every goal comparison
        // false, so the day prints a green check for a nutrient nothing
        // measured — a wrong answer that looks like a right one. `inf` is
        // the same shape. A negative falls out of the same rule and is
        // included below for completeness, not as the motivating case.
        for (label, seg) in [
            ("a second decimal salt never keeps", "300 kcal, 2.00g salt"),
            ("a leading zero on kcal", "0300 kcal, 2.0g salt"),
            ("a trailing point on kcal", "300. kcal, 2.0g salt"),
            ("a NaN, which re-renders as itself", "300 kcal, NaNg fiber"),
            ("an infinity, likewise", "300 kcal, infg salt"),
            ("a sign", "300 kcal, -5.0g salt"),
            (
                "a whole-gram salt written to two places",
                "300 kcal, 2.50g salt",
            ),
        ] {
            let md = format!("## Food\n- **08:00** X ({seg})\n");
            let r = sum_food_section(&md);
            assert_eq!(r.salt.unknown, 1, "{label}: {seg} -> {:?}", r.salt);
            assert_eq!(r.salt.sum, 0.0, "{label}: {seg}");
        }

        // …while everything the formatter does write still reads back, on
        // both sides of the trailing-zero strip.
        for (seg, expected) in [
            ("300 kcal, 2.0g salt", 2.0),
            ("300 kcal, 1.25g salt", 1.25),
            ("300 kcal, 0.05g salt", 0.05),
            ("300 kcal, 10.0g salt", 10.0),
        ] {
            let md = format!("## Food\n- **08:00** X ({seg})\n");
            let r = sum_food_section(&md);
            assert_eq!(r.salt.unknown, 0, "{seg} -> {:?}", r.salt);
            assert!(
                (r.salt.sum - expected).abs() < 1e-9,
                "{seg} -> {:?}",
                r.salt
            );
        }

        // A token so long it overflows to infinity is not a number the
        // formatter could have produced either, so the line is declined
        // rather than counted with an infinite total.
        let huge = "9".repeat(400);
        let md = format!("## Food\n- **08:00** X ({huge} kcal, 2.0g salt)\n");
        let r = sum_food_section(&md);
        assert_eq!(r.entry_count, 0, "got kcal={}", r.kcal);
        assert_eq!(r.skipped_lines, 1);
    }
}
