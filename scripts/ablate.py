#!/usr/bin/env python3
"""Temporarily remove normalization rules; restore exact source bytes after each run."""
import json
import re
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
rows = [
    ('claude_message', 'src/adapters/claude.rs', 'if typ == "assistant" || has_text || typ == "user" && !only_results {', 'if false {', 'native_message_fragments_empty_replies_summary_and_history_dedup', 'codex_context_arguments_and_telemetry'),
    ('claude_model', 'src/adapters/claude.rs', '.filter(|m| m != "<synthetic>")', '', 'inherited_models_change_retrospective_group_counts', 'pi_model_tool_call_and_camel_case_flag'),
    ('claude_flag', 'src/adapters/claude.rs', 'e.ok = b.get("is_error").and_then(Value::as_bool).map(|v| !v);', '', 'claude_blocks_flags_and_synthetic_model', 'codex_context_arguments_and_telemetry'),
    ('claude_fragment_id', 'src/adapters/claude.rs', 'string(m, "id").map(|id| format!("message:{id}"))', 'None::<String>', 'native_message_fragments_empty_replies_summary_and_history_dedup', 'pi_model_tool_call_and_camel_case_flag'),
    ('claude_compaction', 'src/adapters/claude.rs', 'if summary {', 'if false {', 'native_message_fragments_empty_replies_summary_and_history_dedup', 'pi_model_tool_call_and_camel_case_flag'),
    ('claude_sidechain', 'src/adapters/claude.rs', 's.kind = "delegated".into();', '', 'attribution_native_denials_instructions_children_and_retry_negative_control', 'codex_context_arguments_and_telemetry'),
    ('claude_classifier', 'src/adapters/claude.rs', '"classifier" | "automode-blocked" => "classifier",', '"classifier" => "classifier",', 'attribution_native_denials_instructions_children_and_retry_negative_control', 'codex_context_arguments_and_telemetry'),
    ('codex_model', 'src/adapters/codex.rs', 's.model = Some(m);', '', 'inherited_models_change_retrospective_group_counts', 'pi_model_tool_call_and_camel_case_flag'),
    ('json_arguments', 'src/normalize.rs', 'serde_json::from_str(s).unwrap_or_else(|_| v.clone())', 'v.clone()', 'codex_context_arguments_and_telemetry', 'pi_model_tool_call_and_camel_case_flag'),
    ('codex_base_instructions', 'src/adapters/codex.rs', 's.instruction_hash = Some(hash(content.as_bytes()));', '', 'codex_context_arguments_and_telemetry', 'pi_model_tool_call_and_camel_case_flag'),
    ('codex_injected_instructions', 'src/adapters/codex.rs', 's.instruction_hash = e.text.as_deref().map(|t| hash(t.as_bytes()));', '', 'attribution_native_denials_instructions_children_and_retry_negative_control', 'pi_model_tool_call_and_camel_case_flag'),
    ('codex_text_outcome', 'src/adapters/codex.rs', 'crate::outcomes::classify(&mut e, &p["output"], "codex");', '', 'codex_text_array_batch_failures_and_quoted_negative_control', 'claude_blocks_flags_and_synthetic_model'),
    ('batch_rejection', 'src/outcomes.rs', 'if status == "rejected" {', 'if status == "removed-rejection" {', 'codex_text_array_batch_failures_and_quoted_negative_control', 'claude_blocks_flags_and_synthetic_model'),
    ('result_container', 'src/outcomes.rs', '["content", "results", "items", "result"]', '["content", "results", "items"]', 'codex_text_array_batch_failures_and_quoted_negative_control', 'claude_blocks_flags_and_synthetic_model'),
    ('mixed_outcome_failure', 'src/outcomes.rs', 'if failed || e.ok_source == "none" && !codes.is_empty() {', 'if e.ok_source == "none" && (failed || !codes.is_empty()) {', 'codex_text_array_batch_failures_and_quoted_negative_control', 'claude_blocks_flags_and_synthetic_model'),
    ('native_hook_flag', 'src/outcomes.rs', 'if harness != "codex" && e.ok == Some(true) {', 'if false {', 'claude_hook_check_errors_respect_native_success_flags', 'codex_text_array_batch_failures_and_quoted_negative_control'),
    ('hook_check_error', 'src/outcomes.rs', 'line.contains("hook error:")', 'false', 'claude_hook_check_errors_respect_native_success_flags', 'codex_text_array_batch_failures_and_quoted_negative_control'),
    ('hook_prefix', 'src/outcomes.rs', 's.trim_start().strip_prefix("Script error:")', 'None::<&str>', 'codex_text_array_batch_failures_and_quoted_negative_control', 'claude_blocks_flags_and_synthetic_model'),
    ('newline_hook', 'src/outcomes.rs', 'payload\n            .trim_start()', "payload\n            .trim_start_matches(' ')", 'split_script_error_and_truncated_batch_are_denials_with_transport_quote_control', 'claude_blocks_flags_and_synthetic_model'),
    ('pi_model', 'src/adapters/pi.rs', 's.model = string(v, "modelId");', '', 'inherited_models_change_retrospective_group_counts', 'claude_blocks_flags_and_synthetic_model'),
    ('pi_message_model', 'src/adapters/pi.rs', 's.model = Some(model);', '', 'pi_empty_response_and_explicit_message_model_override', 'claude_blocks_flags_and_synthetic_model'),
    ('pi_message', 'src/adapters/pi.rs', 'if role == "assistant" || role == "user" || role == "system" {', 'if false {', 'pi_empty_response_and_explicit_message_model_override', 'claude_blocks_flags_and_synthetic_model'),
    ('pi_call', 'src/adapters/pi.rs', '"toolCall" => {', '"removed-toolCall" => {', 'pi_model_tool_call_and_camel_case_flag', 'claude_blocks_flags_and_synthetic_model'),
    ('pi_flag', 'src/adapters/pi.rs', 'e.ok = m.get("isError").and_then(Value::as_bool).map(|v| !v);', '', 'pi_model_tool_call_and_camel_case_flag', 'claude_blocks_flags_and_synthetic_model'),
    ('pi_guard_reason', 'src/outcomes.rs', 'harness == "pi"', 'false', 'attribution_native_denials_instructions_children_and_retry_negative_control', 'claude_blocks_flags_and_synthetic_model'),
    ('native_history_dedup', 'src/schema.sql', 'WHERE copy_rank=1;', 'WHERE 1=1;', 'native_message_fragments_empty_replies_summary_and_history_dedup', 'lookup_cjk_latin_filters_and_show_references'),
    ('shell_ast', 'src/shell.rs', 'program(source, &mut out, 0).is_err()', '{ out.clear(); false }', 'brush_sites_include_nested_syntax_without_counting_quoted_program_names', 'codex_text_array_batch_failures_and_quoted_negative_control'),
    ('cjk_spacing', 'src/redaction.rs', "out.push(' ');\n            out.push(c);\n            out.push(' ');", "out.push(c);", 'lookup_cjk_latin_filters_and_show_references', 'codex_text_array_batch_failures_and_quoted_negative_control'),
    ('auxiliary_coverage', 'src/adapters/claude.rs', '"agent-name"', '"removed-agent-name"', 'inventory_variants_are_classified_with_future_shape_negative_control', 'codex_text_array_batch_failures_and_quoted_negative_control'),
]

