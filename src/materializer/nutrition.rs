use crate::config::Config;
use crate::db::{FoodIngredient, FoodInsert, NutrientPanel, TotalPanel};
use color_eyre::eyre::{eyre, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::LazyLock;
use yaml_rust2::{Yaml, YamlLoader};

#[derive(Debug, Clone)]
pub(crate) struct ParsedEntry {
    pub name: String,
    pub yaml: Yaml,
    pub notes: Option<String>,
    pub line_number: usize,
}

static RE_HEADING: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^##\s+(.+?)\s*$").unwrap());
static RE_YAML_FENCE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^```yaml\s*$").unwrap());
static RE_FENCE_CLOSE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^```\s*$").unwrap());

#[derive(Default)]
struct PendingEntry {
    name: String,
    line_number: usize,
    yaml_lines: Vec<String>,
    notes_lines: Vec<String>,
    in_yaml_fence: bool,
    yaml_seen: bool,
}

pub(crate) fn split_entries(content: &str) -> Vec<ParsedEntry> {
    let mut entries = Vec::new();
    let mut current: Option<PendingEntry> = None;

    for (idx, line) in content.lines().enumerate() {
        let lineno = idx + 1;

        if let Some(caps) = RE_HEADING.captures(line) {
            // Flush previous entry before starting a new one.
            if let Some(prev) = current.take() {
                if let Some(entry) = finalize(prev) {
                    entries.push(entry);
                }
            }
            current = Some(PendingEntry {
                name: caps.get(1).unwrap().as_str().to_string(),
                line_number: lineno,
                ..Default::default()
            });
            continue;
        }

        let Some(entry) = current.as_mut() else {
            continue; // before first ## heading: ignore
        };

        if entry.in_yaml_fence {
            if RE_FENCE_CLOSE.is_match(line) {
                entry.in_yaml_fence = false;
            } else {
                entry.yaml_lines.push(line.to_string());
            }
            continue;
        }

        if !entry.yaml_seen && RE_YAML_FENCE.is_match(line) {
            entry.in_yaml_fence = true;
            entry.yaml_seen = true;
            continue;
        }

        // Anything else (prose, dividers) goes to notes.
        entry.notes_lines.push(line.to_string());
    }

    if let Some(prev) = current.take() {
        if let Some(entry) = finalize(prev) {
            entries.push(entry);
        }
    }
    entries
}

fn finalize(pending: PendingEntry) -> Option<ParsedEntry> {
    if !pending.yaml_seen {
        eprintln!(
            "Warning: nutrition-db.md entry '{}' (line {}) has no fenced YAML block — skipped",
            pending.name, pending.line_number
        );
        return None;
    }
    let yaml_str = pending.yaml_lines.join("\n");
    let yaml = match YamlLoader::load_from_str(&yaml_str) {
        Ok(mut docs) if !docs.is_empty() => docs.remove(0),
        Ok(_) => Yaml::Null,
        Err(_) => Yaml::BadValue,
    };
    let notes_joined = pending
        .notes_lines
        .join("\n")
        .trim_matches(|c: char| c == '\n' || c.is_whitespace())
        .to_string();
    let notes = if notes_joined.is_empty() {
        None
    } else {
        Some(notes_joined)
    };
    Some(ParsedEntry {
        name: pending.name,
        yaml,
        notes,
        line_number: pending.line_number,
    })
}

const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "per_100g",
    "per_100ml",
    "density_g_per_ml",
    "gi",
    "gl_per_100g",
    "gl_per_100ml",
    "ii",
    "aliases",
    "description",
    "ingredients",
    "total",
];

