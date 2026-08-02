use vitalog::config::Config;
use vitalog::db;
use vitalog::modules;

fn setup_test_env() -> (tempfile::TempDir, Config) {
    let dir = tempfile::TempDir::new().unwrap();
    let notes_dir = dir.path().to_path_buf();
    // Use forward slashes for Windows compatibility in TOML
    let notes_dir_str = notes_dir.display().to_string().replace('\\', "/");
    let toml_str = format!(
        r#"
notes_dir = "{notes_dir_str}"

[modules]
dashboard = true
training = true
trends = true
climbing = false

[exercises]
squat = {{ display = "Squat", color = "cyan" }}
pullup = {{ display = "Pullup", color = "blue" }}

[metrics]
resting_hr = {{ display = "Resting HR", color = "red", unit = "bpm" }}
"#
    );
    let config: Config = toml::from_str(&toml_str).unwrap();
    (dir, config)
}

fn setup_db(config: &Config, modules: &[Box<dyn modules::Module>]) -> rusqlite::Connection {
    let db_path = config.db_path();
    let conn = db::open_rw(&db_path).unwrap();
    db::init_db(&conn, modules).unwrap();
    modules::validate_module_tables(modules).unwrap();
    conn
}

/// Full round-trip: write notes via log_cmd -> sync -> verify DB -> verify status JSON.
#[test]
fn test_full_roundtrip() {
    let (dir, config) = setup_test_env();
    let registry = modules::build_registry(&config);
    let _conn = setup_db(&config, &registry);

    // 1. Log several values
    vitalog::cli::log_cmd::execute("weight", &["173.4".into()], &config, &registry).unwrap();
    vitalog::cli::log_cmd::execute("mood", &["4".into()], &config, &registry).unwrap();
    vitalog::cli::log_cmd::execute("energy", &["3".into()], &config, &registry).unwrap();
    vitalog::cli::log_cmd::execute("sleep", &["10:30pm-6:15am".into()], &config, &registry)
        .unwrap();
    vitalog::cli::log_cmd::execute(
        "lift",
        &["squat".into(), "185x5,".into(), "205x3".into()],
        &config,
        &registry,
    )
    .unwrap();
    vitalog::cli::log_cmd::execute(
        "metric",
        &["resting_hr".into(), "52".into()],
        &config,
        &registry,
    )
    .unwrap();

    // 2. Verify the note file exists and has correct content
    let today = config.effective_today();
    let note_path = dir.path().join(format!("{today}.md"));
    assert!(note_path.exists(), "Note file should exist");
    let content = std::fs::read_to_string(&note_path).unwrap();
    assert!(content.contains("weight: 173.4"));
    assert!(content.contains("mood: 4"));
    assert!(content.contains("energy: 3"));
    assert!(content.contains("resting_hr: 52"));
    assert!(content.contains("squat: 185x5, 205x3"));

    // 3. Sync to database
    let conn = db::open_rw(&config.db_path()).unwrap();
    db::init_db(&conn, &registry).unwrap();
    let (synced, errors) =
        vitalog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry)
            .unwrap();
    assert_eq!(synced, 1, "Should sync 1 file");
    assert_eq!(errors, 0, "Should have 0 errors");

    // 4. Verify DB data
    let day = db::load_today(&conn, &today)
        .unwrap()
        .expect("Should have today's data");
    assert_eq!(day["weight"], 173.4);
    assert_eq!(day["mood"], 4);
    assert_eq!(day["energy"], 3);
    assert_eq!(day["sleep_start"], "22:30");
    assert_eq!(day["sleep_end"], "06:15");
    assert!((day["sleep_hours"].as_f64().unwrap() - 7.75).abs() < 0.01);

    // 5. Verify metrics
    let metrics = db::load_metrics(&conn, &today).unwrap();
    let rhr = metrics.iter().find(|(k, _)| k == "resting_hr");
    assert!(rhr.is_some(), "Should have resting_hr metric");
    assert_eq!(rhr.unwrap().1, 52.0);

    // 6. Verify lift sets
    let mut stmt = conn
        .prepare("SELECT exercise, set_number, weight_lbs, reps FROM lift_sets WHERE date = ?1 ORDER BY set_number")
        .unwrap();
    let sets: Vec<(String, i32, f64, i32)> = stmt
        .query_map([&today], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, i32>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(sets.len(), 2, "Should have 2 squat sets");
    assert_eq!(sets[0], ("squat".into(), 1, 185.0, 5));
    assert_eq!(sets[1], ("squat".into(), 2, 205.0, 3));
}

