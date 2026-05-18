use chrono::{Local, NaiveDate, Timelike};
use color_eyre::eyre::{Result, WrapErr};
use color_eyre::Section;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static LEGACY_HINT_PRINTED: OnceLock<()> = OnceLock::new();

// Manual smoke: invoke a command twice in one process and confirm the
// hint message appears at most once on stderr.
fn print_legacy_hint_once(legacy: &Path, current: &Path) {
    if LEGACY_HINT_PRINTED.set(()).is_ok() {
        eprintln!(
            "Note: Found legacy daylog data at {}.\n\
             Run `vitalog migrate` to move it to {}.",
            legacy.display(),
            current.display(),
        );
    }
}

/// Path-injectable variant of `Config::config_path()`. Prefers
/// `<parent>/vitalog/config.toml`; falls back to
/// `<parent>/daylog/config.toml` when the new path does not exist but
/// the legacy one does. Pure — no I/O side effects, no logging.
pub(crate) fn resolve_config_path(parent: &Path) -> PathBuf {
    let current = parent.join("vitalog").join("config.toml");
    if current.exists() {
        return current;
    }
    let legacy = parent.join("daylog").join("config.toml");
    if legacy.exists() {
        return legacy;
    }
    current // best default for "doesn't exist anywhere yet" — vitalog wins
}

/// Env-aware variant of [`resolve_config_path`]. When `env_override` is
/// `Some(non-empty)`, returns that path verbatim (tilde-expanded) without
/// consulting `parent` — this is what enables a sandbox config via
/// `$VITALOG_CONFIG`. Empty strings and `None` fall back to the
/// parent-based resolver. Pure — no I/O side effects, no logging.
pub(crate) fn resolve_config_path_with_env(env_override: Option<&str>, parent: &Path) -> PathBuf {
    if let Some(p) = env_override {
        if !p.is_empty() {
            return expand_tilde(p);
        }
    }
    resolve_config_path(parent)
}

