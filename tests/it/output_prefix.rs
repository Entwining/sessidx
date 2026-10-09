use crate::common::{indexed, search_hits};
use sessidx::query::{self, Filters};
use std::time::Duration;

#[test]
fn tool_output_prefix_is_bounded_and_raw_tail_remains_reachable() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/output-prefix.json")).unwrap();
    let prefix = format!("outputneedle {}", "x".repeat(2047 - "outputneedle ".len()));
    let text = format!("{prefix}太tailoutside");
    for f in fixture.as_array().unwrap().iter().take(3) {
        let h = f["harness"].as_str().unwrap();
        let mut record = f["record"].clone();
        let field = match h {
            "claude" => "/message/content/0/content",
            "codex" => "/payload/output/0/text",
            _ => "/message/content/0/text",
        };
        *record.pointer_mut(field).unwrap() = serde_json::json!(text);
        let (_dir, store, _) = indexed(h, &(record.to_string() + "\n"));
        let hits = search_hits(&store.db, "outputneedle", &Filters::default(), 20, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].role, "tool");
        assert!(hits[0].truncated);
        assert_eq!(hits[0].snippet, prefix);
        assert!(
            search_hits(&store.db, "tailoutside", &Filters::default(), 20, 0)
                .unwrap()
                .is_empty()
        );
        let (raw, c) = query::scan(
            &store.db,
            "tailoutside",
            &Filters {
                harness: vec![match h {
                    "claude" => sessidx::query::Harness::Claude,
                    "codex" => sessidx::query::Harness::Codex,
                    _ => sessidx::query::Harness::Pi,
                }],
                ..Filters::default()
            },
            20,
            0,
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(raw.len(), 1);
        assert!(!c.incomplete && !raw[0].truncated);
        assert!(raw[0].snippet.contains("tailoutside"));
        let (shown, c) = query::show(&store.db, &format!("{}:1", hits[0].path), 0, 20, 0).unwrap();
        assert_eq!(shown.len(), 1);
        assert!(!c.incomplete && !shown[0].truncated);
        assert!(shown[0].snippet.contains("tailoutside"));
    }
    let mut s = sessidx::model::State::default();
    let mut record = fixture[0]["record"].clone();
    record["message"]["content"][0]["content"] = serde_json::json!("key=a ".repeat(341));
    let event = sessidx::adapters::parse("claude", &record, &mut s)
        .events
        .remove(0);
    assert!(event.text.as_ref().unwrap().len() <= 2048);
    assert!(event.text_truncated);
    record["message"]["content"][0]["content"] = serde_json::json!([
        {"type":"text","text":"first"},
        {"type":"text","text":""},
        {"type":"text","text":"second"}
    ]);
    let event = sessidx::adapters::parse("claude", &record, &mut s)
        .events
        .remove(0);
    assert_eq!(event.text.as_deref(), Some("first\nsecond"));
    assert!(!event.text_truncated);
}

#[test]
fn diagnostic_attachments_are_tool_outputs_without_creating_results() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/output-prefix.json")).unwrap();
    let data = format!("{}\n{}\n", fixture[3]["record"], fixture[4]["record"]);
    let (_dir, store, _) = indexed("claude", &data);
    let hits = search_hits(&store.db, "diagnosticneedle", &Filters::default(), 20, 0).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        search_hits(&store.db, "cSpell", &Filters::default(), 20, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(hits[0].role, "tool");
    assert!(!hits[0].truncated);
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM events WHERE kind='tool_result'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM events WHERE kind='context'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}