pub(crate) fn build_food_insert(entry: &ParsedEntry) -> Result<FoodInsert> {
    let yaml = &entry.yaml;
    if matches!(yaml, Yaml::BadValue | Yaml::Null) {
        return Err(eyre!(
            "entry '{}' (line {}): YAML block is empty or malformed",
            entry.name,
            entry.line_number
        ));
    }

    let per_100g = read_nutrient_panel(&yaml["per_100g"]);
    let per_100ml = read_nutrient_panel(&yaml["per_100ml"]);
    let total = read_total_panel(&yaml["total"]);
    if per_100g.is_none() && per_100ml.is_none() && total.is_none() {
        return Err(eyre!(
            "entry '{}' (line {}): must include per_100g, per_100ml, or total",
            entry.name,
            entry.line_number
        ));
    }

    let density_g_per_ml = read_real(&yaml["density_g_per_ml"]);
    if let Some(d) = density_g_per_ml {
        // `!is_finite()` first: `+inf` passes `d <= 0.0`, and every NaN
        // comparison is false so NaN passes it too. An infinite density
        // collapses the g→ml factor to zero and writes a full panel of
        // `0.0g` tokens that read back as *measured* zeros — the one state
        // the unknown counting exists to prevent.
        if !d.is_finite() || d <= 0.0 {
            return Err(eyre!(
                "entry '{}' (line {}): density_g_per_ml must be a positive \
                 finite number (got {})",
                entry.name,
                entry.line_number,
                d
            ));
        }
    }

    let gi = read_real(&yaml["gi"]);
    let gl_per_100g = read_real(&yaml["gl_per_100g"]);
    let gl_per_100ml = read_real(&yaml["gl_per_100ml"]);
    let ii = read_real(&yaml["ii"]);
    for (label, val) in [("gi", gi), ("ii", ii)] {
        if let Some(v) = val {
            if !(0.0..=200.0).contains(&v) {
                eprintln!(
                    "Warning: entry '{}' (line {}): {label}={v} outside 0..200 (still stored)",
                    entry.name, entry.line_number
                );
            }
        }
    }

    let description = read_string(&yaml["description"]);
    let notes = entry.notes.clone();

    let mut aliases: Vec<String> = read_string_list(&yaml["aliases"])
        .into_iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    aliases.push(entry.name.trim().to_lowercase());
    aliases.sort();
    aliases.dedup();

    let ingredients = read_ingredients(&yaml["ingredients"], &entry.name, entry.line_number);

    if let Yaml::Hash(hash) = yaml {
        for (k, _) in hash.iter() {
            if let Yaml::String(key) = k {
                if !KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()) {
                    eprintln!(
                        "Warning: entry '{}' (line {}): unknown key '{key}' ignored",
                        entry.name, entry.line_number
                    );
                }
            }
        }
    }

    Ok(FoodInsert {
        name: entry.name.trim().to_string(),
        per_100g,
        per_100ml,
        density_g_per_ml,
        total,
        gi,
        gl_per_100g,
        gl_per_100ml,
        ii,
        description,
        notes,
        aliases,
        ingredients,
    })
}

fn read_nutrient_panel(node: &Yaml) -> Option<NutrientPanel> {
    if matches!(node, Yaml::BadValue | Yaml::Null) {
        return None;
    }
    let panel = NutrientPanel {
        kcal: read_real(&node["kcal"]),
        protein: read_real(&node["protein"]),
        carbs: read_real(&node["carbs"]),
        fat: read_real(&node["fat"]),
        sat_fat: read_real(&node["sat_fat"]),
        sugar: read_real(&node["sugar"]),
        salt: read_real(&node["salt"]),
        fiber: read_real(&node["fiber"]),
    };
    if panel == NutrientPanel::default() {
        None
    } else {
        Some(panel)
    }
}

fn read_total_panel(node: &Yaml) -> Option<TotalPanel> {
    if matches!(node, Yaml::BadValue | Yaml::Null) {
        return None;
    }
    let panel = TotalPanel {
        weight_g: read_real(&node["weight_g"]),
        kcal: read_real(&node["kcal"]),
        protein: read_real(&node["protein"]),
        carbs: read_real(&node["carbs"]),
        fat: read_real(&node["fat"]),
        sat_fat: read_real(&node["sat_fat"]),
        sugar: read_real(&node["sugar"]),
        salt: read_real(&node["salt"]),
        fiber: read_real(&node["fiber"]),
    };
    if panel == TotalPanel::default() {
        None
    } else {
        Some(panel)
    }
}

