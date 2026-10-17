use sessidx::{
    adapters,
    model::{Agent, State},
    shell,
};

#[test]
fn suffix_assignments_stay_in_argv_and_prefix_assignments_do_not() {
    for (command, expected) in [
        ("make CC=clang all", vec!["make", "CC=clang", "all"]),
        (
            "gh api repos/o/r/issues -f title=hello -F draft=true",
            vec![
                "gh",
                "api",
                "repos/o/r/issues",
                "-f",
                "title=hello",
                "-F",
                "draft=true",
            ],
        ),
        ("FOO=1 env", vec!["env"]),
    ] {
        let events = adapters::parse(
            Agent::Claude,
            &serde_json::json!({"type":"assistant","uuid":"assign","sessionId":"claude-assign","message":{"role":"assistant","content":[{"type":"tool_use","id":"call-assign","name":"Bash","input":{"command":command}}]}}),
            &mut State::default(),
        );
        let sites: Vec<_> = events.iter().flat_map(|e| &e.sites).collect();
        assert_eq!(sites.len(), 1, "{command}");
        assert_eq!(sites[0].argv, expected, "{command}");
    }
}

#[test]
fn brush_sites_include_nested_syntax_without_counting_quoted_program_names() {
    let sites = shell::sites(
        "rg one src | cat; if test -d src; then rg two src; fi; echo \"$(rg three src)\"; printf 'rg is an argument'; f() { rg four src; }; cat <(rg five src); while read l; do echo \"$l\"; done < <(rg six src); cat <<< \"$(rg seven src)\"; cat <<EOF\n$(rg eight src)\nEOF\n",
    );
    assert_eq!(
        sites
            .iter()
            .filter(|s| s.program.as_deref() == Some("rg"))
            .count(),
        8
    );
    assert!(sites.iter().all(|s| s.parsed));
    assert_eq!(
        shell::sites("printf 'rg quoted'; cat <<'EOF'\n$(rg quoted)\nEOF\n")
            .iter()
            .filter(|s| s.program.as_deref() == Some("rg"))
            .count(),
        0
    );
    let unparsed = shell::sites("echo 'unterminated");
    assert_eq!(unparsed.len(), 1);
    assert!(!unparsed[0].parsed);
}

#[test]
fn codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/code-mode.json")).unwrap();
    // Longer than any fixture literal: size caps must not make ordinary commands opaque.
    let long = serde_json::json!({"name":"long","source":format!("tools.exec_command({{cmd: \"rg {}\"}});", "a".repeat(2000)),"programs":["rg"],"unparsed":0});
    for f in fixtures.as_array().unwrap().iter().chain([&long]) {
        let events = adapters::parse(
            Agent::Codex,
            &serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"wrapper","input":f["source"]}}),
            &mut State::default(),
        );
        let sites = &events[0].sites;
        let mut programs: Vec<_> = sites.iter().filter_map(|s| s.program.as_deref()).collect();
        let mut expected: Vec<_> = f["programs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        programs.sort();
        expected.sort();
        assert_eq!(programs, expected, "{}", f["name"]);
        assert_eq!(
            sites.iter().filter(|s| !s.parsed).count(),
            f["unparsed"].as_u64().unwrap() as usize,
            "{}",
            f["name"]
        );
        if let Some(argv) = f["argv"].as_array() {
            let expected: Vec<Vec<String>> =
                serde_json::from_value(serde_json::Value::Array(argv.clone())).unwrap();
            assert_eq!(
                sites.iter().map(|s| s.argv.clone()).collect::<Vec<_>>(),
                expected,
                "{}",
                f["name"]
            );
        }
    }
}