/// Test that demo data generation + sync produces a populated DB.
#[test]
fn test_demo_data_roundtrip() {
    let (dir, config) = setup_test_env();
    let registry = modules::build_registry(&config);

    // Generate demo data
    let count = vitalog::demo::generate_demo_data(dir.path()).unwrap();
    assert_eq!(count, 14);

    // Sync all
    let conn = db::open_rw(&config.db_path()).unwrap();
    db::init_db(&conn, &registry).unwrap();
    modules::validate_module_tables(&registry).unwrap();
    let (synced, errors) =
        vitalog::materializer::rebuild_all(&conn, &config.notes_dir_path(), &config, &registry)
            .unwrap();
    assert_eq!(synced, 14);
    assert_eq!(errors, 0);

    // Verify we have days in the DB
    let day_count: i32 = conn
        .query_row("SELECT COUNT(*) FROM days", [], |row| row.get(0))
        .unwrap();
    assert_eq!(day_count, 14);

    // Verify we have some lift sets
    let lift_count: i32 = conn
        .query_row("SELECT COUNT(*) FROM lift_sets", [], |row| row.get(0))
        .unwrap();
    assert!(lift_count > 0, "Should have some lift sets from demo data");

    // Verify weight trend
    let weight_trend = db::load_weight_trend(&conn, 42).unwrap();
    assert!(!weight_trend.is_empty(), "Should have weight data");
}

/// Test that input validation rejects garbage.
#[test]
fn test_validation_rejects_garbage() {
    let (_, config) = setup_test_env();
    let registry = modules::build_registry(&config);

    // Invalid weight
    let result = vitalog::cli::log_cmd::execute("weight", &["banana".into()], &config, &registry);
    assert!(result.is_err());

    // Mood out of range
    let result = vitalog::cli::log_cmd::execute("mood", &["999".into()], &config, &registry);
    assert!(result.is_err());

    // Energy zero
    let result = vitalog::cli::log_cmd::execute("energy", &["0".into()], &config, &registry);
    assert!(result.is_err());

    // Bad sleep format
    let result = vitalog::cli::log_cmd::execute("sleep", &["whenever".into()], &config, &registry);
    assert!(result.is_err());

    // Unknown field
    let result = vitalog::cli::log_cmd::execute("banana", &["123".into()], &config, &registry);
    assert!(result.is_err());
}

/// Test that rebuild deletes and recreates cleanly.
#[test]
fn test_rebuild_is_idempotent() {
    let (dir, config) = setup_test_env();
    let registry = modules::build_registry(&config);

    vitalog::demo::generate_demo_data(dir.path()).unwrap();

    let conn = db::open_rw(&config.db_path()).unwrap();
    db::init_db(&conn, &registry).unwrap();
    modules::validate_module_tables(&registry).unwrap();

    // First build
    let (s1, e1) =
        vitalog::materializer::rebuild_all(&conn, &config.notes_dir_path(), &config, &registry)
            .unwrap();
    assert_eq!(e1, 0);

    // Second build (should produce identical results)
    let (s2, e2) =
        vitalog::materializer::rebuild_all(&conn, &config.notes_dir_path(), &config, &registry)
            .unwrap();
    assert_eq!(s1, s2);
    assert_eq!(e2, 0);

    let count: i32 = conn
        .query_row("SELECT COUNT(*) FROM days", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 14, "Should still have exactly 14 days after rebuild");
}

