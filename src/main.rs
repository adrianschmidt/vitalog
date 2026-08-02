use clap::Parser;
use color_eyre::eyre::{Result, WrapErr};
use std::io::{IsTerminal, Write};

use vitalog::cli::{Cli, Commands};
use vitalog::config::{self, Config};
use vitalog::db;
use vitalog::modules;

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let quiet = cli.quiet;

    match cli.command {
        Some(Commands::Init { notes_dir, no_demo }) => cmd_init(notes_dir, no_demo),
        Some(Commands::Migrate) => vitalog::cli::migrate_cmd::execute(),
        Some(Commands::Log { field, value }) => cmd_log(&field, &value),
        Some(Commands::Status) => cmd_status(),
        Some(Commands::Sync) => cmd_sync(),
        Some(Commands::Edit { date }) => cmd_edit(date.as_deref()),
        Some(Commands::Rebuild) => cmd_rebuild(),
        Some(Commands::Completions { shell }) => {
            vitalog::cli::completions::generate(shell);
            Ok(())
        }
        Some(Commands::Readme) => {
            vitalog::cli::readme_cmd::execute();
            Ok(())
        }
        Some(Commands::SleepStart { time }) => cmd_sleep_start(time.as_deref()),
        Some(Commands::SleepEnd { time }) => cmd_sleep_end(time.as_deref()),
        Some(Commands::Food {
            name,
            amount,
            nutrients,
            date,
            time,
        }) => cmd_food(name, amount, nutrients, date, time, quiet),
        Some(Commands::Note { text, date, time }) => cmd_note(text, date, time, quiet),
        Some(Commands::Bp {
            sys,
            dia,
            pulse,
            morning,
            evening,
            date,
            time,
        }) => cmd_bp(sys, dia, pulse, morning, evening, date, time, quiet),
        Some(Commands::Today { date, json }) => cmd_today(date, json),
        Some(Commands::Trend {
            field,
            days,
            compact,
            json,
        }) => cmd_trend(field, days, compact, json),
        None => cmd_run(),
    }
}

fn cmd_sleep_start(time: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    vitalog::cli::sleep_cmd::cmd_sleep_start(time, &config)
}

fn cmd_sleep_end(time: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    vitalog::cli::sleep_cmd::cmd_sleep_end(time, &config)
}

fn cmd_init(notes_dir_arg: Option<String>, no_demo: bool) -> Result<()> {
    let config_dir = Config::config_dir()?;
    let config_path = Config::config_path()?;

    if config_path.exists() {
        eprintln!(
            "Config already exists at {}. Delete it first to re-initialize.",
            config_path.display()
        );
        return Ok(());
    }

    // Determine notes_dir
    let notes_dir = if let Some(dir) = notes_dir_arg {
        dir
    } else if std::io::stdin().is_terminal() {
        print!("Notes directory [~/vitalog-notes/]: ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim();
        if input.is_empty() {
            "~/vitalog-notes".to_string()
        } else {
            input.to_string()
        }
    } else {
        "~/vitalog-notes".to_string()
    };

    // Create directories
    std::fs::create_dir_all(&config_dir)?;
    let notes_path = config::expand_tilde(&notes_dir);
    std::fs::create_dir_all(&notes_path)?;

    // Write config
    let config_content = format!(
        "notes_dir = \"{}\"\n\n{}",
        notes_dir,
        config::default_config_contents()
            .lines()
            .skip(1) // skip the notes_dir line from preset
            .collect::<Vec<_>>()
            .join("\n")
    );
    std::fs::write(&config_path, config_content)?;
    eprintln!("Created {}", config_path.display());

    // Generate demo data
    if !no_demo {
        let existing_notes: Vec<_> = std::fs::read_dir(&notes_path)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
            .collect();

        let should_demo = if !existing_notes.is_empty() && std::io::stdin().is_terminal() {
            print!(
                "Found {} existing notes. Skip demo generation? [Y/n]: ",
                existing_notes.len()
            );
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            input.trim().to_lowercase() == "n"
        } else {
            existing_notes.is_empty()
        };

        if should_demo {
            let count = vitalog::demo::generate_demo_data(&notes_path)?;
            eprintln!("Generated {count} days of demo data");
        }
    }

    // Run initial sync
    let config = Config::load()?;
    let registry = modules::build_registry(&config);
    let db_path = config.db_path();
    let conn = db::open_rw(&db_path)?;
    db::init_db(&conn, &registry)?;
    modules::validate_module_tables(&registry)?;
    let (synced, errors) =
        vitalog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry)?;
    if synced > 0 {
        eprintln!("Synced {synced} notes to database");
    }
    if errors > 0 {
        eprintln!("{errors} notes had parse errors");
    }

    eprintln!("Run `vitalog` to start!");
    Ok(())
}

