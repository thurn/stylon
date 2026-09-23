use std::fs;
use std::path::Path;

use tempfile::tempdir;

use crate::config::Config;
use crate::qualification;
use tempfile::TempDir;

fn config() -> (TempDir, Config) {
    let directory = tempdir().expect("temporary directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[workspace]\nmembers=[]\n",
    )
    .expect("manifest");
    let config = Config::load(directory.path(), None).expect("configuration");
    (directory, config)
}

#[test]
fn shortens_type_variant_and_function_paths_with_shared_imports() {
    let (_directory, config) = config();
    let source =
        "fn f(value: cards::Card) { workflow::runner::run(); let _ = ui::Value::Ready; }\n";
    let findings = qualification::check(&config, Path::new("src/lib.rs"), source);
    assert_eq!(findings.len(), 3);
    let mut fixed = source.to_owned();
    let mut edits = findings[0].edits.clone();
    edits.sort_by_key(|edit| edit.range.start);
    for edit in edits.into_iter().rev() {
        fixed.replace_range(edit.range, &edit.replacement);
    }
    assert!(fixed.contains("use cards::Card;"));
    assert!(fixed.contains("use ui::Value;"));
    assert!(fixed.contains("use workflow::runner;"));
    assert!(fixed.contains("fn f(value: Card)"));
    assert!(fixed.contains("runner::run()"));
    assert!(fixed.contains("Value::Ready"));
}

#[test]
fn groups_inserted_imports_by_shared_prefix() {
    let (_directory, config) = config();
    let source = "fn view(group: reactant::world::Group, sprite: reactant::world::Sprite) {}\n";
    let findings = qualification::check(&config, Path::new("src/lib.rs"), source);
    let mut fixed = source.to_owned();
    let mut edits = findings[0].edits.clone();
    edits.sort_by_key(|edit| edit.range.start);
    for edit in edits.into_iter().rev() {
        fixed.replace_range(edit.range, &edit.replacement);
    }

    assert!(fixed.contains("use reactant::world::{Group, Sprite};"));
    assert!(!fixed.contains("use reactant::world::Group;"));
    assert!(!fixed.contains("use reactant::world::Sprite;"));
}

#[test]
fn rewrites_relative_imports_from_the_file_module() {
    let (_directory, config) = config();
    let source = "use super::card::Card;\n";
    let findings = qualification::check(&config, Path::new("src/game/deck.rs"), source);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].edits[0].replacement, "crate::game::card::Card;");
}

#[test]
fn exempts_self_associated_types_and_standard_module_aliases() {
    let (_directory, config) = config();
    let source = "use std::{fmt, io};\n\nfn result() -> fmt::Result { todo!() }\nfn associated() -> Self::Output { todo!() }\nfn generic<S>() -> Result<S::Ok, S::Error> { todo!() }\nfn error() -> io::Error { todo!() }\n";

    let findings = qualification::check(&config, Path::new("src/lib.rs"), source);

    assert!(findings.is_empty());
}

#[test]
fn resolves_super_from_an_extracted_test_module() {
    let (_directory, config) = config();
    let source = "use super::helper;\n";

    let findings = qualification::check(&config, Path::new("src/parser_tests.rs"), source);

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].edits[0].replacement, "crate::parser::helper;");
}

#[test]
fn keeps_a_type_qualified_when_its_leaf_is_already_used() {
    let (_directory, config) = config();
    let source = "fn example(value: Box<u8>) { external::host::Box::new(); drop(value); }\n";

    let findings = qualification::check(&config, Path::new("src/lib.rs"), source);

    assert!(findings.is_empty());
}