fn read_real(node: &Yaml) -> Option<f64> {
    match node {
        Yaml::Real(s) => s.parse().ok(),
        Yaml::Integer(i) => Some(*i as f64),
        Yaml::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn read_string(node: &Yaml) -> Option<String> {
    match node {
        Yaml::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

fn read_string_list(node: &Yaml) -> Vec<String> {
    match node {
        Yaml::Array(arr) => arr
            .iter()
            .filter_map(|item| match item {
                Yaml::String(s) => Some(s.clone()),
                Yaml::Integer(i) => Some(i.to_string()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

fn read_ingredients(node: &Yaml, entry_name: &str, lineno: usize) -> Vec<FoodIngredient> {
    let Yaml::Array(arr) = node else {
        return vec![];
    };
    let mut out = Vec::new();
    for item in arr {
        let Yaml::Hash(_) = item else {
            eprintln!(
                "Warning: entry '{entry_name}' (line {lineno}): ingredient is not a mapping — skipped"
            );
            continue;
        };
        let food = read_string(&item["food"]);
        let Some(food_name) = food else {
            eprintln!(
                "Warning: entry '{entry_name}' (line {lineno}): ingredient missing 'food' — skipped"
            );
            continue;
        };
        out.push(FoodIngredient {
            ingredient_name: food_name,
            amount_g: read_real(&item["amount_g"]),
        });
    }
    out
}

pub fn materialize_nutrition_db(
    conn: &Connection,
    file_path: &Path,
    _config: &Config,
) -> Result<usize> {
    if !file_path.exists() {
        return Ok(0);
    }
    let content = std::fs::read_to_string(file_path)
        .map_err(|e| eyre!("failed to read {}: {e}", file_path.display()))?;

    let entries = split_entries(&content);
    if entries.is_empty() {
        return Ok(0);
    }

    let mut food_inserts: Vec<FoodInsert> = Vec::new();
    for entry in &entries {
        match build_food_insert(entry) {
            Ok(fi) => food_inserts.push(fi),
            Err(e) => eprintln!(
                "Warning: nutrition-db.md entry '{}' (line {}): {e}",
                entry.name, entry.line_number
            ),
        }
    }

    let tx = conn.unchecked_transaction()?;
    crate::db::delete_all_foods(&tx)?;

    let mut inserted = 0usize;
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for fi in &food_inserts {
        if !seen_names.insert(fi.name.to_lowercase()) {
            eprintln!(
                "Warning: nutrition-db.md duplicate heading '{}' — first occurrence kept",
                fi.name
            );
            continue;
        }
        match crate::db::insert_food(&tx, fi) {
            Ok(_id) => inserted += 1,
            Err(e) => eprintln!(
                "Warning: nutrition-db.md insert failed for '{}': {e}",
                fi.name
            ),
        }
    }

    let mtime = std::fs::metadata(file_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| {
            let secs = d.as_secs() as i64;
            chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    tx.execute(
        "INSERT OR REPLACE INTO sync_meta (key, value) VALUES ('last_nutrition_sync', ?1)",
        [mtime],
    )?;
    tx.commit()?;

    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use yaml_rust2::Yaml;

    #[test]
    fn split_basic_three_entries() {
        let content = r#"# Nutrition

## Kelda Skogssvampsoppa

```yaml
per_100g:
  kcal: 70
```

Some prose here.

---

## Laktosfri helmjölk 3%

```yaml
per_100ml:
  kcal: 62
```

## proteinshake

```yaml
description: shake
total:
  weight_g: 462
  kcal: 234
```
"#;
        let entries = split_entries(content);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "Kelda Skogssvampsoppa");
        assert_eq!(entries[1].name, "Laktosfri helmjölk 3%");
        assert_eq!(entries[2].name, "proteinshake");
    }

    #[test]
    fn split_attaches_notes_below_yaml() {
        let content = r#"## Foo

```yaml
per_100g:
  kcal: 100
```

Free prose under the block.

More prose.
"#;
        let entries = split_entries(content);
        assert_eq!(entries.len(), 1);
        let notes = entries[0].notes.as_ref().unwrap();
        assert!(notes.contains("Free prose"));
        assert!(notes.contains("More prose"));
    }

    #[test]
    fn split_skips_heading_without_yaml_block() {
        let content = r#"## NoYaml

just some words.

## HasYaml

```yaml
per_100g:
  kcal: 50
```
"#;
        let entries = split_entries(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "HasYaml");
    }

    #[test]
    fn split_tolerates_h1_and_dividers() {
        let content = r#"# Top Title

---

## Foo

```yaml
per_100g:
  kcal: 1
```

---
"#;
        let entries = split_entries(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Foo");
    }

    #[test]
    fn split_records_line_numbers() {
        let content = r#"## First

```yaml
per_100g:
  kcal: 1
```

## Second

```yaml
per_100ml:
  kcal: 1
```
"#;
        let entries = split_entries(content);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].line_number, 1);
        assert!(entries[1].line_number > entries[0].line_number);
    }

    #[test]
    fn split_empty_file_returns_empty() {
        assert!(split_entries("").is_empty());
        assert!(split_entries("# Just a title\n").is_empty());
    }

    #[test]
    fn split_yaml_with_syntax_error_still_returns_entry() {
        // The splitter shouldn't decide validity — that's build_food_insert's job.
        // Bad YAML → entry.yaml is Yaml::BadValue, but the entry is still returned.
        let content = r#"## Broken

```yaml
per_100g:
  kcal: : :
```
"#;
        let entries = split_entries(content);
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].yaml, Yaml::BadValue));
    }

    use yaml_rust2::YamlLoader;

    fn parse(name: &str, yaml_str: &str) -> ParsedEntry {
        let yaml = YamlLoader::load_from_str(yaml_str)
            .ok()
            .and_then(|mut d| {
                if d.is_empty() {
                    None
                } else {
                    Some(d.remove(0))
                }
            })
            .unwrap_or(Yaml::Null);
        ParsedEntry {
            name: name.to_string(),
            yaml,
            notes: None,
            line_number: 1,
        }
    }

    #[test]
    fn build_basic_per_100g() {
        let entry = parse(
            "Kelda Skogssvampsoppa",
            "per_100g:\n  kcal: 70\n  protein: 1.4\ngi: 40\n",
        );
        let fi = build_food_insert(&entry).unwrap();
        assert_eq!(fi.name, "Kelda Skogssvampsoppa");
        assert_eq!(fi.per_100g.as_ref().unwrap().kcal, Some(70.0));
        assert_eq!(fi.per_100g.as_ref().unwrap().protein, Some(1.4));
        assert_eq!(fi.gi, Some(40.0));
        // Heading is auto-added as a lowercased alias.
        assert!(fi.aliases.contains(&"kelda skogssvampsoppa".to_string()));
    }

    #[test]
    fn build_basic_per_100ml_with_density() {
        let entry = parse(
            "Helmjölk",
            "per_100ml:\n  kcal: 62\ndensity_g_per_ml: 1.03\n",
        );
        let fi = build_food_insert(&entry).unwrap();
        assert_eq!(fi.per_100ml.as_ref().unwrap().kcal, Some(62.0));
        assert_eq!(fi.density_g_per_ml, Some(1.03));
    }

    #[test]
    fn build_total_only_is_valid() {
        let entry = parse(
            "proteinshake",
            "description: 62g pulver + 4 dl vatten\n\
             total:\n  weight_g: 462\n  kcal: 234\n  protein: 48\n",
        );
        let fi = build_food_insert(&entry).unwrap();
        assert!(fi.total.is_some());
        assert_eq!(fi.total.as_ref().unwrap().kcal, Some(234.0));
        assert_eq!(fi.description.as_deref(), Some("62g pulver + 4 dl vatten"));
    }

    #[test]
    fn build_rejects_no_panel() {
        let entry = parse("Empty", "gi: 40\n");
        let err = build_food_insert(&entry).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Empty") && msg.contains("per_100g"),
            "expected error to mention entry name and missing panels: {msg}"
        );
    }

    #[test]
    fn build_rejects_zero_density() {
        let entry = parse("Bad", "per_100g:\n  kcal: 50\ndensity_g_per_ml: 0\n");
        let err = build_food_insert(&entry).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("density"), "expected density error: {msg}");
    }

    #[test]
    fn build_aliases_normalized_and_deduped() {
        let entry = parse(
            "Foo Bar",
            "per_100g:\n  kcal: 1\naliases: [Foo, \"FOO\", \"Bar Baz\", \"foo bar\"]\n",
        );
        let fi = build_food_insert(&entry).unwrap();
        // heading auto-added, all lowercased, deduped
        let mut aliases = fi.aliases.clone();
        aliases.sort();
        assert!(aliases.contains(&"foo".to_string()));
        assert!(aliases.contains(&"bar baz".to_string()));
        assert!(aliases.contains(&"foo bar".to_string()));
        // dedup
        let unique_count = {
            let mut a = aliases.clone();
            a.dedup();
            a.len()
        };
        assert_eq!(unique_count, aliases.len());
        assert!(!fi.aliases.iter().any(|a| a.is_empty()));
    }

    #[test]
    fn build_ingredients_preserve_order() {
        let entry = parse(
            "Composite",
            "total:\n  kcal: 100\n\
             ingredients:\n  - food: Whey\n    amount_g: 62\n  - food: Water\n  - food: Sugar\n    amount_g: 5\n",
        );
        let fi = build_food_insert(&entry).unwrap();
        assert_eq!(fi.ingredients.len(), 3);
        assert_eq!(fi.ingredients[0].ingredient_name, "Whey");
        assert_eq!(fi.ingredients[0].amount_g, Some(62.0));
        assert_eq!(fi.ingredients[1].ingredient_name, "Water");
        assert_eq!(fi.ingredients[1].amount_g, None);
        assert_eq!(fi.ingredients[2].ingredient_name, "Sugar");
    }

    #[test]
    fn build_ingredient_missing_food_skipped() {
        let entry = parse(
            "Composite",
            "total:\n  kcal: 100\n\
             ingredients:\n  - amount_g: 50\n  - food: Whey\n    amount_g: 62\n",
        );
        let fi = build_food_insert(&entry).unwrap();
        assert_eq!(fi.ingredients.len(), 1);
        assert_eq!(fi.ingredients[0].ingredient_name, "Whey");
    }

    #[test]
    fn build_unknown_top_level_key_warns_not_errors() {
        let entry = parse("Foo", "per_100g:\n  kcal: 1\ntags: [foo, bar]\n");
        // Should not panic / error; entry built normally.
        let fi = build_food_insert(&entry).unwrap();
        assert!(fi.per_100g.is_some());
    }

    #[test]
    fn build_uses_notes_when_present() {
        let mut entry = parse("Foo", "per_100g:\n  kcal: 1\n");
        entry.notes = Some("Some prose.".to_string());
        let fi = build_food_insert(&entry).unwrap();
        assert_eq!(fi.notes.as_deref(), Some("Some prose."));
    }

    use crate::db::CORE_SCHEMA_TEST_HOOK;
    use std::io::Write;
    use tempfile::TempDir;

    fn open_inmem_with_schema() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(CORE_SCHEMA_TEST_HOOK).unwrap();
        conn
    }

    fn write_fixture(dir: &TempDir, content: &str) -> std::path::PathBuf {
        let path = dir.path().join("nutrition-db.md");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    fn empty_config() -> Config {
        toml::from_str("notes_dir = '/tmp'\n").unwrap()
    }

    #[test]
    fn materialize_three_entries_e2e() {
        let conn = open_inmem_with_schema();
        let dir = TempDir::new().unwrap();
        let content = r#"# Nutrition

## Kelda Skogssvampsoppa

```yaml
per_100g:
  kcal: 70
  protein: 1.4
gi: 40
aliases: [skogssvampsoppa]
```

## Helmjölk

```yaml
per_100ml:
  kcal: 62
density_g_per_ml: 1.03
aliases: [mjölk]
```

## proteinshake

```yaml
description: 62g pulver + 4 dl vatten
total:
  weight_g: 462
  kcal: 234
ingredients:
  - food: Whey
    amount_g: 62
  - food: Water
    amount_g: 400
```
"#;
        let path = write_fixture(&dir, content);
        let n = materialize_nutrition_db(&conn, &path, &empty_config()).unwrap();
        assert_eq!(n, 3);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM foods", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);

        let alias_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM food_aliases", [], |r| r.get(0))
            .unwrap();
        // 3 headings + 1 explicit alias each (skogssvampsoppa, mjölk) +
        // proteinshake heading already auto-added → 5
        assert!(alias_count >= 5);

        let ingredient_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM food_ingredients", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ingredient_count, 2);
    }

    #[test]
    fn materialize_replaces_on_rerun() {
        let conn = open_inmem_with_schema();
        let dir = TempDir::new().unwrap();

        let v1 = "## A\n\n```yaml\nper_100g:\n  kcal: 1\n```\n\n## B\n\n```yaml\nper_100g:\n  kcal: 2\n```\n";
        let v2 = "## A\n\n```yaml\nper_100g:\n  kcal: 1\n```\n";

        let path = write_fixture(&dir, v1);
        materialize_nutrition_db(&conn, &path, &empty_config()).unwrap();
        std::fs::write(&path, v2).unwrap();
        materialize_nutrition_db(&conn, &path, &empty_config()).unwrap();

        let names: Vec<String> = conn
            .prepare("SELECT name FROM foods ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(names, vec!["A".to_string()]);
    }

    #[test]
    fn materialize_partial_failure_continues() {
        let conn = open_inmem_with_schema();
        let dir = TempDir::new().unwrap();
        let content = r#"## Good1

```yaml
per_100g:
  kcal: 1
```

## Bad

```yaml
gi: 40
```

## Good2

```yaml
per_100g:
  kcal: 2
```
"#;
        let path = write_fixture(&dir, content);
        let n = materialize_nutrition_db(&conn, &path, &empty_config()).unwrap();
        assert_eq!(n, 2);

        let names: Vec<String> = conn
            .prepare("SELECT name FROM foods ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(names, vec!["Good1".to_string(), "Good2".to_string()]);
    }

    #[test]
    fn materialize_missing_file_is_silent() {
        let conn = open_inmem_with_schema();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nutrition-db.md");
        // file does not exist
        let n = materialize_nutrition_db(&conn, &path, &empty_config()).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn materialize_records_last_synced() {
        let conn = open_inmem_with_schema();
        let dir = TempDir::new().unwrap();
        let path = write_fixture(&dir, "## A\n\n```yaml\nper_100g:\n  kcal: 1\n```\n");
        materialize_nutrition_db(&conn, &path, &empty_config()).unwrap();

        let last: Option<String> = conn
            .query_row(
                "SELECT value FROM sync_meta WHERE key = 'last_nutrition_sync'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert!(last.is_some());
    }

    #[test]
    fn materialize_duplicate_heading_keeps_first() {
        let conn = open_inmem_with_schema();
        let dir = TempDir::new().unwrap();
        let content = "## Foo\n\n```yaml\nper_100g:\n  kcal: 1\n```\n\n## Foo\n\n```yaml\nper_100g:\n  kcal: 999\n```\n";
        let path = write_fixture(&dir, content);
        let n = materialize_nutrition_db(&conn, &path, &empty_config()).unwrap();
        assert_eq!(n, 1);

        let kcal: f64 = conn
            .query_row(
                "SELECT kcal_per_100g FROM foods WHERE name = 'Foo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kcal, 1.0);
    }
}
