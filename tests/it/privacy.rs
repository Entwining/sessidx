use crate::common::{indexed, search_hits, serial, sessidx};
use sessidx::query::{self, Filters};
use std::{fs, time::Duration};

fn assert_canaries_absent_from_storage(dir: &std::path::Path, values: &[&str]) {
    for name in ["index.db", "index.db-wal", "index.db-shm"] {
        let bytes = match fs::read(dir.join(name)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && name != "index.db" => continue,
            Err(e) => panic!("cannot check {name}: {e}"),
        };
        for value in values {
            assert!(
                !bytes.windows(value.len()).any(|b| b == value.as_bytes()),
                "canary leaked to {name}"
            );
        }
    }
}

fn canaries() -> Vec<String> {
    (0..6)
        .map(|n| {
            if n % 2 == 0 {
                format!("syntheticCanaryToken{n}ZaQwSxEdCvRfTgBhYj")
            } else {
                format!("zQ8vN2rK7xP4mT9aF6wH3cS5uD1jL0eB_yGqR{o}", o = n)
            }
        })
        .collect()
}

#[test]
fn synthetic_secret_canaries_absent_from_storage_and_lookup_outputs() {
    let serial = serial();
    let mut values = canaries();
    let line = |id: &str, typ: &str, value: serde_json::Value| {
        serde_json::json!({"type":typ,"uuid":id,"sessionId":"canary-session","cwd":"/synthetic","message":value}).to_string()+"\n"
    };
    let mut data = String::new();
    for (i, value) in values.iter().enumerate() {
        let text = if i % 2 == 0 {
            format!("Bearer {value}")
        } else {
            value.clone()
        };
        let m = match i / 2 {
            0 => serde_json::json!({"role":"user","content":format!("needle {text}")}),
            1 => {
                serde_json::json!({"role":"assistant","model":"model","content":[{"type":"tool_use","id":format!("call-{i}"),"name":"Bash","input":{"command":format!("printf 'needle {text}'")}}]})
            }
            _ => {
                serde_json::json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call-2","is_error":false,"content":format!("needle {text}")}]})
            }
        };
        data += &line(
            &format!("event-{i}"),
            if i / 2 == 1 { "assistant" } else { "user" },
            m,
        );
    }
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/credential-context.json")).unwrap();
    for (i, f) in fixture["labels"].as_array().unwrap().iter().enumerate() {
        let label = f["label"].as_str().unwrap();
        let value = f["value"].as_str().unwrap();
        values.push(value.into());
        let mut input = serde_json::json!({"command":format!("printf 'needle {label}={value}'")});
        input
            .as_object_mut()
            .unwrap()
            .insert(label.into(), value.into());
        for (kind, message) in [
            (
                "user",
                serde_json::json!({"role":"user","content":format!("needle {label}: {value}")}),
            ),
            (
                "assistant",
                serde_json::json!({"role":"assistant","content":[{"type":"tool_use","id":format!("label-{i}"),"name":"Bash","input":input}]}),
            ),
            (
                "user",
                serde_json::json!({"role":"user","content":[{"type":"tool_result","tool_use_id":format!("label-{i}"),"is_error":false,"content":format!("needle {label}={value}")}]}),
            ),
        ] {
            data += &line(&format!("label-{i}-{kind}"), kind, message);
        }
    }
    for key in ["symbol", "slash", "url_path", "url_query", "uuid_layout"] {
        let value = fixture[key].as_str().unwrap();
        values.push(value.into());
        let carrier = match key {
            "url_path" => format!("https://hooks.slack.com/services/T123/B456/{value}"),
            "url_query" => format!("https://a.io/?ref={value}"),
            _ => value.to_owned(),
        };
        data += &line(
            key,
            "user",
            serde_json::json!({"role":"user","content":format!("needle my password is {carrier} ok")}),
        );
        data += &line(
            &format!("{key}-input"),
            "assistant",
            serde_json::json!({"role":"assistant","content":[{"type":"tool_use","id":key,"name":"Bash","input":{"command":format!("printf 'needle {carrier}'")}}]}),
        );
        data += &line(
            &format!("{key}-result"),
            "user",
            serde_json::json!({"role":"user","content":[{"type":"tool_result","tool_use_id":key,"is_error":false,"content":format!("needle {carrier}")}]}),
        );
    }
    data += &line(
        "public-identifiers",
        "user",
        serde_json::json!({"role":"user","content":format!("needle {}",fixture["identifiers"].as_array().unwrap().iter().map(|v|v.as_str().unwrap()).collect::<Vec<_>>().join(" "))}),
    );
    let (dir, store, roots) = indexed("claude", &data);
    assert_canaries_absent_from_storage(
        dir.path(),
        &values.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    for v in fixture["identifiers"].as_array().unwrap() {
        let value = v.as_str().unwrap();
        let hits = search_hits(&store.db, value, &Filters::default(), 20, 0).unwrap();
        assert!(
            hits.iter().any(|h| h.snippet.contains(value)),
            "public identifier lost"
        );
    }
    let mut outputs = Vec::new();
    for args in [
        vec!["index"],
        vec!["search", "needle"],
        vec![
            "grep",
            "needle",
            "--session",
            "canary-session",
            "--limit",
            "1000",
        ],
        vec!["show", "canary-session", "--limit", "1000"],
        vec!["grep", "needle"],
        vec!["count", "commands"],
        vec!["count", "failures"],
        vec!["count", "denials"],
        vec!["doctor"],
        vec!["sql", "SELECT * FROM events"],
        vec!["sql", "SELECT * FROM commands"],
        vec!["sql", "SELECT * FROM shapes"],
    ] {
        let output = sessidx(&serial, &store.path, &roots, &args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if args == ["grep", "needle"] { 2 } else { 0 }),
            "{args:?}"
        );
        outputs.extend_from_slice(&output.stdout);
        outputs.extend_from_slice(&output.stderr);
    }
    for v in &values {
        assert!(
            !outputs.windows(v.len()).any(|b| b == v.as_bytes()),
            "canary leaked to command output"
        );
    }
    assert_canaries_absent_from_storage(
        dir.path(),
        &values.iter().map(String::as_str).collect::<Vec<_>>(),
    );
}

