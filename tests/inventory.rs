use sessidx::{adapters, model::State};

#[test]
fn inventory_variants_are_classified_with_future_shape_negative_control() {
    let variants: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/inventory.json")).unwrap();
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
                None
            };
            if let Some(kind) = kind {
                assert_eq!(
                    r.events.iter().filter(|e| e.kind == kind).count(),
                    1,
                    "{shape}"
                );
            }
        }
        known += usize::from(r.known);
        unknown += usize::from(!r.known);
    }
    assert_eq!(known, 79, "{mismatches:?}");
    assert_eq!(unknown, 1);
    let future =
        serde_json::json!({"type":"unknown-future-shape","message":{"content":"negative control"}});
    for h in ["claude", "codex", "pi"] {
        assert!(!adapters::parse(h, &future, &mut State::default()).known);
    }
}
