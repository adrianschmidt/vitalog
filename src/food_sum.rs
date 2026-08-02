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
    /// `is_lower_bound` is what two earlier rounds of review found. Off the
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
        Some(n) => Some(ParsedLine {
            kcal: n.kcal?,
            protein: n.protein.unwrap_or(0.0),
            carbs: n.carbs.unwrap_or(0.0),
            fat: n.fat.unwrap_or(0.0),
            fiber: n.fiber,
            salt: n.salt,
        }),
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
        None => Some(ParsedLine {
            kcal: extract_number_before(line, " kcal")?,
            protein: extract_number_before(line, "g protein").unwrap_or(0.0),
            carbs: extract_number_before(line, "g carbs").unwrap_or(0.0),
            fat: extract_number_before(line, "g fat").unwrap_or(0.0),
            fiber: None,
            salt: None,
        }),
    }
}

/// The six tokens `format_nutrient_segment` writes, in the order it writes
/// them. The anchor search below reads this list, so a nutrient added to
/// the formatter is added here once.
const NUTRIENT_TOKENS: [&str; 6] = [
    " kcal",
    "g protein",
    "g carbs",
    "g fat",
    "g fiber",
    "g salt",
];

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
fn machine_segment(line: &str) -> Option<MachineNutrients> {
    let anchor = NUTRIENT_TOKENS.iter().filter_map(|t| line.rfind(t)).max()?;
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

/// The six values `format_nutrient_segment` can write, each present only if
/// the formatter wrote it.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct MachineNutrients {
    kcal: Option<f64>,
    protein: Option<f64>,
    carbs: Option<f64>,
    fat: Option<f64>,
    fiber: Option<f64>,
    salt: Option<f64>,
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
fn machine_nutrients(s: &str) -> Option<MachineNutrients> {
    let mut n = MachineNutrients::default();
    let mut rank = 0u8;
    for item in s.split(", ") {
        // `{}` for kcal (`round() as i64`), `{:.1}` for the macros and
        // fiber, and `{:.2}` less a single stripped `0` for salt.
        let (value, this_rank) = if let Some(d) = item.strip_suffix(" kcal") {
            (decimals(d, 0, 0)?, 1)
        } else if let Some(d) = item.strip_suffix("g protein") {
            (decimals(d, 1, 1)?, 2)
        } else if let Some(d) = item.strip_suffix("g carbs") {
            (decimals(d, 1, 1)?, 3)
        } else if let Some(d) = item.strip_suffix("g fat") {
            (decimals(d, 1, 1)?, 4)
        } else if let Some(d) = item.strip_suffix("g fiber") {
            (decimals(d, 1, 1)?, 5)
        } else if let Some(d) = item.strip_suffix("g salt") {
            (decimals(d, 1, 2)?, 6)
        } else {
            return None;
        };
        // Strictly increasing: the formatter's order, no repeats.
        if this_rank <= rank {
            return None;
        }
        rank = this_rank;
        match this_rank {
            1 => n.kcal = Some(value),
            2 => n.protein = Some(value),
            3 => n.carbs = Some(value),
            4 => n.fat = Some(value),
            5 => n.fiber = Some(value),
            _ => n.salt = Some(value),
        }
    }
    (rank > 0).then_some(n)
}

/// Parse `s` as a non-negative decimal with between `min` and `max` digits
/// after the point, and at least one before it. Rejects a sign, a leading
/// `~`, whitespace, and anything else `format!` would not have produced.
fn decimals(s: &str, min: usize, max: usize) -> Option<f64> {
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || (!frac.is_empty() && !digits(frac)) {
        return None;
    }
    (min..=max).contains(&frac.len()).then(|| s.parse().ok())?
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
    std::str::from_utf8(&before[start..end]).ok()?.parse().ok()
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
        // `kcal:` makes vitalog itself write this line. Anchoring on
        // ` kcal` alone walked out from the name, discarded the real
        // 5.76 g and recorded the name's 1.9 g as a measurement — a green
        // `✓ under maximum` off free text, on the number issue #39 makes
        // clinically load-bearing.
        let md = "## Food\n- **12:00** Bouillontärning (0 kcal, 1.9g salt per tärning) (12g) (5.76g salt)\n";
        let r = sum_food_section(md);
        assert_eq!(r.salt.sum, 0.0, "salt: {:?}", r.salt);
        assert_eq!(r.salt.unknown, 0, "salt: {:?}", r.salt);
        assert_eq!(r.entry_count, 0);
        // The name is what the two shapes differ by, and it no longer
        // changes the outcome: both are the kcal-less group that
        // `parse_food_line` has always declined to read, counted and
        // flagged rather than guessed at.
        assert_eq!(r.skipped_lines, 1);
        let control =
            "## Food\n- **12:00** Bouillontärning (1.9g salt per tärning) (12g) (5.76g salt)\n";
        assert_eq!(sum_food_section(control), r);
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
    fn food_name_cannot_forge_a_macro_token_either() {
        // Same anchoring protects the four macros on lines that omit them.
        let md = "## Food\n- **08:00** Bar 20.0g protein per serving (5 kcal)\n";
        let r = sum_food_section(md);
        assert_eq!(r.kcal, 5.0);
        assert_eq!(r.protein, 0.0);
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
        // (`format_salt_grams` strips a single trailing zero); everything
        // else is fixed.
        for (label, line, fiber, salt) in [
            (
                "full panel, two-decimal salt",
                "- **08:00** Sallad (500g) (350 kcal, 7.0g protein, 24.0g carbs, 10.0g fat, 5.0g fiber, 2.00g salt)",
                Some(5.0),
                Some(2.0),
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
}
