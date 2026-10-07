use sessidx::{adapters, model::State};

#[test]
fn inventory_variants_are_classified_with_future_shape_negative_control() {
    let variants: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/inventory.json")).unwrap();
    let mut known = 0;
    let mut unknown = 0;
    let mut mismatches = Vec::new();
    for variant in variants.as_array().unwrap() {
        let r = adapters::parse(
            variant["harness"].as_str().unwrap(),
            &variant["record"],
            &mut State::default(),
        );
        if r.known != variant["known"].as_bool().unwrap() {
            mismatches.push(format!("{} {}", variant["harness"], variant["shape"]));
        }
        if variant["scope"] == "block" {
            let shape = variant["shape"].as_str().unwrap();
            let kind = if shape.ends_with("server_tool_use") {
                Some("server_tool_call")
            } else if shape.ends_with("advisor_tool_result") {
                Some("server_tool_result")
            } else if shape.ends_with("tool_use") || shape.ends_with("toolCall") {
                Some("tool_call")
            } else if shape.ends_with("tool_result") {
                Some("tool_result")
            } else {
                Some("message")
            };
            if let Some(kind) = kind {
                assert_eq!(
                    r.events.iter().filter(|e| e.kind == kind).count(),
                    1,
                    "{shape}"
                );
            }
            if shape.ends_with("/ text") {
                assert!(
                    r.events
                        .iter()
                        .any(|e| e.kind == "message" && e.text.as_deref() == Some("synthetic-text")),
                    "{shape}"
                );
            }
        }
        known += usize::from(r.known);
        unknown += usize::from(!r.known);
    }
    assert_eq!(known, 79, "{mismatches:?}");
    assert_eq!(unknown, 1);
    assert!(mismatches.is_empty(), "{mismatches:?}");
    let future =
        serde_json::json!({"type":"unknown-future-shape","message":{"content":"negative control"}});
    for h in ["claude", "codex", "pi"] {
        assert!(!adapters::parse(h, &future, &mut State::default()).known);
    }
}

#[test]
fn formats_rows_name_existing_fixtures_and_tests() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let formats = std::fs::read_to_string(root.join("docs/formats.md")).unwrap();
    let test_fn = |file: &str, name: &str| {
        std::fs::read_to_string(root.join(file))
            .unwrap()
            .contains(&format!("fn {name}("))
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
    let files: Vec<_> = std::fs::read_dir(root.join("tests"))
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