#[test]
fn adversarial_hex_fragments_basic_and_passphrase_canaries_are_removed() {
    let values: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/privacy.json")).unwrap();
    let text = format!(
        "needle {} token={} Authorization: Basic {} password = \"{}\" https://example.invalid/?token={} https://example.invalid/?page=2&key={} https://example.invalid/?access_token={}",
        values["unmarked"].as_str().unwrap(),
        values["hex"].as_str().unwrap(),
        values["basic"].as_str().unwrap(),
        values["passphrase"].as_str().unwrap(),
        values["query_short"].as_str().unwrap(),
        values["query_short"].as_str().unwrap(),
        values["query_short"].as_str().unwrap()
    );
    let data=serde_json::json!({"type":"user","uuid":"privacy-user","sessionId":"privacy","message":{"role":"user","content":text}}).to_string()+"\n"+&serde_json::json!({"type":"assistant","uuid":"privacy-input","sessionId":"privacy","message":{"role":"assistant","content":[{"type":"tool_use","id":"privacy-call","name":"Bash","input":{"command":"printf needle","part_one":values["part_one"],"part_two":values["part_two"]}}]}}).to_string()+"\n";
    let (dir, store, _) = indexed("claude", &data);
    let output = query::show(&store.db, "privacy", 0, 20, 0)
        .unwrap()
        .0
        .into_iter()
        .map(|h| h.snippet)
        .collect::<Vec<_>>()
        .join("\n");
    for value in values
        .as_object()
        .unwrap()
        .values()
        .filter_map(|v| v.as_str())
    {
        assert!(
            !output.contains(value),
            "adversarial canary leaked on display"
        );
        assert_canaries_absent_from_storage(dir.path(), &[value]);
    }
}

