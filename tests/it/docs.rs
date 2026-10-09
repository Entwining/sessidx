use crate::common::{serial, sessidx, shell};
use std::path::Path;

#[test]
fn readme_examples_print_their_shown_output_for_the_readme_fixture() {
    let serial = serial();
    let readme = include_str!("../../README.md");
    let home = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/readme");
    // The README's commands use the default roots and database, so the scratch
    // HOME links those roots to the fixture and the pipelines run verbatim.
    for (root, dir) in [
        (".claude/projects", "claude"),
        (".codex/sessions", "codex"),
        (".pi/agent/sessions", "pi"),
    ] {
        let link = home.path().join(root);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(fixture.join(dir), link).unwrap();
    }
    let bin = Path::new(env!("CARGO_BIN_EXE_sessidx")).parent().unwrap();
    let mut examples: Vec<(&str, String)> = Vec::new();
    let mut in_console = false;
    for line in readme.lines() {
        if line.starts_with("```") {
            in_console = line == "```console";
        } else if let (true, Some(command)) = (in_console, line.strip_prefix("$ ")) {
            examples.push((command, String::new()));
        } else if in_console {
            let shown = &mut examples.last_mut().unwrap().1;
            shown.push_str(line);
            shown.push('\n');
        }
    }
    assert_eq!(examples.len(), 2, "README console examples not found");
    for (command, shown) in examples {
        let out = shell(&serial, command)
            .env("HOME", home.path())
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(out.status.success(), "{command}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), shown, "{command}");
    }
}

#[test]
fn skill_names_only_commands_and_options_the_cli_accepts() {
    let serial = serial();
    let skill = include_str!("../../skills/sessidx/SKILL.md");
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.db");
    let mut help = String::new();
    let verbs = regex::Regex::new(r"`sessidx ([a-z]+)").unwrap();
    for verb in verbs.captures_iter(skill).map(|c| c[1].to_owned()) {
        let out = sessidx(&serial, &db, &[], &[&verb, "--help"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "sessidx {verb} --help");
        help.push_str(&String::from_utf8_lossy(&out.stdout));
    }
    let options = regex::Regex::new(r"--[a-z][a-z-]*").unwrap();
    for option in options.find_iter(skill).map(|m| m.as_str()) {
        let listed = regex::Regex::new(&format!("{}(?:[^a-z-]|$)", regex::escape(option))).unwrap();
        assert!(listed.is_match(&help), "{option} is not a sessidx option");
    }
}

#[test]
fn formats_rows_name_existing_fixtures_and_tests() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let formats = std::fs::read_to_string(root.join("docs/formats.md")).unwrap();
    let test_fn = |file: &str, name: &str| {
        std::fs::read_to_string(root.join(file))
            .unwrap()
            .contains(&format!("#[test]\nfn {name}("))
    };
    let mut tests = Vec::new();
    let mut in_rules = false;
    for line in formats.lines() {
        if line == "| Rule | Fixture | Reason | Regression test |" {
            in_rules = true;
        } else if !line.starts_with('|') {
            in_rules = false;
        } else if in_rules && !line.starts_with("| ---") {
            let cell = line.trim_end_matches('|').rsplit(" | ").next().unwrap();
            tests.push(cell.trim().trim_matches('`').to_owned());
        }
    }
    assert!(tests.len() > 100, "rule tables not found");
    let files: Vec<_> = std::fs::read_dir(root.join("tests/it"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
        .collect();
    for name in &tests {
        assert!(
            files.iter().any(|f| test_fn(f, name)),
            "formats.md names missing test {name}"
        );
    }
    let references = regex::Regex::new(r"`(tests/[^`\s:\[]+)(?:::([a-z0-9_]+))?").unwrap();
    for c in references.captures_iter(&formats) {
        assert!(
            root.join(&c[1]).exists(),
            "formats.md names missing file {}",
            &c[1]
        );
        if let Some(name) = c.get(2) {
            assert!(
                test_fn(&c[1], name.as_str()),
                "formats.md names missing test {}::{}",
                &c[1],
                name.as_str()
            );
        }
    }
}