def run_test(name):
    result = subprocess.run(['cargo', 'test', '--test', test_targets[name], name, '--', '--exact'], cwd=root, capture_output=True, text=True, timeout=180)
    output = result.stdout + result.stderr
    assertion = 'assertion' in output or 'canary leaked' in output
    source = (root / 'tests' / f'{test_targets[name]}.rs').read_text().splitlines()
    for line in re.findall(rf'panicked at tests/{re.escape(test_targets[name])}\.rs:(\d+):\d+:', output):
        position = int(line) - 1
        assertion |= 0 <= position < len(source) and source[position].lstrip().startswith(('assert!(', 'assert_eq!(', 'assert_ne!('))
    return result.returncode, f'test {name} ... ok' in output, f'test {name} ... FAILED' in output, assertion

rows.extend([
    ('truncated_rejection', 'src/outcomes.rs', 'if truncated {', 'if false {', 'split_script_error_and_truncated_batch_are_denials_with_transport_quote_control', 'claude_blocks_flags_and_synthetic_model'),
    ('server_call_kind', 'src/adapters/claude.rs', 'e.kind = "server_tool_call".into();', '', 'raw_shape_counts_survive_redaction_and_server_tools_keep_their_own_kind', 'codex_context_arguments_and_telemetry'),
    ('server_result_kind', 'src/adapters/claude.rs', 'e.kind = "server_tool_result".into();', '', 'raw_shape_counts_survive_redaction_and_server_tools_keep_their_own_kind', 'codex_context_arguments_and_telemetry'),
    ('structural_signature', 'src/store.rs', 'format!("{typ}/{subtype}:{}", keys.join(", "))', 'redact(&format!("{typ}/{subtype}:{}", keys.join(",")))', 'raw_shape_counts_survive_redaction_and_server_tools_keep_their_own_kind', 'codex_context_arguments_and_telemetry'),
])
rows.extend([
    ('context_pointers', 'src/adapters/mod.rs', 'if r.events.is_empty() {', 'if false {', 'lookup_cjk_latin_filters_and_show_references', 'codex_text_array_batch_failures_and_quoted_negative_control'),
    ('first_record_time', 'src/adapters/mod.rs', 'if state.first_ts.is_none() {', 'if state.first_ts.as_ref().is_none_or(|old| ts < *old) {', 'native_copy_ownership_uses_origin_time_before_filename', 'pi_model_tool_call_and_camel_case_flag'),
    ('native_origin_order', 'src/schema.sql', "ORDER BY coalesce(f.first_ts,'9999'),f.path,e.line_no,e.ordinal", 'ORDER BY f.path,e.line_no,e.ordinal', 'native_copy_ownership_uses_origin_time_before_filename', 'pi_model_tool_call_and_camel_case_flag'),
    ('body_entropy', 'src/redaction.rs', 'if entropy >= 3.5 {', 'if false {', 'adversarial_hex_fragments_basic_and_passphrase_canaries_are_removed', 'native_identifiers_and_cwd_remain_queryable_with_body_entropy_redaction'),
    ('basic_auth', 'src/redaction.rs', '(?:Bearer|Basic)', '(?:Bearer)', 'adversarial_hex_fragments_basic_and_passphrase_canaries_are_removed', 'codex_context_arguments_and_telemetry'),
    ('metadata_identity', 'src/adapters/mod.rs', 'state.session_id = redact_metadata(&state.session_id);', 'state.session_id = redact(&state.session_id);', 'native_identifiers_and_cwd_remain_queryable_with_body_entropy_redaction', 'codex_text_array_batch_failures_and_quoted_negative_control'),
    ('raw_scan', 'src/query.rs', 'String::from_utf8_lossy(&bytes)', 'display_range(&bytes)?', 'scan_matches_original_ranges_before_redacting_display', 'codex_context_arguments_and_telemetry'),
    ('continuation_location', 'src/store.rs', 'report.continuation = Some(path.to_string_lossy().into_owned());\n                break;', 'report.continuation = Some(redact_metadata(&path.to_string_lossy()));\n                break;', 'writer_lock_budget_missing_root_and_scan_cursor_are_visible', 'claude_blocks_flags_and_synthetic_model'),
    ('deferred_tail', 'src/store.rs', 'coverage = FileCoverage::DeferredTail;', 'coverage = FileCoverage::BudgetExhausted;', 'partial_tail_is_deferred_without_staleness_and_resumes_once', 'codex_context_arguments_and_telemetry'),
    ('ctime_invalidation', 'src/store.rs', 'meta.ctime(),\n            meta.ctime_nsec()', '0,\n            0', 'same_size_rewrite_with_preserved_mtime_equals_clean_rebuild', 'codex_context_arguments_and_telemetry'),
    ('initial_writer_lock', 'src/store.rs', 'db.busy_timeout(Duration::from_millis(50))?;', 'db.busy_timeout(Duration::from_millis(50))?; db.execute_batch(include_str!("schema.sql"))?;', 'initial_schema_creation_respects_the_writer_lock', 'codex_context_arguments_and_telemetry'),
    ('codex_telemetry_coverage', 'src/adapters/codex.rs', '"event_msg"\n        | "compacted"', '"compacted"', 'inventory_variants_are_classified_with_future_shape_negative_control', 'pi_model_tool_call_and_camel_case_flag'),
    ('codex_tool_search', 'src/adapters/codex.rs', '"function_call" | "custom_tool_call" | "tool_search_call"', '"function_call" | "custom_tool_call"', 'inventory_variants_are_classified_with_future_shape_negative_control', 'pi_model_tool_call_and_camel_case_flag'),
])
rows.append(('guard_stdout_control', 'src/outcomes.rs', 'harness != "codex"\n            && e.ok == Some(false)', 'true', 'successful_stdout_guard_examples_are_not_denials', 'codex_context_arguments_and_telemetry'))
rows.append(('credential_assignments', 'src/redaction.rs', 'if assignment.start() >= offset\n            && CREDENTIAL_NAME.is_match(&caps[1])', 'if false', 'adversarial_hex_fragments_basic_and_passphrase_canaries_are_removed', 'codex_context_arguments_and_telemetry'))
rows.append(('harness_outcome_scope', 'src/counting.rs', 'o.harness=f.harness AND ', '', 'call_outcomes_are_isolated_by_harness', 'pi_model_tool_call_and_camel_case_flag'))
rows.append(('count_program_binding', 'src/counting.rs', 'Value::Text(program.context("program parameter missing")?.into())', 'Value::Text("removed-program".into())', 'counts_state_units_denominators_unknowns_and_sql_is_read_only', 'codex_context_arguments_and_telemetry'))
rows.append(('mcp_transport_flag', 'src/outcomes.rs', 'if let Some(error) = m.get("isError").and_then(Value::as_bool) {', 'if let Some(error) = None::<bool> {', 'codex_mcp_transport_error_flag_is_text_evidence_with_content_control', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('opaque_payload', 'src/normalize.rs', 'if matches!(typ.as_str(), "thinking" | "image" | "fallback")', 'if false', 'opaque_blocks_are_not_materialized_and_normalized_counts_are_preserved', 'codex_context_arguments_and_telemetry'))
rows.extend([
    ('body_identifiers', 'src/redaction.rs', 'if hex.bytes().all(|b| b.is_ascii_hexdigit())', 'if false', 'body_identifiers_are_searchable_and_credential_context_stays_redacted', 'claude_blocks_flags_and_synthetic_model'),
    ('key_label', 'src/redaction.rs', 'CREDENTIAL_NAME.is_match(&caps[1])', 'CREDENTIAL_NAME.is_match(&caps[1]) && &caps[1] != "key"', 'body_identifiers_are_searchable_and_credential_context_stays_redacted', 'claude_blocks_flags_and_synthetic_model'),
    ('key_field', 'src/redaction.rs', 'if CREDENTIAL_NAME.is_match(&key)', 'if CREDENTIAL_NAME.is_match(&key) && key != "key"', 'synthetic_secret_canaries_absent_from_storage_and_lookup_outputs', 'claude_blocks_flags_and_synthetic_model'),
])
rows.extend([
    ('tool_output_prefix', 'src/normalize.rs', 'pub fn output_prefix(e: &mut crate::model::Event, output: &Value) {', 'pub fn output_prefix(e: &mut crate::model::Event, output: &Value) { return;', 'tool_output_prefix_is_bounded_and_raw_tail_remains_reachable', 'pi_empty_response_and_explicit_message_model_override'),
    ('attachment_output', 'src/adapters/claude.rs', 'if typ == "attachment"', 'if false', 'diagnostic_attachments_are_tool_outputs_without_creating_results', 'pi_empty_response_and_explicit_message_model_override'),
    ('pi_sections', 'src/adapters/pi.rs', 'if let Some(sections) = m["sections"].as_object()', 'if let Some(sections) = None::<&serde_json::Map<String, serde_json::Value>>', 'pi_sections_share_the_native_message_and_are_redacted', 'claude_blocks_flags_and_synthetic_model'),
    ('session_search', 'src/query.rs', 'FROM matches GROUP BY harness,session_id', 'FROM matches', 'ranked_search_pages_sessions_before_selecting_best_hits', 'claude_blocks_flags_and_synthetic_model'),
])
rows.extend([
    ('script_header', 'src/outcomes.rs', 's.trim_start().starts_with("Script failed")', 's.lines().any(|l| l.trim_start().starts_with("Script failed"))', 'result_envelopes_exclude_quoted_markers_and_preserve_native_denials', 'claude_blocks_flags_and_synthetic_model'),
    ('labeled_transport', 'src/outcomes.rs', 'if !matches!(\n                    key.as_str(),\n                    "content" | "results" | "items" | "result" | "output" | "stdout" | "stderr"\n                )', 'if false', 'result_envelopes_exclude_quoted_markers_and_preserve_native_denials', 'claude_blocks_flags_and_synthetic_model'),
    ('legacy_denial', 'src/outcomes.rs', 'line\n                .strip_prefix("Error: ")\n                .unwrap_or(line)\n                .starts_with("Permission to use ")', 'false', 'result_envelopes_exclude_quoted_markers_and_preserve_native_denials', 'codex_context_arguments_and_telemetry'),
    ('hook_denied', 'src/outcomes.rs', 'line.contains("DENIED:")', 'false', 'result_envelopes_exclude_quoted_markers_and_preserve_native_denials', 'codex_context_arguments_and_telemetry'),
    ('pi_leading_reason', 'src/outcomes.rs', '&& leading', '&& true', 'result_envelopes_exclude_quoted_markers_and_preserve_native_denials', 'claude_blocks_flags_and_synthetic_model'),
    ('pi_quoted_reason', 'src/outcomes.rs', '&& !line.contains("\\"")', '&& true', 'result_envelopes_exclude_quoted_markers_and_preserve_native_denials', 'claude_blocks_flags_and_synthetic_model'),
    ('exit_anchor', 'src/outcomes.rs', '(?m)^\\s*(?:Process exited with code|Exit code:)\\s*(-?\\d+)\\s*$', '(?m)(?:Process exited with code|Exit code:)\\s*(-?\\d+)', 'result_envelopes_exclude_quoted_markers_and_preserve_native_denials', 'claude_blocks_flags_and_synthetic_model'),
])
rows.extend([
    ('inventory_mapping', 'src/adapters/pi.rs', '"session_info"', '"custom"', 'inventory_variants_are_classified_with_future_shape_negative_control', 'claude_blocks_flags_and_synthetic_model'),
    ('count_group_model', 'src/counting.rs', "coalesce(e.model,'unknown')", "'constant'", 'counts_state_units_denominators_unknowns_and_sql_is_read_only', 'codex_context_arguments_and_telemetry'),
    ('count_group_week', 'src/counting.rs', "coalesce(strftime('%G-W%V',e.ts),'unknown')", "'constant'", 'counts_state_units_denominators_unknowns_and_sql_is_read_only', 'codex_context_arguments_and_telemetry'),
    ('count_duplicate_group', 'src/counting.rs', 'anyhow::ensure!(!keys.contains(&key), "duplicate --by field");', '', 'counts_state_units_denominators_unknowns_and_sql_is_read_only', 'codex_context_arguments_and_telemetry'),
    ('sql_row_bound', 'src/counting.rs', 'result.len() < 10_000', 'result.len() < 20_000', 'doctor_reports_stored_coverage_gaps_and_sql_bounds', 'claude_blocks_flags_and_synthetic_model'),
    ('sql_deadline', 'src/counting.rs', 'db.progress_handler(1000,', 'db.progress_handler(0,', 'doctor_reports_stored_coverage_gaps_and_sql_bounds', 'claude_blocks_flags_and_synthetic_model'),
    ('history_parent', 'src/adapters/codex.rs', 'p.pointer("/history_base/thread_id")', 'p.pointer("/removed-history/thread_id")', 'codex_context_arguments_and_telemetry', 'claude_blocks_flags_and_synthetic_model'),
    ('numeric_timestamp', 'src/normalize.rs', 'value.as_i64()', 'None::<i64>', 'pi_model_tool_call_and_camel_case_flag', 'claude_blocks_flags_and_synthetic_model'),
    ('shell_array', 'src/normalize.rs', 'args.get("command").and_then(Value::as_array)', 'None::<&Vec<Value>>', 'codex_context_arguments_and_telemetry', 'claude_blocks_flags_and_synthetic_model'),
    ('pi_system', 'src/adapters/pi.rs', 'role == "system"', 'false', 'pi_model_tool_call_and_camel_case_flag', 'claude_blocks_flags_and_synthetic_model'),
    ('fallback_payload', 'src/normalize.rs', '"thinking" | "image" | "fallback"', '"thinking" | "image"', 'opaque_blocks_are_not_materialized_and_normalized_counts_are_preserved', 'codex_context_arguments_and_telemetry'),
    ('doctor_models', 'src/counting.rs', 'SELECT count(*) FROM events WHERE model IS NULL', 'SELECT 0', 'doctor_reports_stored_coverage_gaps_and_sql_bounds', 'claude_blocks_flags_and_synthetic_model'),
    ('doctor_denials', 'src/counting.rs', "SELECT count(*) FROM denials WHERE reason_id='unknown'", 'SELECT 0', 'doctor_reports_stored_coverage_gaps_and_sql_bounds', 'claude_blocks_flags_and_synthetic_model'),
])
rows.append(('native_denial_kind', 'src/adapters/claude.rs', 'e.denials.retain(|(source, _)| source != "native_denial");', '', 'result_envelopes_exclude_quoted_markers_and_preserve_native_denials', 'codex_context_arguments_and_telemetry'))
rows.extend([
    ('session_access', 'src/query.rs', 'SELECT id FROM locations WHERE {column}=?', 'SELECT id FROM locations WHERE +{column}=?', 'session_access_uses_indexes_and_grep_bounds_the_first_sqlite_step', 'codex_context_arguments_and_telemetry'),
    ('grep_sqlite_deadline', 'src/query.rs', 'db.progress_handler(1000,', 'db.progress_handler(0,', 'session_access_uses_indexes_and_grep_bounds_the_first_sqlite_step', 'codex_context_arguments_and_telemetry'),
])
rows.extend([
    ('sql_duplicate_columns', 'src/counting.rs', 'names.iter().collect::<std::collections::HashSet<_>>().len() == names.len()', 'true', 'counts_state_units_denominators_unknowns_and_sql_is_read_only', 'codex_context_arguments_and_telemetry'),
    ('sql_writer_coverage', 'src/main.rs', 'let _read_lock = Store::read_lock(&path)?;', 'let _read_lock = Some(std::fs::File::open(path.with_extension("lock"))?);', 'cli_writer_contention_returns_stale_without_waiting', 'codex_context_arguments_and_telemetry'),
])
rows.extend([
    ('cursor_high_water', 'src/query.rs', 'if let Some(high_water) = self.high_water', 'if let Some(high_water) = None::<i64>', 'cli_streams_end_coverage_and_query_bound_cursors', 'codex_context_arguments_and_telemetry'),
    ('cursor_invalidation', 'src/store.rs', 'HAVING count(*)>0', 'HAVING 0', 'cli_streams_end_coverage_and_query_bound_cursors', 'codex_context_arguments_and_telemetry'),
    ('cursor_rebuild', 'src/store.rs', 'if version >= 4 {', 'if false {', 'cli_streams_end_coverage_and_query_bound_cursors', 'codex_context_arguments_and_telemetry'),
    ('cursor_search_order', 'src/query.rs', 'ordering AS MATERIALIZED (SELECT ? AS ranks)', 'ordering AS MATERIALIZED (SELECT CASE WHEN ? IS NULL THEN NULL ELSE NULL END AS ranks)', 'search_cursor_freezes_order_when_append_changes_fts_statistics', 'codex_context_arguments_and_telemetry'),
])
rows.extend([
    ('symbol_runs', 'src/redaction.rs', '[A-Za-z0-9_+/=.!@$%:&?\\-]{16,}', '[A-Za-z0-9_+/=\\-]{16,}', 'synthetic_secret_canaries_absent_from_storage_and_lookup_outputs', 'codex_context_arguments_and_telemetry'),
    ('slash_run', 'src/redaction.rs', 'if path ||', "if run.starts_with('/') ||", 'synthetic_secret_canaries_absent_from_storage_and_lookup_outputs', 'codex_context_arguments_and_telemetry'),
    ('credential_name_suffix', 'src/redaction.rs', '|(?:private[_-]?key|auth|sig|signature|bearer)$', '', 'synthetic_secret_canaries_absent_from_storage_and_lookup_outputs', 'codex_context_arguments_and_telemetry'),
    ('url_identifier', 'src/redaction.rs', 'URL.is_match(identifier)', 'false', 'synthetic_secret_canaries_absent_from_storage_and_lookup_outputs', 'codex_context_arguments_and_telemetry'),
    ('email_identifier', 'src/redaction.rs', 'EMAIL.is_match(identifier)', 'false', 'synthetic_secret_canaries_absent_from_storage_and_lookup_outputs', 'codex_context_arguments_and_telemetry'),
])
rows.extend([
    ('code_mode_literals', 'src/adapters/codex.rs', 'map(crate::code_mode::sites)', 'map(|_| vec![crate::shell::Site { program: None, argv: Vec::new(), parsed:false }])', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'),
    ('code_mode_opaque', 'src/code_mode.rs', 'if unparsed || referenced && out.is_empty() {', 'if false {', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'),
    ('code_mode_escape', 'src/code_mode.rs', "if c == '\\\\' {", 'if false {', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'),
    ('code_mode_template', 'src/code_mode.rs', "if quote == b'`' && c == '$'", 'if false', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'),
    ('code_mode_duplicate', 'src/code_mode.rs', 'if cmd.is_some() {', 'if false {', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'),
    ('code_mode_quoted_control', 'src/code_mode.rs', '|{STRING}|{COMMENTS}|{REGEX}|(?P<shell>', '|{COMMENTS}|{REGEX}|(?P<shell>', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'),
    ('code_mode_regex_control', 'src/code_mode.rs', '|{REGEX}|(?P<shell>', '|(?P<shell>', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'),
])
rows.append(('code_mode_comments', 'src/code_mode.rs', '|{COMMENTS}|{REGEX}|(?P<shell>', '|{REGEX}|(?P<shell>', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('code_mode_value_end', 'src/code_mode.rs', "if !tail.is_empty() && !tail.starts_with(',') {", 'if false {', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('code_mode_receiver', 'src/code_mode.rs', '} else if caps.name("shell").is_some() {', '} else if false {', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('code_mode_empty_literal', 'src/code_mode.rs', 'unparsed || referenced && out.is_empty()', 'unparsed', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('code_mode_quoted_key', 'src/code_mode.rs', '(?:cmd|"cmd"|\'cmd\')', '(?:cmd|"cmd"|\'removed-cmd\')', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('code_mode_override', 'src/code_mode.rs', 'if cmd.is_some() && caps.name("override").is_some() {', 'if false {', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('code_mode_field_array', 'src/code_mode.rs', '|{object}|{array}|{STRING}', '|{object}|{STRING}', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('code_mode_field_regex', 'src/code_mode.rs', '|{array}|{STRING}|{COMMENTS}|{REGEX}', '|{array}|{STRING}|{COMMENTS}', 'codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('diagnostic_source', 'src/adapters/claude.rs', '.get("source")', '.get("removed-source")', 'diagnostic_attachments_are_tool_outputs_without_creating_results', 'pi_empty_response_and_explicit_message_model_override'))
rows.append(('transport_stdout_control', 'src/outcomes.rs', ' | "output" | "stdout" | "stderr"', '', 'result_envelopes_exclude_quoted_markers_and_preserve_native_denials', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('code_identifiers', 'src/redaction.rs', '|| identifier_shaped(identifier)', '', 'body_identifiers_are_searchable_and_credential_context_stays_redacted', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('url_segments', 'src/redaction.rs', 'return redact_url(run);', 'return run.to_owned();', 'synthetic_secret_canaries_absent_from_storage_and_lookup_outputs', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('identifier_punctuation', 'src/redaction.rs', 'identifier_shaped(identifier)', 'identifier_shaped(run)', 'body_identifiers_are_searchable_and_credential_context_stays_redacted', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('identifier_flags', 'src/redaction.rs', 'let run = run\n        .strip_prefix("--")\n        .or_else(|| run.strip_prefix(\'-\'))\n        .unwrap_or(run);', 'let run = run;', 'body_identifiers_are_searchable_and_credential_context_stays_redacted', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('path_locator', 'src/redaction.rs', 'for _ in 0..2 {', 'for _ in 0..0 {', 'body_identifiers_are_searchable_and_credential_context_stays_redacted', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('path_locator_shape', 'src/redaction.rs', "&& path.contains(['/', '.'])", '', 'body_identifiers_are_searchable_and_credential_context_stays_redacted', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('static_help', 'src/main.rs', 'print!("{e}");', 'print!("");', 'cli_static_help_and_version_preserve_locators_and_errors_redact_values', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('known_token_prefix', 'src/redaction.rs', '(?:gh[pousr]_|github_pat_|xox[baprs]-|AKIA|ASIA)', '(?:github_pat_|xox[baprs]-|AKIA|ASIA)', 'body_identifiers_are_searchable_and_credential_context_stays_redacted', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('missing_root_absent', 'src/store.rs', 'report.stale = roots.iter().any(|r| {', 'report.stale = !report.missing_roots.is_empty() || roots.iter().any(|r| {', 'writer_lock_budget_missing_root_and_scan_cursor_are_visible', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('missing_root_indexed', 'src/store.rs', '!r.path.exists()\n                && indexed', 'false\n                && indexed', 'a_missing_root_is_stale_only_when_it_held_indexed_files', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('missing_root_rebuild', 'src/store.rs', '// Read before initialize, which drops these rows on a full rebuild.\n', 'self.initialize(&_lock, full)?;\n        ', 'a_missing_root_is_stale_only_when_it_held_indexed_files', 'claude_blocks_flags_and_synthetic_model'))
rows.append(('sql_missing_index', 'src/main.rs', 'path.exists(),\n            "no index at', 'true,\n            "no index at', 'sql_without_an_index_names_the_build_command', 'claude_blocks_flags_and_synthetic_model'))
test_sources = [(path.stem, path.read_text()) for path in (root / 'tests').glob('*.rs')]
test_targets = {}
for name in {name for row in rows for name in row[4:]}:
    targets = [target for target, source in test_sources if f'fn {name}(' in source]
    if len(targets) != 1:
        raise RuntimeError(f'{name}: expected one integration test target')
    test_targets[name] = targets[0]
results = []
if len(sys.argv) > 2:
    rows = [row for row in rows if row[0] in sys.argv[2:]]
for rule, relative, before, after, test, control in rows:
    path = root / relative
    original = path.read_bytes()
    source = original.decode()
    if source.count(before) != 1:
        raise RuntimeError(f'{rule}: mutation anchor must occur exactly once')
    try:
        path.write_text(source.replace(before, after, 1))
        code, passed, failed, assertion = run_test(test)
        cc, cp, cf, ca = run_test(control)
        result = dict(rule=rule, failing_test=test, negative_control=control,
                      assertion_failed=code != 0 and failed and assertion,
                      negative_control_passed=cc == 0 and cp)
        results.append(result)
        print(json.dumps(result), flush=True)
        if not result['assertion_failed'] or not result['negative_control_passed']:
            raise RuntimeError(f'{rule}: ablation did not satisfy both checks')
    finally:
        path.write_bytes(original)
if len(sys.argv) >= 2:
    Path(sys.argv[1]).write_text(json.dumps(results, indent=2)+'\n')