#[test]
fn body_identifiers_are_searchable_and_credential_context_stays_redacted() {
    let (_, store, _) = indexed("claude", include_str!("../fixtures/identifiers.jsonl"));
    for value in [
        "0123456789abcdef1032547698badcfe89abcdef",
        "0xabcdef0123456789abcdef0123456789abcdef01",
        "01234567-89ab-cdef-0123-456789abcdef",
        "/synthetic/long-project-directory/source.rs",
        "tools.exec_command",
        "os.path.join",
        "std::fs::File::try_lock",
        "a.b.c_d2",
        "_module_42::Foo_Bar1/Item99",
        "tools.exec_command:",
        "std::fs::File::try_lock.",
        "--max-output-tokens",
        "--dangerously-skip-permissions",
        "/path/to/session.jsonl:42",
        "/path/to/session.jsonl:42:7",
        "src/redaction.rs:83",
        "a/b.rs:12:5",
        "crates/core/src/main.rs:107",
    ] {
        let hits = search_hits(
            &store.db,
            &format!("identifierneedle {value}"),
            &Filters::default(),
            20,
            0,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains(value));
    }
    let context = search_hits(&store.db, "contextneedle", &Filters::default(), 20, 0).unwrap();
    assert_eq!(context.len(), 1);
    for value in [
        "0123456789abcdef1032547698badcfe89abcdef",
        "0xabcdef0123456789abcdef0123456789abcdef01",
        "01234567-89ab-cdef-0123-456789abcdef",
        "vR9xT6qA2nL8cP4hY0sD7fG3jK5mB1wZ",
        "tools.exec_command",
        "std::fs::File::try_lock",
        "ghp_AlphabeticSyntheticToken",
        "_module_42::Foo_Bar1/Item99",
        "module.name:invalidColon9",
        "module..missing_segment9",
        "a1b2.c3d4_e5F6.g7H8",
        "--invalid--flagSyntaxExtra",
        "/path/to/session.jsonl:42:7:3",
        "crates/core/src/main.rs:107:4:8",
        "src/redaction.rs:83",
        "vR9xT6qA2nL8cP4hY0sD7fG3jK5mB1wZ:83",
        "vR9xT6qA2nL8cP4hY0sD7fG3jK5mB1wZ:12:5",
        "order:9182736455647382",
        "aBcDeFgHiJkLmNoP:42",
    ] {
        assert!(
            !context[0].snippet.contains(value),
            "synthetic context control survived: {value}"
        );
    }
    let input = search_hits(&store.db, "identifierinput", &Filters::default(), 20, 0).unwrap();
    assert_eq!(input.len(), 1);
    assert!(input[0].snippet.contains("tools.exec_command"));
    assert!(!input[0].snippet.contains("std::fs::File::try_lock"));
    let stored: String = store
        .db
        .query_row("SELECT text FROM events WHERE kind='tool_call'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let args: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(
        args["namespace_members"],
        "01234567-89ab-cdef-0123-456789abcdef"
    );
    assert_eq!(
        args["schema_properties"],
        "0123456789abcdef1032547698badcfe89abcdef"
    );
    assert!(
        search_hits(
            &store.db,
            "identifierinput 0xabcdef0123456789abcdef0123456789abcdef01",
            &Filters::default(),
            20,
            0
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        input[0]
            .snippet
            .contains("0123456789abcdef1032547698badcfe89abcdef")
    );
    assert!(
        !input[0]
            .snippet
            .contains("0xabcdef0123456789abcdef0123456789abcdef01")
    );
    assert!(
        !input[0]
            .snippet
            .contains("vR9xT6qA2nL8cP4hY0sD7fG3jK5mB1wZ")
    );
}

#[test]
fn pi_sections_share_the_native_message_and_are_redacted() {
    let (dir, store, _) = indexed("pi", include_str!("../fixtures/pi-sections.jsonl"));
    for term in ["contentneedle", "sectionneedle", "TypeSafe", "sectiontail"] {
        let hits = search_hits(&store.db, term, &Filters::default(), 20, 0).unwrap();
        assert_eq!(hits.len(), 1, "{term}");
        assert_eq!(hits[0].role, "system");
    }
    let messages: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM canonical_events WHERE kind='message'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(messages, 1);
    assert_canaries_absent_from_storage(dir.path(), &["Q8vN2rK7xP4mT9aF6wH3cS5uD1jL0eB"]);
}

#[test]
fn scan_matches_original_ranges_before_redacting_display() {
    let token = "zQ8vN2rK7xP4mT9aF6wH3cS5uD1jL0eB_yGqR1";
    let data=serde_json::json!({"type":"user","uuid":"scan-before-redaction","sessionId":"scan-private","message":{"role":"user","content":format!("needle {token}")}}).to_string()+"\n";
    let (_dir, store, _) = indexed("claude", &data);
    let (hits, c) = query::scan(
        &store.db,
        token,
        &Filters {
            session: Some("scan-private".into()),
            ..Filters::default()
        },
        20,
        0,
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(!hits[0].snippet.contains(token));
    assert!(!c.incomplete);
}

#[test]
fn native_identifiers_and_cwd_remain_queryable_with_body_entropy_redaction() {
    let session = "rollout-2026-05-01T10-30-00-syntheticId7QwX9rTbM3k";
    let cwd = "/synthetic/Code/GitHub/project-with-native-identifiers";
    let data=serde_json::json!({"type":"session_meta","payload":{"id":session,"cwd":cwd}}).to_string()+"\n"+&serde_json::json!({"type":"turn_context","payload":{"model":"claude-haiku-4-5-20251001"}}).to_string()+"\n"+&serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"needle"}]}}).to_string()+"\n";
    let (_, store, _) = indexed("codex", &data);
    let hits = search_hits(
        &store.db,
        "needle",
        &Filters {
            session: Some(session.into()),
            cwd: Some(cwd.into()),
            ..Filters::default()
        },
        20,
        0,
    )
    .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].model.as_deref(), Some("claude-haiku-4-5-20251001"));
}
