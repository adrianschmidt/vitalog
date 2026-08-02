//! `vitalog food` — append a food entry to the day's `## Food` section.
//! Implementation is split across tasks: amount parsing, nutrient scaling,
//! and output formatting here; DB lookup and CLI wiring in Task 10.

use color_eyre::eyre::{bail, Result};
use color_eyre::Help;

use crate::config::Config;
use crate::db::{FoodLookup, TotalPanel};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AmountUnit {
    Gram,
    Milliliter,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Amount {
    pub value: f64,
    pub unit: AmountUnit,
}

impl Amount {
    pub fn unit_str(self) -> &'static str {
        match self.unit {
            AmountUnit::Gram => "g",
            AmountUnit::Milliliter => "ml",
        }
    }
}

/// Parse an amount with optional `g` / `ml` suffix. Bare numbers default
/// to grams. Whitespace between number and suffix is tolerated.
///
/// Non-finite input is rejected explicitly rather than left to the
/// positivity test, which `NaN` passes: every comparison against `NaN` is
/// false, so `value <= 0.0` admitted it. `f64::from_str` accepts `nan`,
/// `inf` and `infinity` — and `nang` too, since the `g` suffix is stripped
/// first — so `vitalog food "Pasta" nan --kcal 350 …` wrote a literal
/// `(NaNg)` amount into a durable note. `+inf` had the same route.
pub fn parse_amount(s: &str) -> Result<Amount> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        bail!("Invalid amount: empty.");
    }

    let lower = trimmed.to_ascii_lowercase();
    let (number_part, unit) = if let Some(rest) = lower.strip_suffix("ml") {
        (rest.trim_end(), AmountUnit::Milliliter)
    } else if let Some(rest) = lower.strip_suffix('g') {
        (rest.trim_end(), AmountUnit::Gram)
    } else {
        (lower.as_str(), AmountUnit::Gram)
    };

    let value: f64 = number_part.parse().map_err(|_| {
        color_eyre::eyre::eyre!(
            "Invalid amount: '{trimmed}'. Expected a number with optional 'g' or 'ml' suffix \
             (e.g., 500g, 250ml, or 500)."
        )
    })?;

    if !value.is_finite() || value <= 0.0 {
        return Err(color_eyre::eyre::eyre!(
            "Invalid amount: '{trimmed}'. Must be a finite positive number."
        ))
        .suggestion("Pass a positive number, e.g., 500g.");
    }

    Ok(Amount { value, unit })
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderedEntry {
    pub display_name: String,
    /// `(value, unit_str)` shown in the parens, or `None` to omit.
    pub amount_segment: Option<(f64, &'static str)>,
    pub kcal: Option<f64>,
    pub protein: Option<f64>,
    pub carbs: Option<f64>,
    pub fat: Option<f64>,
    /// `None` means the source had no value — not zero intake. Omitted
    /// from the line so `food_sum` counts it as unknown.
    pub fiber: Option<f64>,
    pub salt: Option<f64>,
    pub gi: Option<f64>,
    pub gl: Option<f64>,
    pub ii: Option<f64>,
}

/// Nutrient values supplied on the command line. All optional; the four
/// macros must be given together (`require_custom_complete`), while
/// `fiber`/`salt`/`gi`/`gl`/`ii` are independent overrides applied in both
/// custom and lookup mode.
///
/// This is the single definition of the flag set: `clap::Args` +
/// `#[command(flatten)]` on `Commands::Food` means the parser, the
/// `main.rs` destructure and this struct cannot drift apart, and a field
/// added here reaches `execute` without three separate edits.
#[derive(Debug, Clone, Copy, Default, PartialEq, clap::Args)]
pub struct NutrientArgs {
    /// Custom kcal value (skips nutrition-db lookup; requires --protein,
    /// --carbs, --fat to also be set)
    #[arg(long)]
    pub kcal: Option<f64>,
    #[arg(long)]
    pub protein: Option<f64>,
    #[arg(long)]
    pub carbs: Option<f64>,
    #[arg(long)]
    pub fat: Option<f64>,
    /// Fiber in grams for this entry (optional; overrides the
    /// nutrition-db value in lookup mode)
    #[arg(long)]
    pub fiber: Option<f64>,
    /// Salt in grams for this entry (optional; overrides the nutrition-db
    /// value in lookup mode)
    #[arg(long)]
    pub salt: Option<f64>,
    #[arg(long)]
    pub gi: Option<f64>,
    #[arg(long)]
    pub gl: Option<f64>,
    #[arg(long)]
    pub ii: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct CustomNutrients {
    pub kcal: f64,
    pub protein: f64,
    pub carbs: f64,
    pub fat: f64,
    pub fiber: Option<f64>,
    pub salt: Option<f64>,
    pub gi: Option<f64>,
    pub gl: Option<f64>,
    pub ii: Option<f64>,
}

/// Build a `RenderedEntry` from a custom-flag invocation.
pub fn render_custom(
    display_name: &str,
    amount: Option<Amount>,
    flags: CustomNutrients,
) -> RenderedEntry {
    let gl = flags.gl.or_else(|| auto_gl(flags.gi, Some(flags.carbs)));
    RenderedEntry {
        display_name: display_name.to_string(),
        amount_segment: amount.map(|a| (a.value, a.unit_str())),
        kcal: Some(flags.kcal),
        protein: Some(flags.protein),
        carbs: Some(flags.carbs),
        fat: Some(flags.fat),
        fiber: flags.fiber,
        salt: flags.salt,
        gi: flags.gi,
        gl,
        ii: flags.ii,
    }
}

/// Build a `RenderedEntry` from a nutrition-db lookup + optional amount.
/// Returns an error for invalid combinations (e.g., per_100g-only food
/// asked for in ml without a density).
pub fn render_lookup(food: &FoodLookup, amount: Option<Amount>) -> Result<RenderedEntry> {
    let entry = match amount {
        None => render_total_only(food)?,
        Some(a) => render_with_amount(food, a)?,
    };
    validate_db_nutrient(entry.kcal, "kcal", &food.name)?;
    validate_db_nutrient(entry.protein, "protein", &food.name)?;
    validate_db_nutrient(entry.carbs, "carbs", &food.name)?;
    validate_db_nutrient(entry.fat, "fat", &food.name)?;
    validate_db_nutrient(entry.fiber, "fiber", &food.name)?;
    validate_db_nutrient(entry.salt, "salt", &food.name)?;
    validate_db_nutrient(entry.gi, "gi", &food.name)?;
    validate_db_nutrient(entry.ii, "ii", &food.name)?;
    // `weight_g` is not a nutrient but reaches the note the same way: it
    // becomes the amount segment and scales `total_gl`, so a negative one
    // writes `(-462g)` and `GL ~-46.2` into a durable file.
    if let Some(total) = food.total.as_ref() {
        validate_db_nutrient(total.weight_g, "weight_g", &food.name)?;
    }
    // GL is screened at its source columns *as well as* at the resolved
    // figure. The source check is the one whose error is actionable — the
    // resolved value has three possible origins (either `gl_per_100*` key,
    // or `gi × carbs` when neither is set), so only the key can name what
    // to fix. The resolved check catches what the source check cannot: a
    // finite column scaled by a finite amount can still overflow to
    // infinity, and that product is what reaches the line.
    validate_db_nutrient(food.gl_per_100g, "gl_per_100g", &food.name)?;
    validate_db_nutrient(food.gl_per_100ml, "gl_per_100ml", &food.name)?;
    validate_db_nutrient(entry.gl, "gl", &food.name)?;
    Ok(entry)
}

fn render_total_only(food: &FoodLookup) -> Result<RenderedEntry> {
    let total = food.total.as_ref().ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "{} requires an amount (e.g., '500g' or '250ml'). It has \
             per_100g/per_100ml values but no total panel.",
            food.name
        )
    })?;
    let amount_segment = total.weight_g.map(|g| (g, "g"));
    let gi = food.gi;
    let gl = total_gl(food, total);
    Ok(RenderedEntry {
        display_name: food.name.clone(),
        amount_segment,
        kcal: total.kcal,
        protein: total.protein,
        carbs: total.carbs,
        fat: total.fat,
        fiber: total.fiber,
        salt: total.salt,
        gi,
        gl,
        ii: food.ii,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PanelKind {
    Per100g,
    Per100ml,
}

fn render_with_amount(food: &FoodLookup, amount: Amount) -> Result<RenderedEntry> {
    if food.per_100g.is_none() && food.per_100ml.is_none() && food.total.is_some() {
        eprintln!(
            "Warning: {} only has a `total` panel; ignoring amount {}{}.",
            food.name,
            amount.value,
            amount.unit_str()
        );
        return render_total_only(food);
    }

    // Resolve which panel to scale, what the scaling factor is, and which
    // panel kind was chosen (needed for correct GL lookup below).
    let (panel, factor, panel_kind) = match amount.unit {
        AmountUnit::Gram => match (&food.per_100g, &food.per_100ml, food.density_g_per_ml) {
            (Some(p), _, _) => (p, amount.value / 100.0, PanelKind::Per100g),
            (None, Some(p), Some(d)) if d > 0.0 => {
                // Solid input on liquid-only food via density: g → ml.
                let ml = amount.value / d;
                (p, ml / 100.0, PanelKind::Per100ml)
            }
            (None, Some(_), _) => {
                bail!(
                    "{} is a liquid (per_100ml only) and has no density_g_per_ml. \
                     Use ml: 'vitalog food {} {}ml'.",
                    food.name,
                    food.name,
                    amount.value
                );
            }
            (None, None, _) => bail!(
                "{} has no per_100g/per_100ml panels and no total. Cannot scale.",
                food.name
            ),
        },
        AmountUnit::Milliliter => match (&food.per_100ml, &food.per_100g, food.density_g_per_ml) {
            (Some(p), _, _) => (p, amount.value / 100.0, PanelKind::Per100ml),
            (None, Some(p), Some(d)) if d > 0.0 => {
                // Liquid input on solid-only food via density: ml → g.
                let g = amount.value * d;
                (p, g / 100.0, PanelKind::Per100g)
            }
            (None, Some(_), _) => {
                bail!(
                    "{} is a solid (per_100g only) and has no density_g_per_ml. \
                     Use grams: 'vitalog food {} {}g'.",
                    food.name,
                    food.name,
                    amount.value
                );
            }
            (None, None, _) => bail!(
                "{} has no per_100g/per_100ml panels and no total. Cannot scale.",
                food.name
            ),
        },
    };

    let kcal = panel.kcal.map(|v| v * factor);
    let protein = panel.protein.map(|v| v * factor);
    let carbs = panel.carbs.map(|v| v * factor);
    let fat = panel.fat.map(|v| v * factor);
    let fiber = panel.fiber.map(|v| v * factor);
    let salt = panel.salt.map(|v| v * factor);

    let gi = food.gi;
    // Key GL lookup on the panel actually chosen, not the input unit.
    // When density bridges the units (e.g., ml input → per_100g panel),
    // using the input unit would look up the wrong GL column.
    let gl_from_panel = match panel_kind {
        PanelKind::Per100g => food.gl_per_100g.map(|v| v * factor),
        PanelKind::Per100ml => food.gl_per_100ml.map(|v| v * factor),
    };
    let gl = gl_from_panel.or_else(|| auto_gl(gi, carbs));

    Ok(RenderedEntry {
        display_name: food.name.clone(),
        amount_segment: Some((amount.value, amount.unit_str())),
        kcal,
        protein,
        carbs,
        fat,
        fiber,
        salt,
        gi,
        gl,
        ii: food.ii,
    })
}

/// GL auto-compute from GI and carbs: `gi * carbs / 100`.
fn auto_gl(gi: Option<f64>, carbs: Option<f64>) -> Option<f64> {
    match (gi, carbs) {
        (Some(g), Some(c)) => Some(g * c / 100.0),
        _ => None,
    }
}

fn total_gl(food: &FoodLookup, total: &TotalPanel) -> Option<f64> {
    food.gl_per_100g
        .and_then(|v| total.weight_g.map(|w| v * w / 100.0))
        .or_else(|| auto_gl(food.gi, total.carbs))
}

/// Format a fully-resolved entry as the `## Food` line. Caller supplies
/// the timestamp prefix (e.g., `"12:42"`).
pub fn format_line(entry: &RenderedEntry, timestamp: &str) -> String {
    let mut line = format!("- **{timestamp}** {}", entry.display_name);

    if let Some((value, unit)) = entry.amount_segment {
        line.push_str(&format!(" ({})", format_amount(value, unit)));
    }

    let nutrients = format_nutrient_segment(entry);
    if !nutrients.is_empty() {
        line.push_str(&format!(" ({nutrients})"));
    }

    let glycemic = format_glycemic_segment(entry);
    if !glycemic.is_empty() {
        line.push_str(&format!(" | {glycemic}"));
    }

    line
}

fn format_amount(value: f64, unit: &str) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("{}{unit}", value.round() as i64)
    } else {
        format!("{value:.1}{unit}")
    }
}