fn cmd_log(field: &str, value: &[String]) -> Result<()> {
    let config = Config::load()?;
    let registry = modules::build_registry(&config);
    vitalog::cli::log_cmd::execute(field, value, &config, &registry)
}

fn cmd_status() -> Result<()> {
    let config = Config::load()?;
    vitalog::cli::status_cmd::execute(&config)
}

fn cmd_sync() -> Result<()> {
    let config = Config::load()?;
    let registry = modules::build_registry(&config);
    let db_path = config.db_path();
    let conn = db::open_rw(&db_path)?;
    db::init_db(&conn, &registry)?;
    modules::validate_module_tables(&registry)?;

    let (synced, errors) =
        vitalog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry)?;
    eprintln!("Synced {synced} files ({errors} errors)");
    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_edit(date: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    let date_str = match date {
        Some(d) => d.to_string(),
        None => config.effective_today(),
    };
    let note_path = config.notes_dir_path().join(format!("{date_str}.md"));

    if !note_path.exists() {
        let content = vitalog::template::render_daily_note(&date_str, &config);
        std::fs::write(&note_path, content)?;
    }

    // Show day-boundary hint when effective date differs from calendar date
    if date.is_none() {
        let calendar_today = chrono::Local::now().format("%Y-%m-%d").to_string();
        if date_str != calendar_today {
            eprintln!(
                "Editing {date_str} (day boundary: before {}:00)",
                config.day_start_hour
            );
        }
    }

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    std::process::Command::new(&editor)
        .arg(&note_path)
        .status()
        .wrap_err_with(|| format!("Failed to open editor: {editor}"))?;

    Ok(())
}

fn cmd_rebuild() -> Result<()> {
    let config = Config::load()?;
    let registry = modules::build_registry(&config);
    let db_path = config.db_path();

    // Delete existing DB
    if db_path.exists() {
        std::fs::remove_file(&db_path)?;
        eprintln!("Deleted {}", db_path.display());
    }

    let conn = db::open_rw(&db_path)?;
    db::init_db(&conn, &registry)?;
    modules::validate_module_tables(&registry)?;

    let (synced, errors) =
        vitalog::materializer::rebuild_all(&conn, &config.notes_dir_path(), &config, &registry)?;
    eprintln!("Rebuilt: {synced} files ({errors} errors)");
    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_run() -> Result<()> {
    vitalog::app::run()
}

fn cmd_food(
    name: String,
    amount: Option<String>,
    nutrients: vitalog::cli::food_cmd::NutrientArgs,
    date: Option<String>,
    time: Option<String>,
    quiet: bool,
) -> Result<()> {
    let config = Config::load()?;
    vitalog::cli::food_cmd::execute(
        &name,
        amount.as_deref(),
        nutrients,
        date.as_deref(),
        time.as_deref(),
        &config,
        quiet,
    )
}

fn cmd_note(
    text: Vec<String>,
    date: Option<String>,
    time: Option<String>,
    quiet: bool,
) -> Result<()> {
    let config = Config::load()?;
    vitalog::cli::note_cmd::execute(&text, date.as_deref(), time.as_deref(), &config, quiet)
}

#[allow(clippy::too_many_arguments)]
fn cmd_bp(
    sys: i32,
    dia: i32,
    pulse: i32,
    morning: bool,
    evening: bool,
    date: Option<String>,
    time: Option<String>,
    quiet: bool,
) -> Result<()> {
    let config = Config::load()?;
    vitalog::cli::bp_cmd::execute(
        sys,
        dia,
        pulse,
        morning,
        evening,
        date.as_deref(),
        time.as_deref(),
        &config,
        quiet,
    )
}

fn cmd_today(date: Option<String>, json: bool) -> Result<()> {
    let config = Config::load()?;
    vitalog::cli::today_cmd::execute(date.as_deref(), json, &config)
}

fn cmd_trend(field: String, days: u32, compact: bool, json: bool) -> Result<()> {
    let config = Config::load()?;
    vitalog::cli::trend_cmd::execute(&field, days, compact, json, &config)
}
