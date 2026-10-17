use crate::common::{body, indexed, search_hits, truncated};
use clap::ValueEnum;
use sessidx::{
    model::Agent,
    query::{self, Filters},
};
use std::time::Duration;

#[test]
fn tool_output_prefix_is_bounded_and_raw_tail_remains_reachable() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/output-prefix.json")).unwrap();
    let prefix = format!("outputneedle {}", "x".repeat(2047 - "outputneedle ".len()));
    let text = format!("{prefix}太tailoutside");
    for f in fixture.as_array().unwrap().iter().take(3) {
        let h = Agent::from_str(f["agent"].as_str().unwrap(), false).unwrap();
        let mut record = f["record"].clone();
        let field = match h {
            Agent::Claude => "/message/content/0/content",
            Agent::Codex => "/payload/output/0/text",
            Agent::Pi => "/message/content/0/text",
        };
        *record.pointer_mut(field).unwrap() = serde_json::json!(text);
        let (_dir, store, _) = indexed(h, &(record.to_string() + "\n"));
        let hits = search_hits(&store.db, "outputneedle", &Filters::default(), 20).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].role, "tool");
        assert!(truncated(&hits[0]));
        assert_eq!(body(&hits[0]), prefix);
        assert!(
            search_hits(&store.db, "tailoutside", &Filters::default(), 20)
                .unwrap()
                .is_empty()
        );
        let (raw, c) = query::scan(
            &store.db,
            "tailoutside",
            &Filters {
                agent: vec![h],
                ..Filters::default()
            },
            20,
            None,
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(raw.len(), 1);
        assert!(!c.incomplete && !truncated(&raw[0]));
        assert!(body(&raw[0]).contains("tailoutside"));
        let (shown, c) =
            query::show(&store.db, &format!("{}:1", hits[0].path), 0, 20, None).unwrap();
        assert_eq!(shown.len(), 1);
        assert!(!c.incomplete && !truncated(&shown[0]));
        assert!(body(&shown[0]).contains("tailoutside"));
    }
    let mut s = sessidx::model::State::default();
    let mut record = fixture[0]["record"].clone();
    record["message"]["content"][0]["content"] = serde_json::json!("key=a ".repeat(341));
    let event = sessidx::adapters::parse(Agent::Claude, &record, &mut s).remove(0);
    assert!(event.text.as_ref().unwrap().len() <= 2048);
    assert!(event.text_truncated);
    record["message"]["content"][0]["content"] = serde_json::json!([
        {"type":"text","text":"first"},
        {"type":"text","text":""},
        {"type":"text","text":"second"}
    ]);
    let event = sessidx::adapters::parse(Agent::Claude, &record, &mut s).remove(0);
    assert_eq!(event.text.as_deref(), Some("first\nsecond"));
    assert!(!event.text_truncated);
}

#[test]
fn diagnostic_attachments_are_tool_outputs_without_creating_results() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/output-prefix.json")).unwrap();
    let data = format!("{}\n{}\n", fixture[3]["record"], fixture[4]["record"]);
    let (_dir, store, _) = indexed(Agent::Claude, &data);
    let hits = search_hits(&store.db, "diagnosticneedle", &Filters::default(), 20).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        search_hits(&store.db, "cSpell", &Filters::default(), 20)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(hits[0].role, "tool");
    assert!(!truncated(&hits[0]));
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

#[test]
fn a_value_cut_by_the_prefix_limit_leaves_no_fragment() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/output-prefix.json")).unwrap();
    for (head, canary, into) in [
        (
            "{\"password\":\"",
            "0123456789abcdef1032547698badcfe89abcdef",
            6,
        ),
        (
            "{\"password\":\"",
            "0123456789abcdef1032547698badcfe89abcdef",
            16,
        ),
        (
            "{\"password\":\"",
            "0123456789abcdef1032547698badcfe89abcdef",
            26,
        ),
        ("", "q7Zx9Lm2Pw4Rt8VbN3cK6yH1sD5fG0jA", 8),
    ] {
        let pad = "x".repeat(2048 - 1 - head.len() - into);
        let mut record = fixture[0]["record"].clone();
        record["message"]["content"][0]["content"] =
            serde_json::json!(format!("{pad} {head}{canary}\"}}"));
        let event =
            sessidx::adapters::parse(Agent::Claude, &record, &mut Default::default()).remove(0);
        let text = event.text.unwrap();
        assert!(event.text_truncated);
        assert!(text.starts_with(&pad), "{into}");
        if !head.is_empty() {
            assert!(text.contains(" {\"password\":\"["), "{into}");
        }
        assert!(
            !canary
                .as_bytes()
                .windows(6)
                .any(|w| text.as_bytes().windows(6).any(|t| t == w)),
            "a fragment of the value cut at {into} survived"
        );
    }
}