fn format_nutrient_segment(entry: &RenderedEntry) -> String {
    // The one place that maps `RenderedEntry`'s fields onto the shared
    // table's order. Everything else about how a nutrient is written — its
    // token, its precision, its position — lives in `NUTRIENTS`, which
    // `machine_nutrients` reads back through, so the writer and the reader
    // cannot disagree.
    //
    // Fiber inherits `render_grams`' one decimal rather than salt's two, and
    // with it that precision's failure mode in miniature: a scaled entry
    // under 0.05 g — 0.5 g/100 g at a 5 g amount — is written `0.0g fiber`
    // and read back as a measured zero. See `render_salt_grams` for why the
    // extra digit is bought for salt and not here.
    let values = [
        entry.kcal,
        entry.protein,
        entry.carbs,
        entry.fat,
        entry.fiber,
        entry.salt,
    ];
    values
        .iter()
        .zip(crate::food_sum::NUTRIENTS.iter())
        .filter_map(|(value, spec)| value.map(|v| format!("{}{}", (spec.render)(v), spec.token)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_glycemic_segment(entry: &RenderedEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(g) = entry.gi {
        parts.push(format!("GI ~{}", round_glycemic(g)));
    }
    if let Some(g) = entry.gl {
        parts.push(format!("GL ~{}", round_glycemic_one_decimal(g)));
    }
    if let Some(g) = entry.ii {
        parts.push(format!("II ~{}", round_glycemic(g)));
    }
    parts.join(", ")
}

fn round_glycemic(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

fn round_glycemic_one_decimal(v: f64) -> String {
    format!("{v:.1}")
}

pub fn execute(
    name: &str,
    amount: Option<&str>,
    nutrients: NutrientArgs,
    date_flag: Option<&str>,
    time_flag: Option<&str>,
    config: &Config,
    quiet: bool,
) -> Result<()> {
    if name.trim().is_empty() {
        bail!("Food name required.");
    }
    // A `## Food` line is one physical line, and `append_line_to_section`
    // inserts the name verbatim. A newline in it therefore writes a second
    // line that the parser reads as its own entry — and since the forged
    // line ends before the real nutrient group, its fiber and salt parse as
    // a *measured* zero, which is precisely the state the unknown counting
    // exists to keep out of the totals. The real entry, meanwhile,
    // disappears from all three counters.
    if name.contains('\n') {
        bail!("Food name must be a single line.");
    }

    let amt = match amount {
        Some(s) => Some(parse_amount(s)?),
        None => None,
    };

    // Every numeric flag, not just the two this feature added: they all
    // reach the markdown line through the same formatter, and the six that
    // are read back are read by the same digit walk. Screened before
    // anything is written, and above the `any_macro` branch so both modes
    // are covered.
    //
    // The list is every flag rather than every flag `food_sum` parses,
    // because "nothing reads it back today" is not a property worth
    // encoding: `GI ~-50` and `GI ~NaN` are just as wrong in a durable note
    // as `-50.0g protein`, and a future reader of the glycemic tokens would
    // otherwise inherit the same sign flip that this guard exists to close.
    validate_nutrient_flag(nutrients.kcal, "--kcal")?;
    validate_nutrient_flag(nutrients.protein, "--protein")?;
    validate_nutrient_flag(nutrients.carbs, "--carbs")?;
    validate_nutrient_flag(nutrients.fat, "--fat")?;
    validate_nutrient_flag(nutrients.fiber, "--fiber")?;
    validate_nutrient_flag(nutrients.salt, "--salt")?;
    validate_nutrient_flag(nutrients.gi, "--gi")?;
    validate_nutrient_flag(nutrients.gl, "--gl")?;
    validate_nutrient_flag(nutrients.ii, "--ii")?;

    let date = crate::cli::resolve::target_date(date_flag, config)?;
    let date_str = date.format("%Y-%m-%d").to_string();
    let when = crate::cli::resolve::target_time(time_flag)?;
    let formatted_time = crate::time::format_time(when, config.time_format);

    let any_macro = nutrients.kcal.is_some()
        || nutrients.protein.is_some()
        || nutrients.carbs.is_some()
        || nutrients.fat.is_some();
    let entry = if any_macro {
        let custom = require_custom_complete(&nutrients)?;
        render_custom(name, amt, custom)
    } else {
        let lookup = lookup_food(config, name)?;
        let mut entry = render_lookup(&lookup, amt)?;
        apply_lookup_overrides(&mut entry, &nutrients);
        entry
    };

    let line = format_line(&entry, &formatted_time);

    let note_path = config.notes_dir_path().join(format!("{date_str}.md"));
    let content = if note_path.exists() {
        std::fs::read_to_string(&note_path)?
    } else {
        crate::template::render_daily_note(&date_str, config)
    };
    let updated = crate::body::ensure_section(&content, "Food");
    let updated = crate::body::append_line_to_section(&updated, "Food", &line);
    crate::frontmatter::atomic_write(&note_path, &updated)?;

    if quiet {
        eprintln!(
            "Food logged: {date_str} {formatted_time} {}",
            entry.display_name
        );
    } else {
        let totals = crate::food_sum::sum_food_section(&updated);
        eprintln!("Food logged: {date_str} {formatted_time}");
        eprintln!("  {line}");
        eprintln!();
        eprintln!("Today so far: {}", format_food_totals(&totals));
    }
    Ok(())
}

fn format_food_totals(t: &crate::food_sum::FoodTotals) -> String {
    let mut out = format!(
        "{} kcal, {}g protein, {}g carbs, {}g fat, {}, {}",
        t.kcal.round() as i64,
        t.protein.round() as i64,
        t.carbs.round() as i64,
        t.fat.round() as i64,
        format_nutrient_total(&t.fiber, "fiber", t.entry_count, t.skipped_lines),
        format_nutrient_total(&t.salt, "salt", t.entry_count, t.skipped_lines),
    );
    // A skipped line's calories are missing from every number above. This
    // is the surface the user is looking at right after logging, so it has
    // to say so — `vitalog today` already does, in the same words.
    //
    // Set off with a dash rather than parenthesized: a nutrient total ends
    // in its own `(2 unknown)` parenthetical, and a second bare
    // parenthetical straight after it reads as though it also scoped to
    // that nutrient. This one scopes to the whole line.
    if let Some(note) = t.skipped_note() {
        out.push_str(&format!(" — {note}"));
    }
    out
}

/// Render a nutrient whose coverage may be partial. Exact when every entry
/// supplied a value, a `+`-marked lower bound when only some did, and an
/// explicit "unknown" when none did — a bare `0.0g` there would claim an
/// intake the data doesn't support. One decimal throughout: integer
/// rounding would destroy salt, whose interesting range is 0.4–8 g.
///
/// A food line the parser dropped costs the total just as much as a
/// missing token does, so it marks the figure a lower bound too. It has no
/// per-entry count to attach — `sum_food_section` counts it in neither
/// `entry_count` nor `unknown` — so the `+` stands alone and the
/// line-scoped note that follows carries the number. `today`'s dashboard
/// row calls the same `NutrientTotal::is_lower_bound`, so the two text
/// surfaces cannot disagree about whether a total is exact.
fn format_nutrient_total(
    total: &crate::food_sum::NutrientTotal,
    label: &str,
    entry_count: usize,
    skipped_lines: usize,
) -> String {
    if !total.is_lower_bound(skipped_lines) {
        return format!("{:.1}g {label}", total.sum);
    }
    if total.unknown > 0 && !total.is_measured(entry_count) {
        let noun = if total.unknown == 1 {
            "entry"
        } else {
            "entries"
        };
        return format!("{label} unknown ({} {noun})", total.unknown);
    }
    if total.unknown > 0 {
        return format!("{:.1}g+ {label} ({} unknown)", total.sum, total.unknown);
    }
    format!("{:.1}g+ {label}", total.sum)
}

/// Reject numeric flag values that cannot survive the round-trip through
/// markdown.
///
/// A negative value writes `-3.5g fiber` and reads back as a *positive*
/// 3.5, because the backward digit walk in
/// `food_sum::extract_number_before` stops at the minus sign — a 7 g typo
/// on `--protein` is a 14 g error in the day's total, silent on every
/// surface. NaN and infinity write literal `NaNg fiber` / `infg salt` into
/// a durable note, which the same walk then reads as no token at all. Zero
/// is allowed: an explicit `0.0g salt` is a measurement, not a gap. Finite
/// magnitude is not screened. `parse_amount` screens the *amount* on the
/// same two grounds and rejects zero as well — a zero-gram entry is a
/// mistyped amount rather than a measurement of nothing.
///
/// It covers all nine numeric flags. The read-back failure is a property of
/// `extract_number_before`, which every token `food_sum` parses is parsed
/// by, so it never was fiber- and salt-specific; the four macros were
/// simply unguarded before those two arrived. The three glycemic flags are
/// not read back by anything today, so for them this is only about what
/// gets written — `GI ~-50` in a note is wrong whether or not a parser
/// currently cares. Where the corruption costs most is fiber and salt: they
/// read back as a *known* measurement, so a sign flip there also spends the
/// "this entry was measured" marking on a wrong number.
///
/// The sign test is `is_sign_negative` rather than `< 0.0` so that `-0.0`
/// is caught too. It compares equal to zero, so a `v < 0.0` screen admits
/// it and `-0.0g salt` reaches the note — harmless arithmetically (it
/// re-parses as `+0.0`) but a stray minus sign in durable storage, which
/// is what this guard exists to keep out.
fn validate_nutrient_flag(value: Option<f64>, flag: &str) -> Result<()> {
    let Some(v) = value else { return Ok(()) };
    if !v.is_finite() {
        return Err(color_eyre::eyre::eyre!(
            "Invalid {flag} value: '{v}'. Must be a finite number."
        ))
        .suggestion(format!("Pass a finite number, e.g., {flag} 4.2."));
    }
    if v.is_sign_negative() {
        return Err(color_eyre::eyre::eyre!(
            "Invalid {flag} value: '{v}'. Must be zero or a positive number."
        ))
        .suggestion(format!("Pass zero or a positive number, e.g., {flag} 4.2."));
    }
    Ok(())
}

/// The same screen as `validate_nutrient_flag`, applied to the values a
/// `nutrition-db.md` panel produced.
///
/// The flag path and the db path reach the same markdown token, so they
/// need the same guard: a negative `salt:` in the db writes `-1.0g salt`
/// and reads back as a *positive* 1.0 marked complete — a sign flip that
/// the "known measurement" marking makes worse than either half alone.
/// Applied after scaling on the six nutrients that scale, so it also
/// catches an amount factor that pushed a finite panel value to infinity;
/// `gi`, `ii` and the two `gl_per_100*` columns are passed raw, because
/// nothing scales them. And `is_sign_negative` rather than `>= 0.0`, so a
/// `-0.0` panel value cannot slip a stray minus into a note.
/// It screens the four macros and the three glycemic values for the same
/// reason the flag path does — and the argument is stronger on this side:
/// one bad key in a file that is read on every logging of that food, not
/// one bad flag typed once. Nothing parses `GI ~-50` back today, so for the
/// glycemic three this is about what reaches the note, exactly as it is for
/// `--gi/--gl/--ii`.
///
/// It runs inside `render_lookup`, before `apply_lookup_overrides`, so a
/// `--salt` on the command line does not rescue a bad `salt:` in the db —
/// deliberately. `render_lookup` is the choke point every db-derived entry
/// passes through (`render_total_only`, `render_with_amount` and the
/// density bridge), and a corrupt db value is a file the user wants to
/// hear about rather than paper over for one invocation; the suggestion
/// points at the file, which is where the fix belongs.
fn validate_db_nutrient(value: Option<f64>, field: &str, food: &str) -> Result<()> {
    let Some(v) = value else { return Ok(()) };
    if v.is_finite() && !v.is_sign_negative() {
        return Ok(());
    }
    // `v` is what this entry produced, and where it came from differs by
    // field: the six that scale with the amount arrive here post-scaling
    // and need not equal what the file says, while `gi`, `ii` and the two
    // `gl_per_100*` columns are the file's own numbers unchanged. Report
    // the value without claiming either, rather than quoting all ten back
    // as if they were the key — the suggestion names the key to open.
    Err(color_eyre::eyre::eyre!(
        "Invalid {field} for '{food}': {v}. The `{field}:` value in \
         nutrition-db.md must be zero or a positive finite number."
    ))
    .suggestion(format!(
        "Fix the `{field}:` key for '{food}' in nutrition-db.md."
    ))
}

fn require_custom_complete(n: &NutrientArgs) -> Result<CustomNutrients> {
    Ok(CustomNutrients {
        kcal: n.kcal.ok_or_else(missing_macros_err)?,
        protein: n.protein.ok_or_else(missing_macros_err)?,
        carbs: n.carbs.ok_or_else(missing_macros_err)?,
        fat: n.fat.ok_or_else(missing_macros_err)?,
        fiber: n.fiber,
        salt: n.salt,
        gi: n.gi,
        gl: n.gl,
        ii: n.ii,
    })
}

fn missing_macros_err() -> color_eyre::eyre::Report {
    color_eyre::eyre::eyre!(
        "Custom mode requires --kcal, --protein, --carbs, and --fat together. \
         Optional flags: --fiber, --salt, --gi, --gl, --ii."
    )
}

/// Apply the explicit non-macro overrides (--fiber / --salt / --gi / --gl
/// / --ii) to a `RenderedEntry` from lookup mode.
///
/// `--fiber` / `--salt` are absolute gram values for the whole entry, not
/// per-100 g figures: they replace whatever the panel scaling produced
/// rather than being multiplied by the amount factor. That matches how the
/// same flags behave in custom mode and is the only reading under which
/// filling in a gap for a db food is usable — 77 of 106 db entries carry
/// no `fiber:` key, so supplying the missing number for a food that *is*
/// in the db is the main reason to reach for the flag.
///
/// If --gi changes the gi value and --gl was not given, re-runs the GL
/// auto-compute cascade with the new gi.
fn apply_lookup_overrides(entry: &mut RenderedEntry, n: &NutrientArgs) {
    if let Some(v) = n.fiber {
        entry.fiber = Some(v);
    }
    if let Some(v) = n.salt {
        entry.salt = Some(v);
    }
    if let Some(v) = n.gi {
        entry.gi = Some(v);
    }
    if let Some(v) = n.ii {
        entry.ii = Some(v);
    }
    if let Some(v) = n.gl {
        entry.gl = Some(v);
    } else if n.gi.is_some() {
        // --gi overrode gi; re-apply auto-compute when GL has no
        // explicit override. This ensures GL reflects the new gi.
        if let Some(carbs) = entry.carbs {
            if let Some(new_gi) = entry.gi {
                entry.gl = Some(new_gi * carbs / 100.0);
            }
        }
    }
}

fn lookup_food(config: &Config, name: &str) -> Result<FoodLookup> {
    let db_path = config.db_path();
    if !db_path.exists() {
        return Err(color_eyre::eyre::eyre!(
            "Database not found at {}. Run 'vitalog init' or 'vitalog sync' first, \
             or pass --kcal/--protein/--carbs/--fat for a one-off entry.",
            db_path.display()
        ));
    }

    let conn = crate::db::open_ro(&db_path)?;
    crate::db::lookup_food_by_name_or_alias(&conn, name)?.ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "No nutrition entry for '{name}'. Add it to nutrition-db.md, \
             use a known alias, or pass --kcal/--protein/--carbs/--fat for a one-off."
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::{FoodLookup, NutrientPanel, TotalPanel};
    use crate::food_sum::{FoodTotals, NutrientTotal};

    fn lookup_per_100g() -> FoodLookup {
        FoodLookup {
            id: 1,
            name: "Kelda Skogssvampsoppa".into(),
            per_100g: Some(NutrientPanel {
                kcal: Some(70.0),
                protein: Some(1.4),
                carbs: Some(4.8),
                fat: Some(5.0),
                sat_fat: None,
                sugar: None,
                salt: None,
                fiber: None,
            }),
            per_100ml: None,
            density_g_per_ml: None,
            total: None,
            gi: Some(40.0),
            gl_per_100g: Some(2.0),
            gl_per_100ml: None,
            ii: Some(35.0),
            description: None,
            notes: None,
        }
    }

    fn lookup_per_100ml_with_density() -> FoodLookup {
        FoodLookup {
            id: 2,
            name: "Helmjölk".into(),
            per_100g: None,
            per_100ml: Some(NutrientPanel {
                kcal: Some(62.0),
                protein: Some(3.4),
                carbs: Some(4.8),
                fat: Some(3.0),
                sat_fat: None,
                sugar: None,
                salt: None,
                fiber: None,
            }),
            density_g_per_ml: Some(1.03),
            total: None,
            gi: Some(30.0),
            gl_per_100g: None,
            gl_per_100ml: None,
            ii: Some(90.0),
            description: None,
            notes: None,
        }
    }

    fn lookup_total_panel() -> FoodLookup {
        FoodLookup {
            id: 3,
            name: "Te, Earl Grey, hot".into(),
            per_100g: None,
            per_100ml: None,
            density_g_per_ml: None,
            total: Some(TotalPanel {
                weight_g: Some(200.0),
                kcal: Some(2.0),
                protein: Some(0.0),
                carbs: Some(0.4),
                fat: Some(0.0),
                sat_fat: None,
                sugar: None,
                salt: None,
                fiber: None,
            }),
            gi: None,
            gl_per_100g: None,
            gl_per_100ml: None,
            ii: None,
            description: None,
            notes: None,
        }
    }

    #[test]
    fn lookup_solid_with_grams_scales_per_100g() {
        let f = lookup_per_100g();
        let amt = parse_amount("500g").unwrap();
        let r = render_lookup(&f, Some(amt)).unwrap();
        assert_eq!(r.kcal, Some(350.0));
        assert!((r.protein.unwrap() - 7.0).abs() < 1e-9);
        assert_eq!(r.gl, Some(10.0));
        assert_eq!(r.gi, Some(40.0));
        assert_eq!(r.amount_segment, Some((500.0, "g")));
    }

    #[test]
    fn lookup_liquid_with_ml_scales_per_100ml() {
        let f = lookup_per_100ml_with_density();
        let amt = parse_amount("250ml").unwrap();
        let r = render_lookup(&f, Some(amt)).unwrap();
        assert_eq!(r.kcal, Some(155.0));
        assert!((r.protein.unwrap() - 8.5).abs() < 1e-9);
        assert_eq!(r.amount_segment, Some((250.0, "ml")));
    }

    #[test]
    fn lookup_solid_with_ml_uses_density() {
        // Build a solid with density to allow ml input via conversion.
        let mut f = lookup_per_100g();
        f.density_g_per_ml = Some(1.0);
        let amt = parse_amount("100ml").unwrap();
        let r = render_lookup(&f, Some(amt)).unwrap();
        // 100ml * 1.0 = 100g; same as 100g of soup.
        assert_eq!(r.kcal, Some(70.0));
        assert_eq!(r.amount_segment, Some((100.0, "ml")));
    }

    #[test]
    fn lookup_solid_with_ml_no_density_errors() {
        let f = lookup_per_100g();
        let amt = parse_amount("100ml").unwrap();
        let err = render_lookup(&f, Some(amt)).unwrap_err();
        assert!(err.to_string().contains("density"), "got: {err}");
    }

    #[test]
    fn lookup_total_panel_no_amount_uses_totals() {
        let f = lookup_total_panel();
        let r = render_lookup(&f, None).unwrap();
        assert_eq!(r.kcal, Some(2.0));
        assert_eq!(r.amount_segment, Some((200.0, "g")));
    }

    #[test]
    fn lookup_total_panel_no_amount_no_weight_g_omits_amount() {
        let mut f = lookup_total_panel();
        f.total.as_mut().unwrap().weight_g = None;
        let r = render_lookup(&f, None).unwrap();
        assert!(r.amount_segment.is_none());
    }

    #[test]
    fn lookup_per_100g_no_amount_errors() {
        let f = lookup_per_100g();
        let err = render_lookup(&f, None).unwrap_err();
        assert!(err.to_string().contains("requires an amount"));
    }

    #[test]
    fn custom_with_gi_carbs_no_gl_autocomputes() {
        let r = render_custom(
            "Random pasta",
            Some(parse_amount("500g").unwrap()),
            CustomNutrients {
                kcal: 350.0,
                protein: 7.0,
                carbs: 24.0,
                fat: 25.0,
                fiber: None,
                salt: None,
                gi: Some(50.0),
                gl: None,
                ii: None,
            },
        );
        assert_eq!(r.gl, Some(12.0));
        assert_eq!(r.gi, Some(50.0));
    }

    #[test]
    fn format_line_full_lookup() {
        let f = lookup_per_100g();
        let r = render_lookup(&f, Some(parse_amount("500g").unwrap())).unwrap();
        let line = format_line(&r, "12:42");
        assert_eq!(
            line,
            "- **12:42** Kelda Skogssvampsoppa (500g) (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat) | GI ~40, GL ~10.0, II ~35"
        );
    }

    #[test]
    fn format_line_omits_glycemic_when_absent() {
        let r = render_custom(
            "Random pasta",
            Some(parse_amount("500g").unwrap()),
            CustomNutrients {
                kcal: 350.0,
                protein: 7.0,
                carbs: 24.0,
                fat: 25.0,
                fiber: None,
                salt: None,
                gi: None,
                gl: None,
                ii: None,
            },
        );
        let line = format_line(&r, "13:00");
        assert!(!line.contains('|'), "got: {line}");
        assert!(line.contains("(350 kcal"));
    }

    #[test]
    fn format_line_glycemic_partial() {
        let r = render_custom(
            "Pasta",
            Some(parse_amount("500g").unwrap()),
            CustomNutrients {
                kcal: 350.0,
                protein: 7.0,
                carbs: 24.0,
                fat: 25.0,
                fiber: None,
                salt: None,
                gi: Some(50.0),
                gl: None,
                ii: None,
            },
        );
        let line = format_line(&r, "13:00");
        assert!(line.contains("| GI ~50, GL ~12.0"));
        assert!(!line.contains("II"));
    }

    #[test]
    fn format_line_total_panel_no_amount_no_parens() {
        let mut f = lookup_total_panel();
        f.total.as_mut().unwrap().weight_g = None;
        let r = render_lookup(&f, None).unwrap();
        let line = format_line(&r, "14:50");
        // No `(...g)` segment when weight_g is missing.
        assert!(
            line.starts_with("- **14:50** Te, Earl Grey, hot ("),
            "expected nutrient segment to start; got: {line}"
        );
        // The opening paren after the name should be the nutrient segment.
        let after_name = line.trim_start_matches("- **14:50** Te, Earl Grey, hot ");
        assert!(after_name.starts_with("(2 kcal"), "got: {after_name}");
    }

    #[test]
    fn lookup_scales_fiber_and_salt_with_amount() {
        let mut f = lookup_per_100g();
        {
            let p = f.per_100g.as_mut().unwrap();
            p.fiber = Some(1.2);
            p.salt = Some(0.9);
        }
        let r = render_lookup(&f, Some(parse_amount("500g").unwrap())).unwrap();
        assert!((r.fiber.unwrap() - 6.0).abs() < 1e-9);
        assert!((r.salt.unwrap() - 4.5).abs() < 1e-9);
    }

    #[test]
    fn lookup_without_fiber_or_salt_yields_none() {
        let f = lookup_per_100g(); // panel has neither
        let r = render_lookup(&f, Some(parse_amount("500g").unwrap())).unwrap();
        assert_eq!(r.fiber, None);
        assert_eq!(r.salt, None);
    }

    #[test]
    fn density_bridge_scales_fiber_from_the_chosen_panel() {
        let mut f = lookup_per_100g();
        f.density_g_per_ml = Some(1.0);
        f.per_100g.as_mut().unwrap().fiber = Some(2.0);
        // 200ml * 1.0 g/ml = 200g → factor 2.0.
        let r = render_lookup(&f, Some(parse_amount("200ml").unwrap())).unwrap();
        assert!((r.fiber.unwrap() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn small_salt_values_survive_the_round_trip_through_markdown() {
        // At one decimal this entry writes `0.0g salt` and reads back as a
        // known zero — a measurement destroyed on write.
        let entry = RenderedEntry {
            display_name: "ICA Salsiccia".into(),
            amount_segment: None,
            kcal: Some(251.0),
            protein: Some(12.0),
            carbs: Some(3.4),
            fat: Some(21.0),
            fiber: None,
            salt: Some(0.02),
            gi: None,
            gl: None,
            ii: None,
        };
        let line = format_line(&entry, "15:13");
        assert!(line.contains("0.02g salt"), "got: {line}");

        let totals = crate::food_sum::sum_food_section(&format!("## Food\n{line}\n"));
        assert!((totals.salt.sum - 0.02).abs() < 1e-9);
        assert_eq!(totals.salt.unknown, 0);
    }

    #[test]
    fn total_panel_passes_fiber_and_salt_through_unscaled() {
        let mut f = lookup_total_panel();
        {
            let t = f.total.as_mut().unwrap();
            t.fiber = Some(0.4);
            t.salt = Some(0.02);
        }
        let r = render_lookup(&f, None).unwrap();
        assert_eq!(r.fiber, Some(0.4));
        assert_eq!(r.salt, Some(0.02));
    }

    #[test]
    fn format_line_appends_fiber_then_salt_after_fat() {
        let mut f = lookup_per_100g();
        {
            let p = f.per_100g.as_mut().unwrap();
            p.fiber = Some(1.2);
            p.salt = Some(0.9);
        }
        let r = render_lookup(&f, Some(parse_amount("500g").unwrap())).unwrap();
        assert_eq!(
            format_line(&r, "12:42"),
            "- **12:42** Kelda Skogssvampsoppa (500g) (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat, 6.0g fiber, 4.5g salt) | GI ~40, GL ~10.0, II ~35"
        );
    }

    #[test]
    fn format_line_omits_fiber_and_salt_tokens_when_unknown() {
        // The omission is load-bearing: it is what makes food_sum count
        // the entry as unknown rather than as zero intake.
        let f = lookup_per_100g();
        let r = render_lookup(&f, Some(parse_amount("500g").unwrap())).unwrap();
        let line = format_line(&r, "12:42");
        assert!(!line.contains("fiber"), "got: {line}");
        assert!(!line.contains("salt"), "got: {line}");
    }

    #[test]
    fn format_line_emits_fiber_without_salt() {
        let mut f = lookup_per_100g();
        f.per_100g.as_mut().unwrap().fiber = Some(1.0);
        let r = render_lookup(&f, Some(parse_amount("100g").unwrap())).unwrap();
        let line = format_line(&r, "12:42");
        assert!(line.contains("1.0g fiber"), "got: {line}");
        assert!(!line.contains("salt"), "got: {line}");
    }

    #[test]
    fn parse_grams_with_suffix() {
        let a = parse_amount("500g").unwrap();
        assert_eq!(a.value, 500.0);
        assert_eq!(a.unit, AmountUnit::Gram);
    }

    #[test]
    fn parse_ml_with_suffix() {
        let a = parse_amount("250ml").unwrap();
        assert_eq!(a.value, 250.0);
        assert_eq!(a.unit, AmountUnit::Milliliter);
    }

    #[test]
    fn parse_bare_number_defaults_to_grams() {
        let a = parse_amount("500").unwrap();
        assert_eq!(a.value, 500.0);
        assert_eq!(a.unit, AmountUnit::Gram);
    }

    #[test]
    fn parse_decimal_amount() {
        let a = parse_amount("12.5g").unwrap();
        assert_eq!(a.value, 12.5);
        assert_eq!(a.unit, AmountUnit::Gram);
    }

    #[test]
    fn parse_uppercase_suffix() {
        let a = parse_amount("250ML").unwrap();
        assert_eq!(a.unit, AmountUnit::Milliliter);
    }

    #[test]
    fn parse_with_internal_space() {
        let a = parse_amount("500 g").unwrap();
        assert_eq!(a.value, 500.0);
        assert_eq!(a.unit, AmountUnit::Gram);
    }

    #[test]
    fn parse_garbage_errors() {
        assert!(parse_amount("500abc").is_err());
        assert!(parse_amount("abc").is_err());
        assert!(parse_amount("").is_err());
    }

    #[test]
    fn parse_negative_or_zero_errors() {
        assert!(parse_amount("-5g").is_err());
        assert!(parse_amount("0g").is_err());
    }

    #[test]
    fn parse_non_finite_amount_errors() {
        // `value <= 0.0` is false for NaN — every comparison against it is —
        // so these reached the note as a literal `(NaNg)` / `(infg)` amount
        // segment. `nang` gets there too: the `g` suffix is stripped before
        // the parse, leaving `nan`.
        for bad in ["nan", "NaN", "nang", "inf", "infinity", "INF", "infg"] {
            assert!(parse_amount(bad).is_err(), "accepted: {bad}");
        }
    }

    #[test]
    fn lookup_density_bridge_uses_correct_gl_panel() {
        // per_100g-only food with gl_per_100g set, queried with ml input.
        // Without the fix, GL would be looked up on gl_per_100ml (None),
        // dropped, and auto-compute would only rescue if gi is set.
        // Strip gi to ensure auto-compute can't mask the bug.
        let mut f = lookup_per_100g();
        f.density_g_per_ml = Some(1.0);
        f.gi = None;
        let amt = parse_amount("200ml").unwrap();
        let r = render_lookup(&f, Some(amt)).unwrap();
        // gl_per_100g = 2.0; 200ml * 1.0 g/ml = 200g; factor = 200/100 = 2.
        // Expected GL = 2.0 * 2.0 = 4.0.
        assert_eq!(
            r.gl,
            Some(4.0),
            "expected per_100g GL to be used in density-bridge"
        );
    }

    fn config_in(notes_dir: &std::path::Path) -> Config {
        let toml_str = format!(
            "notes_dir = '{}'\ntime_format = '24h'\n",
            notes_dir.display().to_string().replace('\\', "/")
        );
        toml::from_str(&toml_str).unwrap()
    }

    fn read_today(notes_dir: &std::path::Path, config: &Config) -> String {
        let date = config.effective_today();
        std::fs::read_to_string(notes_dir.join(format!("{date}.md"))).unwrap()
    }

    fn populate_db(config: &Config) {
        let db_path = config.db_path();
        let conn = db::open_rw(&db_path).unwrap();
        db::init_db(&conn, &[]).unwrap();
        db::insert_food(
            &conn,
            &db::FoodInsert {
                name: "Kelda Skogssvampsoppa".into(),
                per_100g: Some(NutrientPanel {
                    kcal: Some(70.0),
                    protein: Some(1.4),
                    carbs: Some(4.8),
                    fat: Some(5.0),
                    sat_fat: None,
                    sugar: None,
                    salt: None,
                    fiber: None,
                }),
                per_100ml: None,
                density_g_per_ml: None,
                total: None,
                gi: Some(40.0),
                gl_per_100g: Some(2.0),
                gl_per_100ml: None,
                ii: Some(35.0),
                description: None,
                notes: None,
                aliases: vec!["kelda skogssvampsoppa".into()],
                ingredients: vec![],
            },
        )
        .unwrap();
    }

    #[test]
    fn execute_lookup_writes_food_section_and_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        populate_db(&config);

        execute(
            "kelda skogssvampsoppa",
            Some("500g"),
            NutrientArgs::default(),
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(note.contains("## Food"), "got:\n{note}");
        assert!(
            note.contains("- **12:42** Kelda Skogssvampsoppa (500g) (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat) | GI ~40, GL ~10.0, II ~35"),
            "got:\n{note}"
        );
    }

    #[test]
    fn execute_custom_mode_works_without_db() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        // No populate_db — custom mode shouldn't need it.

        execute(
            "Random pasta",
            Some("500g"),
            NutrientArgs {
                kcal: Some(350.0),
                protein: Some(7.0),
                carbs: Some(24.0),
                fat: Some(25.0),
                gi: Some(50.0),
                ..Default::default()
            },
            None,
            Some("13:00"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(note.contains("- **13:00** Random pasta (500g) (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat) | GI ~50, GL ~12.0"), "got:\n{note}");
    }

    #[test]
    fn custom_mode_writes_fiber_and_salt_flags() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        execute(
            "Restaurant pasta",
            Some("400g"),
            NutrientArgs {
                kcal: Some(620.0),
                protein: Some(22.0),
                carbs: Some(78.0),
                fat: Some(24.0),
                fiber: Some(6.2),
                salt: Some(3.1),
                ..Default::default()
            },
            None,
            Some("19:00"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(
            note.contains(
                "(620 kcal, 22.0g protein, 78.0g carbs, 24.0g fat, 6.2g fiber, 3.1g salt)"
            ),
            "got:\n{note}"
        );
    }

    #[test]
    fn custom_mode_without_fiber_and_salt_still_works() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        execute(
            "Random pasta",
            Some("500g"),
            NutrientArgs {
                kcal: Some(350.0),
                protein: Some(7.0),
                carbs: Some(24.0),
                fat: Some(25.0),
                ..Default::default()
            },
            None,
            Some("13:00"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(
            note.contains("(350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat)"),
            "got:\n{note}"
        );
        assert!(!note.contains("fiber"), "got:\n{note}");
    }

    #[test]
    fn fiber_alone_does_not_trigger_custom_mode() {
        // --fiber without the macro quartet must not bypass the db lookup;
        // it should fall through to lookup mode and fail on the unknown name.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        populate_db(&config);

        let err = execute(
            "ghost food",
            Some("500g"),
            NutrientArgs {
                fiber: Some(3.0),
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("No nutrition entry"), "got: {err}");
    }

    #[test]
    fn lookup_mode_applies_fiber_and_salt_overrides() {
        // The main real-world use: the db entry has no fiber/salt keys and
        // the user supplies them. Dropping the values here would report the
        // entry as unknown in the very totals line that exists to flag
        // missing data.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        populate_db(&config);

        execute(
            "kelda skogssvampsoppa",
            Some("500g"),
            NutrientArgs {
                fiber: Some(8.0),
                salt: Some(1.2),
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(note.contains("8.0g fiber"), "got:\n{note}");
        assert!(note.contains("1.2g salt"), "got:\n{note}");

        let totals = crate::food_sum::sum_food_section(&note);
        assert_eq!(totals.fiber.unknown, 0, "got:\n{note}");
        assert_eq!(totals.salt.unknown, 0, "got:\n{note}");
    }

    #[test]
    fn lookup_overrides_are_absolute_not_scaled_by_amount() {
        // --fiber is the fiber in this entry, exactly as in custom mode —
        // not a per-100g figure that the amount factor multiplies.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        populate_db(&config);

        execute(
            "kelda skogssvampsoppa",
            Some("500g"),
            NutrientArgs {
                fiber: Some(8.0),
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(note.contains("8.0g fiber"), "got:\n{note}");
        assert!(!note.contains("40.0g fiber"), "got:\n{note}");
    }

    #[test]
    fn lookup_overrides_replace_a_db_supplied_value() {
        let mut f = lookup_per_100g();
        {
            let p = f.per_100g.as_mut().unwrap();
            p.fiber = Some(2.0);
            p.salt = Some(0.5);
        }
        let mut entry = render_lookup(&f, Some(parse_amount("100g").unwrap())).unwrap();
        apply_lookup_overrides(
            &mut entry,
            &NutrientArgs {
                fiber: Some(9.0),
                salt: Some(3.0),
                ..Default::default()
            },
        );
        assert_eq!(entry.fiber, Some(9.0));
        assert_eq!(entry.salt, Some(3.0));
    }

    /// One named field of a `nutrition-db.md` panel, and how to corrupt it.
    type DbCase = (&'static str, fn(&mut FoodLookup));

    /// One named CLI flag, and how to set it to a value the screen rejects.
    type FlagCase = (&'static str, fn(&mut NutrientArgs));

    #[test]
    fn the_glycemic_db_values_are_screened_too() {
        // The flag screen reached all nine while the db screen covered six,
        // and the db side is where the argument is stronger: one bad key is
        // read on every logging of that food. Both GL columns are screened,
        // including the one this food's panel does not use — it is the file
        // that needs fixing, not this invocation.
        let cases: [DbCase; 4] = [
            ("gi", |f| f.gi = Some(-50.0)),
            ("ii", |f| f.ii = Some(f64::NAN)),
            ("gl_per_100g", |f| f.gl_per_100g = Some(-2.0)),
            ("gl_per_100ml", |f| f.gl_per_100ml = Some(f64::INFINITY)),
        ];
        for (field, mutate) in cases {
            let mut food = lookup_per_100g();
            mutate(&mut food);
            let err = render_lookup(&food, Some(parse_amount("500g").unwrap())).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains(field), "{field}: {msg}");
            assert!(msg.contains("nutrition-db.md"), "{field}: {msg}");
        }
    }

    #[test]
    fn every_rendered_entry_nutrient_reaches_the_db_screen() {
        // The flag path got a compile-time exhaustiveness pin; the db path
        // is the same shape of gap and the argument for closing it is the
        // stronger one, since a tenth `RenderedEntry` nutrient would reach
        // a durable note on *every* logging of that food rather than once.
        // Each case corrupts one panel key and expects `render_lookup` to
        // name it.
        let cases: [DbCase; 10] = [
            ("kcal", |f| f.per_100g.as_mut().unwrap().kcal = Some(-1.0)),
            ("protein", |f| {
                f.per_100g.as_mut().unwrap().protein = Some(f64::NAN)
            }),
            ("carbs", |f| {
                f.per_100g.as_mut().unwrap().carbs = Some(f64::INFINITY)
            }),
            ("fat", |f| f.per_100g.as_mut().unwrap().fat = Some(-2.0)),
            ("fiber", |f| f.per_100g.as_mut().unwrap().fiber = Some(-3.0)),
            ("salt", |f| f.per_100g.as_mut().unwrap().salt = Some(-0.5)),
            ("gi", |f| f.gi = Some(-50.0)),
            ("ii", |f| f.ii = Some(f64::NAN)),
            ("gl_per_100g", |f| f.gl_per_100g = Some(-2.0)),
            ("gl_per_100ml", |f| f.gl_per_100ml = Some(f64::INFINITY)),
        ];
        for (field, mutate) in cases {
            let mut food = lookup_per_100g();
            mutate(&mut food);
            let err = render_lookup(&food, Some(parse_amount("500g").unwrap())).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains(field), "{field}: {msg}");
            assert!(msg.contains("nutrition-db.md"), "{field}: {msg}");
        }

        // Naming every field of the struct the screen guards is what makes
        // a tenth nutrient grow the table above rather than reach the note
        // unscreened: the destructure stops compiling when a field is
        // added, the new binding is an unused-variable warning until it is
        // listed, `[Option<f64>; 8]` stops compiling when it is, and this
        // assertion then fails until the table covers it too.
        let RenderedEntry {
            display_name: _,
            amount_segment: _,
            kcal,
            protein,
            carbs,
            fat,
            fiber,
            salt,
            gi,
            ii,
            // Screened at its two source columns instead — see
            // `render_lookup` — which is why the table is two longer than
            // this list rather than one.
            gl: _,
        } = render_lookup(&lookup_per_100g(), Some(parse_amount("500g").unwrap())).unwrap();
        let screened_on_the_entry: [Option<f64>; 8] =
            [kcal, protein, carbs, fat, fiber, salt, gi, ii];
        assert_eq!(
            cases.len(),
            screened_on_the_entry.len() + 2,
            "every RenderedEntry nutrient needs a case, plus the two GL columns"
        );
    }

    #[test]
    fn every_nutrient_args_field_reaches_the_flag_screen() {
        // The screen in `execute` is nine hand-written calls, which is the
        // coupling `NutrientArgs` was introduced to remove: a tenth field
        // would reach `execute` unscreened with nothing to say so. The
        // table below asserts each field is actually screened rather than
        // merely listed; the destructure and the count assertion after it
        // are what force the table to grow.
        //
        // Binding the fields by name rather than with `_` is the load-
        // bearing part. `newfield: _` would restore the build on its own
        // and leave the table at nine — a compile-time pin that a tenth
        // field can satisfy without being screened is no pin at all.
        // Named, the chain has no such shortcut: the destructure stops
        // compiling, the new binding is an unused-variable warning (denied
        // in CI) until it is listed below, `[Option<f64>; 9]` stops
        // compiling once it is, and the assertion then fails until the
        // case table covers it.
        let NutrientArgs {
            kcal,
            protein,
            carbs,
            fat,
            fiber,
            salt,
            gi,
            gl,
            ii,
        } = NutrientArgs::default();
        let every_field: [Option<f64>; 9] = [kcal, protein, carbs, fat, fiber, salt, gi, gl, ii];

        let fields: [FlagCase; 9] = [
            ("--kcal", |n| n.kcal = Some(-1.0)),
            ("--protein", |n| n.protein = Some(-1.0)),
            ("--carbs", |n| n.carbs = Some(-1.0)),
            ("--fat", |n| n.fat = Some(-1.0)),
            ("--fiber", |n| n.fiber = Some(-1.0)),
            ("--salt", |n| n.salt = Some(-1.0)),
            ("--gi", |n| n.gi = Some(-1.0)),
            ("--gl", |n| n.gl = Some(-1.0)),
            ("--ii", |n| n.ii = Some(-1.0)),
        ];
        assert_eq!(
            fields.len(),
            every_field.len(),
            "every NutrientArgs field needs a case in `fields`"
        );

        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        for (flag, set) in fields {
            // A complete custom entry otherwise, so the screen is the only
            // thing that can reject it.
            let mut args = NutrientArgs {
                kcal: Some(350.0),
                protein: Some(7.0),
                carbs: Some(24.0),
                fat: Some(25.0),
                ..Default::default()
            };
            set(&mut args);
            let err = execute(
                "Random pasta",
                Some("500g"),
                args,
                None,
                Some("12:42"),
                &config,
                true,
            )
            .unwrap_err();
            assert!(err.to_string().contains(flag), "{flag}: {err}");
        }

        assert!(
            !dir.path()
                .join(format!("{}.md", config.effective_today()))
                .exists(),
            "a rejected entry must not have been written"
        );
    }

    #[test]
    fn a_salt_flag_does_not_rescue_a_corrupt_db_value() {
        // Documented behavior that rests entirely on statement order in
        // `execute`: `render_lookup` (which validates) runs before
        // `apply_lookup_overrides` (which would replace the bad value), so
        // a corrupt `salt:` in nutrition-db.md is reported rather than
        // papered over for one invocation — the file is what needs fixing.
        // A reorder would invert that with a green suite otherwise.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        let conn = db::open_rw(&config.db_path()).unwrap();
        db::init_db(&conn, &[]).unwrap();
        db::insert_food(
            &conn,
            &db::FoodInsert {
                name: "Trasig Soppa".into(),
                per_100g: Some(NutrientPanel {
                    kcal: Some(70.0),
                    protein: Some(1.4),
                    carbs: Some(4.8),
                    fat: Some(5.0),
                    sat_fat: None,
                    sugar: None,
                    salt: Some(-0.5),
                    fiber: None,
                }),
                per_100ml: None,
                density_g_per_ml: None,
                total: None,
                gi: None,
                gl_per_100g: None,
                gl_per_100ml: None,
                ii: None,
                description: None,
                notes: None,
                aliases: vec!["trasig soppa".into()],
                ingredients: vec![],
            },
        )
        .unwrap();
        drop(conn);

        let err = execute(
            "trasig soppa",
            Some("200g"),
            NutrientArgs {
                salt: Some(1.0),
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("salt"), "got: {msg}");
        assert!(msg.contains("nutrition-db.md"), "got: {msg}");
    }

    #[test]
    fn lookup_overrides_leave_unsupplied_nutrients_untouched() {
        let mut f = lookup_per_100g();
        f.per_100g.as_mut().unwrap().fiber = Some(2.0);
        let mut entry = render_lookup(&f, Some(parse_amount("100g").unwrap())).unwrap();
        apply_lookup_overrides(&mut entry, &NutrientArgs::default());
        assert_eq!(entry.fiber, Some(2.0));
        assert_eq!(entry.salt, None);
    }

    #[test]
    fn negative_fiber_is_rejected() {
        // A negative value writes `-3.5g fiber` and reads back as a
        // positive 3.5 marked complete — the worst available outcome for a
        // flag whose contract is a trustworthy lower bound.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        let err = execute(
            "Random pasta",
            Some("500g"),
            NutrientArgs {
                kcal: Some(350.0),
                protein: Some(7.0),
                carbs: Some(24.0),
                fat: Some(25.0),
                fiber: Some(-3.5),
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--fiber"), "got: {err}");
        assert!(
            err.to_string().contains("zero or a positive number"),
            "got: {err}"
        );
    }

    #[test]
    fn non_finite_salt_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        for bad in [f64::NAN, f64::INFINITY] {
            let err = execute(
                "Random pasta",
                Some("500g"),
                NutrientArgs {
                    kcal: Some(350.0),
                    protein: Some(7.0),
                    carbs: Some(24.0),
                    fat: Some(25.0),
                    salt: Some(bad),
                    ..Default::default()
                },
                None,
                Some("12:42"),
                &config,
                true,
            )
            .unwrap_err();
            assert!(err.to_string().contains("--salt"), "got: {err}");
            assert!(err.to_string().contains("finite"), "got: {err}");
        }
    }

    #[test]
    fn negative_fiber_is_rejected_in_lookup_mode_too() {
        // What makes the guard cover lookup mode is purely its placement
        // above the `any_macro` branch — and lookup mode is where the
        // round-1 blocker lived. Nothing else pins that placement.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        populate_db(&config);

        let err = execute(
            "kelda skogssvampsoppa",
            Some("500g"),
            NutrientArgs {
                fiber: Some(-3.5),
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--fiber"), "got: {err}");
        assert!(
            err.to_string().contains("zero or a positive number"),
            "got: {err}"
        );
    }

    #[test]
    fn negative_salt_in_the_nutrition_db_is_rejected() {
        // The db path reaches the same markdown token as `--salt`, so it
        // needs the same guard: `-1.0g salt` reads back as a *positive*
        // 1.0 marked complete.
        let mut f = lookup_per_100g();
        f.per_100g.as_mut().unwrap().salt = Some(-0.5);
        let err = render_lookup(&f, Some(parse_amount("200g").unwrap())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("salt"), "got: {msg}");
        assert!(msg.contains("nutrition-db.md"), "got: {msg}");
        assert!(msg.contains("Kelda Skogssvampsoppa"), "got: {msg}");
    }

    #[test]
    fn non_finite_fiber_in_the_nutrition_db_is_rejected() {
        let mut f = lookup_per_100g();
        f.per_100g.as_mut().unwrap().fiber = Some(f64::NAN);
        let err = render_lookup(&f, Some(parse_amount("100g").unwrap())).unwrap_err();
        assert!(err.to_string().contains("fiber"), "got: {err}");
    }

    #[test]
    fn negative_zero_is_rejected_on_both_paths() {
        // `-0.0 < 0.0` is false and `-0.0 >= 0.0` is true, so a plain sign
        // comparison waves it through and `-0.0g salt` lands in a durable
        // note. It re-parses as `+0.0`, so the number survives — but a
        // stray minus sign in stored data is what these guards exist to
        // keep out.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        let err = execute(
            "Random pasta",
            Some("500g"),
            NutrientArgs {
                kcal: Some(350.0),
                protein: Some(7.0),
                carbs: Some(24.0),
                fat: Some(25.0),
                salt: Some(-0.0),
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--salt"), "got: {err}");

        let mut f = lookup_per_100g();
        f.per_100g.as_mut().unwrap().fiber = Some(-0.0);
        let err = render_lookup(&f, Some(parse_amount("100g").unwrap())).unwrap_err();
        assert!(err.to_string().contains("fiber"), "got: {err}");
    }

    #[test]
    fn the_macro_flags_are_screened_like_fiber_and_salt() {
        // `extract_number_before` walks backwards over digits and stops at
        // the minus sign, so `--protein -7` writes `-7.0g protein` and the
        // day's total reads *+7.0* — a 14 g error from a 7 g typo, silent on
        // every surface. `--kcal nan` writes `NaNg` and re-parses as a known
        // 0. Neither is fiber- and salt-specific; the walk is the same one
        // for every token on the line.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        for (flag, kcal, protein, carbs, fat) in [
            ("--kcal", -350.0, 7.0, 24.0, 25.0),
            ("--protein", 350.0, -7.0, 24.0, 25.0),
            ("--carbs", 350.0, 7.0, -24.0, 25.0),
            ("--fat", 350.0, 7.0, 24.0, -25.0),
        ] {
            let err = execute(
                "Random pasta",
                Some("500g"),
                NutrientArgs {
                    kcal: Some(kcal),
                    protein: Some(protein),
                    carbs: Some(carbs),
                    fat: Some(fat),
                    ..Default::default()
                },
                None,
                Some("12:42"),
                &config,
                true,
            )
            .unwrap_err();
            assert!(err.to_string().contains(flag), "got: {err}");
            assert!(
                err.to_string().contains("zero or a positive number"),
                "got: {err}"
            );
        }

        for bad in [f64::NAN, f64::INFINITY] {
            let err = execute(
                "Random pasta",
                Some("500g"),
                NutrientArgs {
                    kcal: Some(350.0),
                    protein: Some(bad),
                    carbs: Some(24.0),
                    fat: Some(25.0),
                    ..Default::default()
                },
                None,
                Some("12:42"),
                &config,
                true,
            )
            .unwrap_err();
            assert!(err.to_string().contains("--protein"), "got: {err}");
            assert!(err.to_string().contains("finite"), "got: {err}");
        }

        assert!(
            !dir.path()
                .join(format!("{}.md", config.effective_today()))
                .exists(),
            "a rejected entry must not have been written"
        );
    }

    #[test]
    fn the_glycemic_flags_are_screened_too() {
        // Nothing parses `GI ~-50` back today, so this is only about what
        // reaches the note — which is reason enough: the screen is supposed
        // to cover every numeric flag, and a reader added later would
        // inherit the same sign flip the six others are guarded against.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        for (flag, gi, gl, ii) in [
            ("--gi", Some(-50.0), None, None),
            ("--gl", None, Some(-10.0), None),
            ("--ii", None, None, Some(f64::NAN)),
        ] {
            let err = execute(
                "Random pasta",
                Some("500g"),
                NutrientArgs {
                    kcal: Some(350.0),
                    protein: Some(7.0),
                    carbs: Some(24.0),
                    fat: Some(25.0),
                    gi,
                    gl,
                    ii,
                    ..Default::default()
                },
                None,
                Some("12:42"),
                &config,
                true,
            )
            .unwrap_err();
            assert!(err.to_string().contains(flag), "got: {err}");
        }

        assert!(
            !dir.path()
                .join(format!("{}.md", config.effective_today()))
                .exists(),
            "a rejected entry must not have been written"
        );
    }

    #[test]
    fn a_negative_macro_in_the_nutrition_db_is_rejected() {
        // Same failure, reached through the db instead of a flag — and worse
        // there, because one bad key is re-read on every logging of that
        // food rather than mistyped once.
        for field in ["kcal", "protein", "carbs", "fat"] {
            let mut f = lookup_per_100g();
            {
                let p = f.per_100g.as_mut().unwrap();
                match field {
                    "kcal" => p.kcal = Some(-70.0),
                    "protein" => p.protein = Some(-1.4),
                    "carbs" => p.carbs = Some(-4.8),
                    _ => p.fat = Some(-5.0),
                }
            }
            let err = render_lookup(&f, Some(parse_amount("200g").unwrap())).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains(field), "got: {msg}");
            assert!(msg.contains("nutrition-db.md"), "got: {msg}");
        }

        let mut f = lookup_per_100g();
        f.per_100g.as_mut().unwrap().kcal = Some(f64::INFINITY);
        let err = render_lookup(&f, Some(parse_amount("100g").unwrap())).unwrap_err();
        assert!(err.to_string().contains("kcal"), "got: {err}");
    }

    #[test]
    fn a_multi_line_food_name_is_rejected() {
        // `append_line_to_section` inserts the name verbatim, so a newline
        // writes a second physical line the parser reads as its own entry.
        // That forged line ends before the real nutrient group, so its
        // fiber and salt parse as a *measured* zero — the one state the
        // unknown counting exists to keep out — while the real entry drops
        // out of every counter.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        let err = execute(
            "Pasta\n- **12:43** Forged (0 kcal, 0.0g protein, 0.0g salt)",
            None,
            NutrientArgs {
                kcal: Some(350.0),
                protein: Some(7.0),
                carbs: Some(24.0),
                fat: Some(25.0),
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("single line"), "got: {err}");
    }

    #[test]
    fn zero_salt_in_the_nutrition_db_is_accepted() {
        let mut f = lookup_per_100g();
        f.per_100g.as_mut().unwrap().salt = Some(0.0);
        let entry = render_lookup(&f, Some(parse_amount("100g").unwrap())).unwrap();
        assert_eq!(entry.salt, Some(0.0));
    }

    #[test]
    fn zero_salt_is_accepted_as_a_measurement() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        execute(
            "Water",
            None,
            NutrientArgs {
                kcal: Some(0.0),
                protein: Some(0.0),
                carbs: Some(0.0),
                fat: Some(0.0),
                salt: Some(0.0),
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(note.contains("0.0g salt"), "got:\n{note}");
    }

    #[test]
    fn execute_custom_mode_partial_macros_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        let err = execute(
            "x",
            Some("500g"),
            NutrientArgs {
                kcal: Some(350.0),
                ..Default::default()
            },
            None,
            Some("13:00"),
            &config,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Custom mode requires"));
    }

    #[test]
    fn execute_lookup_no_db_errors_with_suggestion() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        let err = execute(
            "anything",
            Some("500g"),
            NutrientArgs::default(),
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Database not found"), "got: {msg}");
    }

    #[test]
    fn execute_lookup_unknown_name_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        populate_db(&config);

        let err = execute(
            "ghost food",
            Some("500g"),
            NutrientArgs::default(),
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("No nutrition entry"));
    }

    #[test]
    fn execute_date_flag_writes_to_named_day() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        execute(
            "Custom item",
            Some("500g"),
            NutrientArgs {
                kcal: Some(350.0),
                protein: Some(7.0),
                carbs: Some(24.0),
                fat: Some(25.0),
                ..Default::default()
            },
            Some("2026-04-29"),
            Some("22:00"),
            &config,
            true,
        )
        .unwrap();

        let path = dir.path().join("2026-04-29.md");
        let note = std::fs::read_to_string(&path).unwrap();
        assert!(note.contains("- **22:00** Custom item"));
    }

    #[test]
    fn execute_lookup_with_gi_override_replaces_gi() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        populate_db(&config);

        execute(
            "kelda skogssvampsoppa",
            Some("500g"),
            NutrientArgs {
                gi: Some(45.0), // --gi override (DB has 40)
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(note.contains("GI ~45"), "expected --gi override:\n{note}");
        assert!(!note.contains("GI ~40"), "DB gi should not appear:\n{note}");
    }

    #[test]
    fn execute_lookup_with_gi_override_recomputes_gl_when_no_gl_flag() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        populate_db(&config);

        // 500g of kelda has carbs = 24g. With --gi 50 and no --gl,
        // GL should auto-compute to 50 * 24 / 100 = 12.0.
        execute(
            "kelda skogssvampsoppa",
            Some("500g"),
            NutrientArgs {
                gi: Some(50.0), // --gi; no --gl
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(
            note.contains("GL ~12.0"),
            "expected auto-compute from new gi:\n{note}"
        );
    }

    #[test]
    fn execute_lookup_with_gl_override_replaces_gl() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        populate_db(&config);

        execute(
            "kelda skogssvampsoppa",
            Some("500g"),
            NutrientArgs {
                gl: Some(99.9), // --gl override
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(note.contains("GL ~99.9"), "expected --gl override:\n{note}");
    }

    #[test]
    fn execute_lookup_with_ii_override_replaces_ii() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());
        populate_db(&config);

        execute(
            "kelda skogssvampsoppa",
            Some("500g"),
            NutrientArgs {
                ii: Some(99.0), // --ii override
                ..Default::default()
            },
            None,
            Some("12:42"),
            &config,
            true,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(note.contains("II ~99"), "expected --ii override:\n{note}");
        assert!(!note.contains("II ~35"), "DB ii should not appear:\n{note}");
    }

    #[test]
    fn format_food_totals_complete_coverage() {
        let t = FoodTotals {
            kcal: 1340.4,
            protein: 95.6,
            carbs: 50.0,
            fat: 60.2,
            fiber: NutrientTotal {
                sum: 12.4,
                unknown: 0,
            },
            salt: NutrientTotal {
                sum: 5.6,
                unknown: 0,
            },
            entry_count: 3,
            skipped_lines: 0,
            ..Default::default()
        };
        // Macros keep integer rounding: 1340, 96, 50, 60.
        assert_eq!(
            format_food_totals(&t),
            "1340 kcal, 96g protein, 50g carbs, 60g fat, 12.4g fiber, 5.6g salt"
        );
    }

    #[test]
    fn format_food_totals_marks_partial_coverage_with_plus() {
        let t = FoodTotals {
            kcal: 1077.0,
            protein: 88.0,
            carbs: 38.0,
            fat: 62.0,
            fiber: NutrientTotal {
                sum: 8.4,
                unknown: 9,
            },
            salt: NutrientTotal {
                sum: 5.6,
                unknown: 2,
            },
            entry_count: 12,
            skipped_lines: 0,
            ..Default::default()
        };
        assert_eq!(
            format_food_totals(&t),
            "1077 kcal, 88g protein, 38g carbs, 62g fat, 8.4g+ fiber (9 unknown), 5.6g+ salt (2 unknown)"
        );
    }

    #[test]
    fn format_food_totals_no_coverage_says_unknown() {
        let t = FoodTotals {
            kcal: 700.0,
            protein: 30.0,
            carbs: 100.0,
            fat: 25.0,
            fiber: NutrientTotal {
                sum: 0.0,
                unknown: 3,
            },
            salt: NutrientTotal {
                sum: 0.0,
                unknown: 3,
            },
            entry_count: 3,
            skipped_lines: 0,
            ..Default::default()
        };
        let out = format_food_totals(&t);
        assert!(out.contains("fiber unknown (3 entries)"), "got: {out}");
        assert!(out.contains("salt unknown (3 entries)"), "got: {out}");
        assert!(!out.contains("0.0g fiber"), "got: {out}");
    }

    #[test]
    fn format_food_totals_single_unknown_entry_is_singular() {
        let t = FoodTotals {
            kcal: 200.0,
            protein: 10.0,
            carbs: 20.0,
            fat: 5.0,
            fiber: NutrientTotal {
                sum: 0.0,
                unknown: 1,
            },
            salt: NutrientTotal {
                sum: 0.0,
                unknown: 1,
            },
            entry_count: 1,
            skipped_lines: 0,
            ..Default::default()
        };
        let out = format_food_totals(&t);
        assert!(out.contains("fiber unknown (1 entry)"), "got: {out}");
    }

    #[test]
    fn format_food_totals_empty_day_reports_zero_not_unknown() {
        let out = format_food_totals(&FoodTotals::default());
        assert!(out.contains("0.0g fiber"), "got: {out}");
        assert!(out.contains("0.0g salt"), "got: {out}");
        assert!(!out.contains("couldn't be parsed"), "got: {out}");
    }

    #[test]
    fn format_food_totals_reports_skipped_lines() {
        // Every number on this line is missing that entry's contribution.
        // `vitalog today` says so; the line printed right after logging
        // must not be the one surface where a dropped entry leaves no trace.
        let t = FoodTotals {
            kcal: 200.0,
            skipped_lines: 1,
            entry_count: 1,
            ..Default::default()
        };
        let out = format_food_totals(&t);
        assert!(
            out.contains("— 1 food line couldn't be parsed"),
            "got: {out}"
        );

        let t = FoodTotals {
            skipped_lines: 2,
            ..t
        };
        let out = format_food_totals(&t);
        assert!(
            out.contains("— 2 food lines couldn't be parsed"),
            "got: {out}"
        );

        // And it names the lines it counts. This is the surface the user is
        // looking at the instant a line is dropped, so it is the one where
        // knowing *which* line costs the least to act on.
        let t = FoodTotals {
            skipped_times: vec!["12:00".into(), "19:30".into()],
            ..t
        };
        let out = format_food_totals(&t);
        assert!(
            out.contains("— 2 food lines couldn't be parsed (12:00, 19:30)"),
            "got: {out}"
        );
    }

    #[test]
    fn skipped_line_note_is_not_a_second_bare_parenthetical() {
        // `… 2.0g+ salt (1 unknown) (1 food line couldn't be parsed)` reads
        // as though the second parenthetical also scoped to salt. It is
        // line-scoped, so it is set off with a dash instead.
        let t = FoodTotals {
            kcal: 200.0,
            salt: crate::food_sum::NutrientTotal {
                sum: 2.0,
                unknown: 1,
            },
            entry_count: 2,
            skipped_lines: 1,
            ..Default::default()
        };
        let out = format_food_totals(&t);
        assert!(out.contains("(1 unknown)"), "got: {out}");
        assert!(
            !out.contains(") (1 food line"),
            "two adjacent bare parentheticals: {out}"
        );
        assert!(
            out.contains("(1 unknown) — 1 food line couldn't be parsed"),
            "got: {out}"
        );
    }

    #[test]
    fn a_skipped_line_marks_the_running_total_a_lower_bound() {
        // Full per-entry coverage, one unparsed line: the nutrient the
        // dropped line carried is missing from the sum all the same, so
        // `4.0g salt` would claim an exactness the day does not have.
        // `today`'s dashboard row says `+` here, and this line must agree.
        let t = FoodTotals {
            kcal: 350.0,
            salt: crate::food_sum::NutrientTotal {
                sum: 4.0,
                unknown: 0,
            },
            entry_count: 2,
            skipped_lines: 1,
            ..Default::default()
        };
        let out = format_food_totals(&t);
        assert!(out.contains("4.0g+ salt"), "got: {out}");
        assert!(
            !out.contains("4.0g salt"),
            "still claims an exact total: {out}"
        );
        // No per-entry count exists for a skipped line, so none is invented.
        assert!(!out.contains("0 unknown"), "got: {out}");
    }

    #[test]
    fn execute_verbose_mode_writes_file_unchanged() {
        // quiet=false (verbose) hits the totals-summing path; ensure the
        // file content is identical to quiet=true so output formatting
        // doesn't accidentally affect persistence.
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_in(dir.path());

        execute(
            "Random pasta",
            Some("500g"),
            NutrientArgs {
                kcal: Some(350.0),
                protein: Some(7.0),
                carbs: Some(24.0),
                fat: Some(25.0),
                ..Default::default()
            },
            None,
            Some("13:00"),
            &config,
            false,
        )
        .unwrap();

        let note = read_today(dir.path(), &config);
        assert!(
            note.contains(
                "- **13:00** Random pasta (500g) (350 kcal, 7.0g protein, 24.0g carbs, 25.0g fat)"
            ),
            "got:\n{note}"
        );
    }
}