/// Reads `$VITALOG_CONFIG`, treating an empty value as unset. Centralizes
/// the policy so `config_path`, `config_dir`, and `load` agree on whether
/// the user opted into a sandbox.
fn vitalog_config_env() -> Option<String> {
    std::env::var("VITALOG_CONFIG")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Formats the "config not found" error. When `from_env` is true the user
/// set `$VITALOG_CONFIG` themselves, so `vitalog init` would land at the
/// default path (not the override) — point at the env var instead.
fn config_not_found_message(path: &Path, from_env: bool) -> String {
    if from_env {
        format!(
            "Config not found at {} (set via $VITALOG_CONFIG). \
             Create the file or unset the variable.",
            path.display()
        )
    } else {
        format!(
            "Config not found at {}. Run `vitalog init` to create one.",
            path.display()
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeightUnit {
    #[default]
    Lbs,
    Kg,
}

impl fmt::Display for WeightUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WeightUnit::Lbs => write!(f, "lbs"),
            WeightUnit::Kg => write!(f, "kg"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum TimeFormat {
    #[default]
    #[serde(rename = "12h")]
    TwelveHour,
    #[serde(rename = "24h")]
    TwentyFourHour,
}

impl fmt::Display for TimeFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimeFormat::TwelveHour => write!(f, "12h"),
            TimeFormat::TwentyFourHour => write!(f, "24h"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub notes_dir: String,
    #[serde(default = "default_db_path")]
    pub db_path: Option<String>,
    #[serde(default = "default_refresh_secs")]
    pub refresh_secs: u64,
    #[serde(default)]
    pub day_start_hour: u8,
    /// Unit for weight display. The database stores raw numbers without unit
    /// information, so changing this mid-use makes historical values ambiguous
    /// (no automatic conversion is performed).
    #[serde(default)]
    pub weight_unit: WeightUnit,
    #[serde(default)]
    pub time_format: TimeFormat,
    #[serde(default)]
    pub modules: ModulesConfig,
    #[serde(default)]
    pub exercises: HashMap<String, ExerciseConfig>,
    #[serde(default)]
    pub metrics: HashMap<String, MetricConfig>,
    #[serde(default)]
    pub reminders: HashMap<String, ReminderConfig>,
    #[serde(default)]
    pub notes: NotesConfig,
    #[serde(default = "default_toml_table")]
    pub climbing: toml::Value,
}

fn default_db_path() -> Option<String> {
    None
}

fn default_toml_table() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

fn default_refresh_secs() -> u64 {
    15
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModulesConfig {
    #[serde(default = "default_true")]
    pub dashboard: bool,
    #[serde(default = "default_true")]
    pub training: bool,
    #[serde(default = "default_true")]
    pub trends: bool,
    #[serde(default)]
    pub climbing: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NotesConfig {
    #[serde(default)]
    pub aliases: HashMap<String, String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExerciseConfig {
    pub display: String,
    #[serde(default = "default_color")]
    pub color: String,
}

fn default_color() -> String {
    "white".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetricConfig {
    pub display: String,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReminderConfig {
    pub display: String,
    pub interval_days: u32,
    pub watch: String,
    pub target: toml::Value,
    #[serde(default)]
    pub count_zero_as_logged: bool,
    #[serde(default)]
    pub not_before: Option<String>,
    #[serde(default)]
    pub not_after: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            let from_env = vitalog_config_env().is_some();
            color_eyre::eyre::bail!("{}", config_not_found_message(&path, from_env));
        }
        let contents = std::fs::read_to_string(&path)
            .wrap_err_with(|| format!("Failed to read config at {}", path.display()))?;
        let config: Config = toml::from_str(&contents).map_err(|e| {
            let err = color_eyre::eyre::eyre!("Failed to parse config at {}: {e}", path.display());
            if e.message().contains("weight_unit") {
                err.suggestion("weight_unit must be \"kg\" or \"lbs\" (default: \"lbs\").")
            } else if e.message().contains("time_format") {
                err.suggestion("time_format must be \"12h\" or \"24h\" (default: \"12h\").")
            } else {
                err
            }
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn load_or_keep(current: &Config) -> Config {
        match Self::load() {
            Ok(new_config) => new_config,
            Err(e) => {
                eprintln!("Warning: config reload failed: {e}. Keeping current config.");
                current.clone()
            }
        }
    }

    fn validate(&self) -> Result<()> {
        let notes = self.notes_dir_path();
        if !notes.exists() {
            color_eyre::eyre::bail!(
                "Notes directory does not exist: {}. Check notes_dir in your config or run `vitalog init`.",
                notes.display()
            );
        }
        if !notes.is_dir() {
            color_eyre::eyre::bail!(
                "notes_dir points to a file, not a directory: {}. Check your config.",
                notes.display()
            );
        }
        if self.day_start_hour > 23 {
            return Err(color_eyre::eyre::eyre!(
                "day_start_hour must be between 0 and 23, got {}.",
                self.day_start_hour
            ))
            .suggestion("Set day_start_hour to a value between 0 and 23 in your config.toml.");
        }
        // Surface [reminders] structural errors fail-fast on every command,
        // not just `today` and `status`. load_reminders runs the same
        // validation that today/status would otherwise do at runtime; we
        // discard the Vec — the result is recomputed when those commands
        // actually need the parsed reminders.
        crate::reminders::load_reminders(self)?;
        Ok(())
    }

    /// Returns today's effective date as a `NaiveDate`, shifted by `day_start_hour`.
    ///
    /// If the current time is before `day_start_hour`, the effective date
    /// is yesterday. For example, with `day_start_hour = 4`, 00:30 on
    /// April 10 counts as April 9.
    pub fn effective_today_date(&self) -> NaiveDate {
        effective_date_naive(Local::now(), self.day_start_hour)
    }

    /// Returns today's effective date as a formatted `YYYY-MM-DD` string.
    pub fn effective_today(&self) -> String {
        self.effective_today_date().format("%Y-%m-%d").to_string()
    }

    /// Directory containing the config file. Used by `vitalog init` as the
    /// `mkdir -p` target before writing `config_path()`. Derived from
    /// `config_path()` so the env override is honored uniformly: if a user
    /// sets `$VITALOG_CONFIG=/tmp/sandbox/config.toml`, init creates
    /// `/tmp/sandbox/`, not `~/Library/Application Support/vitalog/`.
    pub fn config_dir() -> Result<PathBuf> {
        let path = Self::config_path()?;
        path.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| color_eyre::eyre::eyre!("Could not determine config directory"))
    }

    pub fn config_path() -> Result<PathBuf> {
        let parent = dirs::config_dir()
            .ok_or_else(|| color_eyre::eyre::eyre!("Could not determine config directory"))?;
        let env = vitalog_config_env();
        let resolved = resolve_config_path_with_env(env.as_deref(), &parent);
        // Legacy daylog fallback only matters when we're using the
        // default parent-based resolution. With an env override the user
        // has explicitly opted into a sandbox path; there's no legacy to
        // migrate from.
        if env.is_none() {
            let legacy_dir = parent.join("daylog");
            if resolved.starts_with(&legacy_dir) {
                print_legacy_hint_once(&legacy_dir, &parent.join("vitalog"));
            }
        }
        Ok(resolved)
    }

    pub fn notes_dir_path(&self) -> PathBuf {
        expand_tilde(&self.notes_dir)
    }

    pub fn db_path(&self) -> PathBuf {
        if let Some(p) = &self.db_path {
            return expand_tilde(p);
        }
        let notes = self.notes_dir_path();
        let current = notes.join(".vitalog.db");
        if current.is_file() {
            return current;
        }
        let legacy = notes.join(".daylog.db");
        if legacy.is_file() {
            print_legacy_hint_once(&legacy, &current);
            return legacy;
        }
        current
    }

    pub fn module_config(&self, id: &str) -> Option<&toml::Value> {
        if id == "climbing" {
            if self.climbing.is_table() {
                Some(&self.climbing)
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        match id {
            "dashboard" => self.modules.dashboard,
            "training" => self.modules.training,
            "trends" => self.modules.trends,
            "climbing" => self.modules.climbing,
            _ => false,
        }
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

pub fn default_config_contents() -> &'static str {
    include_str!("../presets/default.toml")
}

#[cfg(test)]
fn effective_date<Tz: chrono::TimeZone>(now: chrono::DateTime<Tz>, day_start_hour: u8) -> String
where
    Tz::Offset: std::fmt::Display,
{
    effective_date_naive(now, day_start_hour)
        .format("%Y-%m-%d")
        .to_string()
}

/// Compute the effective `NaiveDate` for a given datetime and day-start hour.
pub(crate) fn effective_date_naive<Tz: chrono::TimeZone>(
    now: chrono::DateTime<Tz>,
    day_start_hour: u8,
) -> NaiveDate {
    debug_assert!(day_start_hour <= 23, "day_start_hour must be 0..=23");
    let date = now.date_naive();
    if (now.hour() as u8) < day_start_hour {
        date.checked_sub_days(chrono::Days::new(1))
            .expect("date subtraction should not underflow")
    } else {
        date
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde("~/notes");
        assert!(expanded.to_str().unwrap().contains("notes"));
        assert!(!expanded.to_str().unwrap().starts_with("~"));
    }

    #[test]
    fn test_parse_default_config() {
        let config: Config = toml::from_str(default_config_contents()).unwrap();
        assert!(config.modules.dashboard);
        assert!(config.modules.training);
        assert!(config.modules.trends);
        assert!(!config.modules.climbing);
        assert_eq!(config.day_start_hour, 0);
    }

    #[test]
    fn test_parse_day_start_hour() {
        let config: Config =
            toml::from_str("notes_dir = '/tmp/test'\nday_start_hour = 4\n[modules]\n").unwrap();
        assert_eq!(config.day_start_hour, 4);
    }

    #[test]
    fn test_day_start_hour_defaults_to_zero() {
        let config: Config = toml::from_str("notes_dir = '/tmp/test'\n[modules]\n").unwrap();
        assert_eq!(config.day_start_hour, 0);
    }

    #[test]
    fn test_day_start_hour_over_23_rejected() {
        let config: Config =
            toml::from_str("notes_dir = '/tmp'\nday_start_hour = 24\n[modules]\n").unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let config = Config {
            notes_dir: dir.path().to_str().unwrap().to_string(),
            ..config
        };
        let err = config.validate().unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("day_start_hour"),
            "error should mention day_start_hour: {msg}"
        );
    }

    // -- effective_date tests --

    use chrono::TimeZone;

    fn local(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        min: u32,
    ) -> chrono::DateTime<chrono::FixedOffset> {
        chrono::FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(year, month, day, hour, min, 0)
            .unwrap()
    }

    #[test]
    fn test_effective_date_midnight_boundary_default() {
        // With day_start_hour=0, 00:30 on Apr 10 → Apr 10
        let dt = local(2026, 4, 10, 0, 30);
        assert_eq!(effective_date(dt, 0), "2026-04-10");
    }

    #[test]
    fn test_effective_date_before_boundary() {
        // With day_start_hour=4, 00:30 on Apr 10 → Apr 9 (still "yesterday")
        let dt = local(2026, 4, 10, 0, 30);
        assert_eq!(effective_date(dt, 4), "2026-04-09");
    }

    #[test]
    fn test_effective_date_at_boundary() {
        // With day_start_hour=4, 04:00 on Apr 10 → Apr 10 (new day starts)
        let dt = local(2026, 4, 10, 4, 0);
        assert_eq!(effective_date(dt, 4), "2026-04-10");
    }

    #[test]
    fn test_effective_date_after_boundary() {
        // With day_start_hour=4, 23:00 on Apr 9 → Apr 9 (normal)
        let dt = local(2026, 4, 9, 23, 0);
        assert_eq!(effective_date(dt, 4), "2026-04-09");
    }

    #[test]
    fn test_effective_date_just_before_boundary() {
        // With day_start_hour=4, 03:59 on Apr 10 → Apr 9
        let dt = local(2026, 4, 10, 3, 59);
        assert_eq!(effective_date(dt, 4), "2026-04-09");
    }

    #[test]
    fn test_effective_date_day_start_hour_23() {
        // With day_start_hour=23, only 23:00-23:59 is "today".
        // Everything from 0:00-22:59 rolls back to the previous day.
        let before = local(2026, 4, 10, 22, 59);
        assert_eq!(effective_date(before, 23), "2026-04-09");

        let at = local(2026, 4, 10, 23, 0);
        assert_eq!(effective_date(at, 23), "2026-04-10");

        let midnight = local(2026, 4, 10, 0, 0);
        assert_eq!(effective_date(midnight, 23), "2026-04-09");

        let midday = local(2026, 4, 10, 12, 0);
        assert_eq!(effective_date(midday, 23), "2026-04-09");
    }

    #[test]
    fn test_effective_date_jan_1_rollback() {
        // With day_start_hour=5, 02:00 on Jan 1 → Dec 31 of previous year
        let dt = local(2026, 1, 1, 2, 0);
        assert_eq!(effective_date(dt, 5), "2025-12-31");
    }

    #[test]
    fn test_weight_unit_defaults_to_lbs() {
        let config: Config = toml::from_str("notes_dir = '/tmp/test'\n").unwrap();
        assert_eq!(config.weight_unit, WeightUnit::Lbs);
    }

    #[test]
    fn test_weight_unit_kg() {
        let config: Config =
            toml::from_str("notes_dir = '/tmp/test'\nweight_unit = 'kg'\n").unwrap();
        assert_eq!(config.weight_unit, WeightUnit::Kg);
    }

    #[test]
    fn test_weight_unit_lbs_explicit() {
        let config: Config =
            toml::from_str("notes_dir = '/tmp/test'\nweight_unit = 'lbs'\n").unwrap();
        assert_eq!(config.weight_unit, WeightUnit::Lbs);
    }

    #[test]
    fn test_weight_unit_invalid() {
        let result: std::result::Result<Config, _> =
            toml::from_str("notes_dir = '/tmp/test'\nweight_unit = 'stones'\n");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().message().to_string();
        assert!(
            err_msg.contains("unknown variant"),
            "error should mention unknown variant: {err_msg}"
        );
    }

    #[test]
    fn test_weight_unit_display() {
        assert_eq!(WeightUnit::Lbs.to_string(), "lbs");
        assert_eq!(WeightUnit::Kg.to_string(), "kg");
    }

    #[test]
    fn test_time_format_defaults_to_12h() {
        let config: Config = toml::from_str("notes_dir = '/tmp/test'\n").unwrap();
        assert_eq!(config.time_format, TimeFormat::TwelveHour);
    }

    #[test]
    fn test_time_format_24h() {
        let config: Config =
            toml::from_str("notes_dir = '/tmp/test'\ntime_format = '24h'\n").unwrap();
        assert_eq!(config.time_format, TimeFormat::TwentyFourHour);
    }

    #[test]
    fn test_time_format_12h_explicit() {
        let config: Config =
            toml::from_str("notes_dir = '/tmp/test'\ntime_format = '12h'\n").unwrap();
        assert_eq!(config.time_format, TimeFormat::TwelveHour);
    }

    #[test]
    fn test_time_format_invalid() {
        let result: std::result::Result<Config, _> =
            toml::from_str("notes_dir = '/tmp/test'\ntime_format = 'military'\n");
        assert!(result.is_err());
    }

    #[test]
    fn test_time_format_display() {
        assert_eq!(TimeFormat::TwelveHour.to_string(), "12h");
        assert_eq!(TimeFormat::TwentyFourHour.to_string(), "24h");
    }

    #[test]
    fn parses_notes_aliases() {
        let toml_str = r#"
notes_dir = '/tmp/test'

[notes.aliases]
med-morning = "Morning meds"
med-evening = "Evening meds"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.notes.aliases.get("med-morning").map(String::as_str),
            Some("Morning meds")
        );
        assert_eq!(
            config.notes.aliases.get("med-evening").map(String::as_str),
            Some("Evening meds")
        );
    }

    #[test]
    fn notes_aliases_default_empty() {
        let config: Config = toml::from_str("notes_dir = '/tmp/test'\n").unwrap();
        assert!(config.notes.aliases.is_empty());
    }
}

#[cfg(test)]
mod legacy_fallback_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn config_path_falls_back_to_legacy_when_only_old_exists() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path();
        std::fs::create_dir(parent.join("daylog")).unwrap();
        std::fs::write(parent.join("daylog/config.toml"), "notes_dir = \"~/x\"\n").unwrap();

        let resolved = resolve_config_path(parent);

        assert_eq!(resolved, parent.join("daylog/config.toml"));
    }

    #[test]
    fn config_path_uses_current_when_both_exist() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path();
        std::fs::create_dir(parent.join("daylog")).unwrap();
        std::fs::create_dir(parent.join("vitalog")).unwrap();
        std::fs::write(parent.join("vitalog/config.toml"), "").unwrap();

        let resolved = resolve_config_path(parent);

        assert_eq!(resolved, parent.join("vitalog/config.toml"));
    }

    #[test]
    fn config_path_defaults_to_vitalog_when_neither_exists() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path();

        let resolved = resolve_config_path(parent);

        assert_eq!(resolved, parent.join("vitalog/config.toml"));
    }
}

#[cfg(test)]
mod env_override_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn env_override_returns_env_path_verbatim_when_set() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join("sandbox/config.toml");
        let parent = TempDir::new().unwrap();

        let resolved =
            resolve_config_path_with_env(Some(env_path.to_str().unwrap()), parent.path());

        assert_eq!(resolved, env_path);
    }

    #[test]
    fn env_override_expands_tilde() {
        let parent = TempDir::new().unwrap();

        let resolved = resolve_config_path_with_env(Some("~/sandbox/config.toml"), parent.path());

        let home = dirs::home_dir().expect("home dir");
        assert_eq!(resolved, home.join("sandbox/config.toml"));
        assert!(!resolved.to_str().unwrap().starts_with('~'));
    }

    #[test]
    fn env_override_falls_back_to_parent_resolution_when_none() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path();
        std::fs::create_dir(parent.join("vitalog")).unwrap();
        std::fs::write(parent.join("vitalog/config.toml"), "").unwrap();

        let resolved = resolve_config_path_with_env(None, parent);

        assert_eq!(resolved, parent.join("vitalog/config.toml"));
    }

    #[test]
    fn env_override_treats_empty_string_as_unset() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path();

        let resolved = resolve_config_path_with_env(Some(""), parent);

        assert_eq!(resolved, parent.join("vitalog/config.toml"));
    }

    #[test]
    fn not_found_message_mentions_env_var_when_from_env_true() {
        let msg = config_not_found_message(Path::new("/missing/sandbox.toml"), true);

        assert!(
            msg.contains("VITALOG_CONFIG"),
            "expected message to mention env var: {msg}"
        );
        assert!(
            msg.contains("/missing/sandbox.toml"),
            "expected message to include the path: {msg}"
        );
        assert!(
            !msg.contains("vitalog init"),
            "env-overridden missing path should NOT suggest `vitalog init`: {msg}"
        );
    }

    #[test]
    fn not_found_message_suggests_init_when_from_env_false() {
        let msg = config_not_found_message(Path::new("/home/u/.config/vitalog/config.toml"), false);

        assert!(
            msg.contains("vitalog init"),
            "default missing path should suggest `vitalog init`: {msg}"
        );
        assert!(
            !msg.contains("VITALOG_CONFIG"),
            "default missing path should NOT mention env var: {msg}"
        );
    }

    #[test]
    fn env_override_does_not_consult_parent_when_set() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path();
        // Set up a legacy daylog config in `parent`. If the env override
        // is honored verbatim, this legacy file must NOT be picked up.
        std::fs::create_dir(parent.join("daylog")).unwrap();
        std::fs::write(parent.join("daylog/config.toml"), "").unwrap();

        let resolved = resolve_config_path_with_env(Some("/explicit/override/config.toml"), parent);

        assert_eq!(
            resolved,
            std::path::PathBuf::from("/explicit/override/config.toml")
        );
    }
}