#[test]
fn sync_all_includes_nutrition_db() {
    let dir = tempfile::TempDir::new().unwrap();
    let notes_dir = dir.path();
    std::fs::write(
        notes_dir.join("2026-04-29.md"),
        "---\ndate: 2026-04-29\nweight: 173.4\n---\n",
    )
    .unwrap();
    std::fs::write(
        notes_dir.join("nutrition-db.md"),
        "## Apple\n\n```yaml\nper_100g:\n  kcal: 52\n```\n",
    )
    .unwrap();

    let db_path = notes_dir.join(".vitalog.db");
    let config: vitalog::config::Config = toml::from_str(&format!(
        "notes_dir = '{}'\n",
        notes_dir.display().to_string().replace('\\', "/")
    ))
    .unwrap();
    let registry = vitalog::modules::build_registry(&config);
    let conn = vitalog::db::open_rw(&db_path).unwrap();
    vitalog::db::init_db(&conn, &registry).unwrap();
    vitalog::modules::validate_module_tables(&registry).unwrap();

    let (synced, errors) =
        vitalog::materializer::sync_all(&conn, notes_dir, &config, &registry).unwrap();
    assert_eq!(errors, 0);
    assert!(synced >= 2, "expected at least 2 synced (1 note + 1 db)");

    let foods_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM foods", [], |r| r.get(0))
        .unwrap();
    assert_eq!(foods_count, 1);
}

#[test]
fn rebuild_reparses_nutrition_unconditionally() {
    let dir = tempfile::TempDir::new().unwrap();
    let notes_dir = dir.path();
    std::fs::write(
        notes_dir.join("nutrition-db.md"),
        "## Apple\n\n```yaml\nper_100g:\n  kcal: 52\n```\n",
    )
    .unwrap();

    let db_path = notes_dir.join(".vitalog.db");
    let config: vitalog::config::Config = toml::from_str(&format!(
        "notes_dir = '{}'\n",
        notes_dir.display().to_string().replace('\\', "/")
    ))
    .unwrap();
    let registry = vitalog::modules::build_registry(&config);
    let conn = vitalog::db::open_rw(&db_path).unwrap();
    vitalog::db::init_db(&conn, &registry).unwrap();
    vitalog::modules::validate_module_tables(&registry).unwrap();

    vitalog::materializer::sync_all(&conn, notes_dir, &config, &registry).unwrap();
    // Mark sync time in the future so a normal sync_all would skip the file.
    let future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        + 86_400.0;
    vitalog::db::set_last_sync(&conn, future).unwrap();
    // Tweak the file to simulate an updated value but with stale mtime
    // is impossible portably; rebuild should run regardless of mtime.
    std::fs::write(
        notes_dir.join("nutrition-db.md"),
        "## Apple\n\n```yaml\nper_100g:\n  kcal: 99\n```\n",
    )
    .unwrap();

    vitalog::materializer::rebuild_all(&conn, notes_dir, &config, &registry).unwrap();

    let kcal: f64 = conn
        .query_row(
            "SELECT kcal_per_100g FROM foods WHERE name = 'Apple'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kcal, 99.0);
}

#[test]
fn sync_all_silent_when_nutrition_db_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let notes_dir = dir.path();
    std::fs::write(
        notes_dir.join("2026-04-29.md"),
        "---\ndate: 2026-04-29\n---\n",
    )
    .unwrap();
    // No nutrition-db.md.

    let db_path = notes_dir.join(".vitalog.db");
    let config: vitalog::config::Config = toml::from_str(&format!(
        "notes_dir = '{}'\n",
        notes_dir.display().to_string().replace('\\', "/")
    ))
    .unwrap();
    let registry = vitalog::modules::build_registry(&config);
    let conn = vitalog::db::open_rw(&db_path).unwrap();
    vitalog::db::init_db(&conn, &registry).unwrap();
    vitalog::modules::validate_module_tables(&registry).unwrap();

    let (_synced, errors) =
        vitalog::materializer::sync_all(&conn, notes_dir, &config, &registry).unwrap();
    assert_eq!(errors, 0);

    let foods_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM foods", [], |r| r.get(0))
        .unwrap();
    assert_eq!(foods_count, 0);
}

