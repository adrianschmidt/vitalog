pub mod bp_cmd;
pub mod completions;
pub mod food_cmd;
pub mod log_cmd;
pub mod migrate_cmd;
pub mod note_cmd;
pub mod readme_cmd;
pub mod sleep_cmd;
pub mod status_cmd;
pub mod today_cmd;
pub mod trend_cmd;

use clap::{Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser)]
#[command(
    name = "vitalog",
    version,
    about = "A terminal dashboard that tracks your life from markdown notes"
)]
pub struct Cli {
    /// Suppress the full-line + totals confirmation from `food`/`note`/`bp`/`log`;
    /// emit just the existing one-line `<thing> logged: ...` summary.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Set up vitalog: create config, generate demo data
    Init {
        /// Notes directory path (skip interactive prompt)
        #[arg(long)]
        notes_dir: Option<String>,
        /// Skip demo data generation
        #[arg(long)]
        no_demo: bool,
    },
    /// Migrate legacy daylog paths (config dir, database) to vitalog locations.
    /// Idempotent: safe to run multiple times.
    Migrate,
    /// Log a value to today's note
    Log {
        /// Field name (weight, sleep, mood, energy, lift, climb, metric)
        field: String,
        /// Value (all args joined — no shell quoting needed)
        #[arg(trailing_var_arg = true)]
        value: Vec<String>,
    },
    /// Print today's data as JSON
    Status,
    /// Sync notes to database (one-shot, no TUI)
    Sync,
    /// Open today's note (or a specific date) in $EDITOR
    Edit {
        /// Date in YYYY-MM-DD format (defaults to today)
        date: Option<String>,
    },
    /// Delete and rebuild the database from all notes
    Rebuild,
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
    /// Print the embedded README to stdout (compiled into the binary)
    Readme,
    /// Record bedtime (uses now, or pass a time)
    ///
    /// Stores the pending bedtime in `.vitalog-state.toml` next to the DB.
    /// Run `vitalog sleep-end` after waking to finalize the entry.
    ///
    /// Re-running before `sleep-end` replaces the previous pending bedtime
    /// (with a stderr notice). A pending bedtime older than 24h is treated
    /// as stale and discarded by `sleep-end`.
    SleepStart {
        /// Bedtime in HH:MM (24h) or H:MMam/pm (12h)
        time: Option<String>,
    },
    /// Finalize sleep entry on today's note (uses now, or pass a wake time)
    ///
    /// Reads the pending bedtime from `vitalog sleep-start` and writes
    /// `sleep: "bedtime-waketime"` to today's note. The wake date is
    /// always calendar today (the date on the wall clock), independent of
    /// `day_start_hour` — bedtimes past midnight land on the wake-day's
    /// note, which is the convention this command exists to enforce.
    ///
    /// The written value is rendered per `time_format` from your config
    /// (`12h` or `24h`); the database always stores canonical 24h.
    SleepEnd {
        /// Wake time in HH:MM (24h) or H:MMam/pm (12h)
        time: Option<String>,
    },
    /// Log a food entry to the day's `## Food` section
    Food {
        /// Name (literal or nutrition-db alias)
        name: String,
        /// Amount with optional unit (e.g., 500g, 250ml). Required for
        /// per_100g/per_100ml entries; optional for total-panel entries.
        amount: Option<String>,
        /// Every nutrient flag, defined once in `food_cmd` and consumed
        /// there — see `NutrientArgs`.
        #[command(flatten)]
        nutrients: crate::cli::food_cmd::NutrientArgs,
        /// Override target date (YYYY-MM-DD). Default: effective_today.
        #[arg(long)]
        date: Option<String>,
        /// Override entry time (HH:MM 24h or H:MMam/pm 12h). Default: now.
        #[arg(long)]
        time: Option<String>,
    },
    /// Log a free-text note to the day's `## Notes` section
    Note {
        /// Override target date (YYYY-MM-DD). Default: effective_today.
        #[arg(long)]
        date: Option<String>,
        /// Override entry time (HH:MM 24h or H:MMam/pm 12h). Default: now.
        #[arg(long)]
        time: Option<String>,
        /// Note text or [notes.aliases] key (joined; no shell quoting needed)
        #[arg(trailing_var_arg = true)]
        text: Vec<String>,
    },
    /// Log a blood pressure reading (YAML + `## Vitals` line)
    Bp {
        /// Systolic pressure (mmHg)
        sys: i32,
        /// Diastolic pressure (mmHg)
        dia: i32,
        /// Pulse (bpm)
        pulse: i32,
        /// Force the morning slot (otherwise auto-pick by time vs. the 14:00 cutoff)
        #[arg(long, conflicts_with = "evening")]
        morning: bool,
        /// Force the evening slot
        #[arg(long)]
        evening: bool,
        /// Override target date (YYYY-MM-DD). Default: effective_today.
        #[arg(long)]
        date: Option<String>,
        /// Override entry time (HH:MM 24h or H:MMam/pm 12h). Default: now.
        #[arg(long)]
        time: Option<String>,
    },
    /// Print a compact daily summary (food totals, weight, sleep, BP morning,
    /// custom metrics) with optional goal comparison from goals.md.
    Today {
        /// Date in YYYY-MM-DD format (defaults to effective today)
        date: Option<String>,
        /// Print JSON instead of formatted text
        #[arg(long)]
        json: bool,
    },
    /// Print a chart of recent values for any tracked field.
    ///
    /// Built-in fields: weight, sleep_hours, mood, energy.
    /// Custom fields: anything in [metrics] in your config.
    Trend {
        /// Field name to chart.
        field: String,
        /// Window length in days (default 14).
        #[arg(default_value_t = 14)]
        days: u32,
        /// One-line sparkline instead of multi-row chart.
        #[arg(long, conflicts_with = "json")]
        compact: bool,
        /// Print structured JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Helpers shared by food/note/bp for resolving --date and --time flags
/// and rendering the timestamp prefix per `config.time_format`.
pub mod resolve {
    use chrono::{Local, NaiveDate, NaiveTime};
    use color_eyre::eyre::Result;
    use color_eyre::Help;

    use crate::config::Config;
    use crate::time;

    /// Resolve the target date for a logging command. `--date` overrides;
    /// otherwise `config.effective_today_date()`.
    pub fn target_date(flag: Option<&str>, config: &Config) -> Result<NaiveDate> {
        match flag {
            Some(s) => NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
                .map_err(|_| color_eyre::eyre::eyre!("Invalid --date: '{s}'. Expected YYYY-MM-DD."))
                .suggestion("Use a date in YYYY-MM-DD form, e.g., 2026-04-30."),
            None => Ok(config.effective_today_date()),
        }
    }

    /// Resolve the timestamp for the `**HH:MM**` prefix and BP slot
    /// detection. `--time` overrides; otherwise `Local::now().time()`.
    pub fn target_time(flag: Option<&str>) -> Result<NaiveTime> {
        match flag {
            Some(s) => time::parse_time(s)
                .ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "Invalid --time: '{s}'. Expected HH:MM (24h) or H:MMam/pm (12h)."
                    )
                })
                .suggestion("Examples: 22:30, 07:05, 10:30pm, 6:15am."),
            None => Ok(Local::now().time()),
        }
    }
}