#[test]
fn checks_local_roots_and_imported_local_module_aliases() {
    let (_directory, config) = config();
    for root in ["crate", "self", "super", "super::super"] {
        let source = format!(
            "fn example(value: {root}::model::Opponent) {{ {root}::model::Opponent::new(); let _ = {root}::model::State::Ready; {root}::runner::run(); }}\n"
        );
        let findings = qualification::check(&config, Path::new("src/game/app.rs"), &source);
        assert_eq!(findings.len(), 4, "{root}");
        let fixed = apply_findings(&source, &findings);
        assert!(fixed.contains("value: Opponent"));
        assert!(fixed.contains("Opponent::new()"));
        assert!(fixed.contains("State::Ready"));
        assert!(fixed.contains("runner::run()"));
        assert!(!fixed.contains(&format!("value: {root}::")));
    }
    let source =
        "use crate::model as model_alias;\n\nfn example(value: model_alias::Opponent) {}\n";
    let findings = qualification::check(&config, Path::new("src/app.rs"), source);
    assert_eq!(findings.len(), 1);
    let fixed = apply_findings(source, &findings);
    assert!(fixed.contains("use crate::model::Opponent;"));
    assert!(fixed.contains("value: Opponent"));
}

#[test]
fn local_paths_keep_collisions_and_single_qualifier_calls() {
    let (_directory, config) = config();
    for source in [
        "struct Opponent;\n\nfn example(value: crate::other::Opponent) {}\n",
        "use crate::first::Opponent;\n\nfn example(value: crate::other::Opponent) {}\n",
        "fn example() { self::helper(); super::helper(); Self::new(); }\n",
        "macro_rules! factory { () => { crate::model::Opponent::new() }; }\n",
    ] {
        assert!(
            qualification::check(&config, Path::new("src/app.rs"), source).is_empty(),
            "{source}"
        );
    }
}

#[test]
fn local_paths_obey_disabled_rules() {
    let (directory, _) = config();
    fs::write(directory.path().join("stylon.toml"), "version = 1\n[rules]\n\"path.type-qualification\" = false\n\"path.function-qualification\" = false\n\"path.enum-variant-qualification\" = false\n").expect("configuration");
    let config = Config::load(directory.path(), None).expect("configuration");
    let source = "fn example(value: crate::model::Opponent) { crate::model::Opponent::new(); let _ = crate::model::State::Ready; crate::runner::run(); }\n";
    assert!(qualification::check(&config, Path::new("src/app.rs"), source).is_empty());
}

fn apply_findings(source: &str, findings: &[crate::rules::Finding]) -> String {
    let mut edits: Vec<_> = findings
        .iter()
        .flat_map(|finding| finding.edits.clone())
        .collect();
    edits.sort_by_key(|edit| edit.range.start);
    let mut fixed = source.to_owned();
    for edit in edits.into_iter().rev() {
        fixed.replace_range(edit.range, &edit.replacement);
    }
    fixed
}

#[test]
fn reuses_existing_grouped_imports_for_local_paths() {
    let (_directory, config) = config();
    let source = "use crate::model::{Opponent, State};\n\nfn example(value: crate::model::Opponent) { crate::model::Opponent::new(); let _ = crate::model::State::Ready; }\n";
    let findings = qualification::check(&config, Path::new("src/app.rs"), source);
    assert_eq!(findings.len(), 3);
    let fixed = apply_findings(source, &findings);
    assert_eq!(fixed.matches("use ").count(), 1);
    assert!(fixed.contains("value: Opponent"));
    assert!(fixed.contains("Opponent::new()"));
    assert!(fixed.contains("State::Ready"));
    assert!(qualification::check(&config, Path::new("src/app.rs"), &fixed).is_empty());
}

#[test]
fn self_imports_resolve_in_the_enclosing_inline_module() {
    let (_directory, config) = config();
    let source = "use crate::outer as model;\n\nmod child {\n    use crate::inner as model;\n\n    fn example() { self::model::Opponent::new(); }\n}\n";
    let findings = qualification::check(&config, Path::new("src/app.rs"), source);
    assert_eq!(findings.len(), 1);
    let fixed = apply_findings(source, &findings);
    assert!(fixed.contains("    use crate::inner::Opponent;"));
    assert!(!fixed.contains("use crate::outer::Opponent;"));
}