#[test]
fn status_json_includes_nutrition_db() {
    let dir = tempfile::TempDir::new().unwrap();
    let notes_dir = dir.path();
    std::fs::write(
        notes_dir.join("nutrition-db.md"),
        "## Apple\n\n```yaml\nper_100g:\n  kcal: 52\n```\n",
    )
    .unwrap();

    let db_path = notes_dir.join(".vitalog.db");
    let config: vitalog::config::Config = toml::from_str(&format!(
        "notes_dir = '{}'\n",
        notes_dir.display().to_string().replace('\\', "/")
    ))
    .unwrap();
    let registry = vitalog::modules::build_registry(&config);
    let conn = vitalog::db::open_rw(&db_path).unwrap();
    vitalog::db::init_db(&conn, &registry).unwrap();
    vitalog::modules::validate_module_tables(&registry).unwrap();

    vitalog::materializer::sync_all(&conn, notes_dir, &config, &registry).unwrap();

    let status = vitalog::db::nutrition_status(&conn).unwrap();
    assert_eq!(status.foods_count, 1);
    assert!(status.last_synced.is_some());
}

/// End-to-end: run food + bp + note on a fresh today's note and verify
/// the resulting markdown has all three sections in canonical order
/// with their respective entries.
#[test]
fn test_food_note_bp_full_day() {
    use vitalog::db::{FoodInsert, NutrientPanel};

    let dir = tempfile::TempDir::new().unwrap();
    let notes_dir = dir.path().to_path_buf();
    let notes_dir_str = notes_dir.display().to_string().replace('\\', "/");
    let toml_str = format!(
        r#"
notes_dir = "{notes_dir_str}"
time_format = "24h"

[modules]
dashboard = true
training = true
trends = true
climbing = false
"#
    );
    let config: vitalog::config::Config = toml::from_str(&toml_str).unwrap();
    let registry = modules::build_registry(&config);
    let _conn = setup_db(&config, &registry);

    // Seed the nutrition DB with one entry for the food lookup.
    let conn = db::open_rw(&config.db_path()).unwrap();
    db::insert_food(
        &conn,
        &FoodInsert {
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
    drop(conn);

    vitalog::cli::bp_cmd::execute(
        141,
        96,
        70,
        false,
        false,
        None,
        Some("07:30"),
        &config,
        true,
    )
    .unwrap();
    vitalog::cli::food_cmd::execute(
        "kelda skogssvampsoppa",
        Some("500g"),
        vitalog::cli::food_cmd::NutrientArgs::default(),
        None,
        Some("12:42"),
        &config,
        true,
    )
    .unwrap();
    vitalog::cli::note_cmd::execute(
        &["Attentin".into(), "10mg".into()],
        None,
        Some("13:00"),
        &config,
        true,
    )
    .unwrap();

    let date = config.effective_today();
    let note = std::fs::read_to_string(dir.path().join(format!("{date}.md"))).unwrap();

    // YAML scalars from BP.
    assert!(note.contains("bp_morning_sys: 141"), "got:\n{note}");
    assert!(note.contains("bp_morning_dia: 96"));
    assert!(note.contains("bp_morning_pulse: 70"));

    // Sections in canonical order.
    let food = note.find("## Food").expect("## Food");
    let vitals = note.find("## Vitals").expect("## Vitals");
    let notes_h = note.find("## Notes").expect("## Notes");
    assert!(food < vitals && vitals < notes_h, "wrong order:\n{note}");

    // Each section has its line.
    assert!(note.contains("- **07:30** BP: 141/96, pulse 70 bpm"));
    assert!(note.contains("- **12:42** Kelda Skogssvampsoppa (500g)"));
    assert!(note.contains("- **13:00** Attentin 10mg"));
}
