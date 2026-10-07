# Format normalization

All records in testdata are synthetic. The source inventory contains 80 root/content variants from the three declared roots; this document maps every variant to an array entry in `testdata/inventory.json`. Its census selected files by mtime since 2026-08-01, so it is not a universal schema or a runtime guarantee. Rules below own their reasons once. Extra fields, ancestry, guardian overlays, and retries are exercised in `testdata/attribution.json`.

## Rules and regression counts

`scripts/ablate.py` records the failing assertion test and unrelated passing control for each rule. The acceptance report owns actual results; this table does not claim an unrun check. Fixtures prefixed with tests/ are inline synthetic records. Coverage-only auxiliary rules change the known/unknown record count, not the message count.

| Rule | Fixture | Reason | Count regression |
| --- | --- | --- | --- |
| `claude_message` | `testdata/structure.jsonl` | Tool-only and empty assistant attempts remain messages; tool-result-only user envelopes are tool results. | `native_message_fragments_empty_replies_summary_and_history_dedup` |
| `claude_model` | `testdata/claude.jsonl` | The explicit synthetic placeholder stays on its own event without replacing the last real model. | `inherited_models_change_retrospective_group_counts` |
| `claude_flag` | `testdata/claude.jsonl` | is_error is authoritative Boolean evidence; absent flags remain unknown. | `claude_blocks_flags_and_synthetic_model` |
| `claude_fragment_id` | `testdata/structure.jsonl` | Transcript fragments can share message.id despite different UUIDs; the message identity owns the logical count. | `native_message_fragments_empty_replies_summary_and_history_dedup` |
| `claude_compaction` | `testdata/structure.jsonl` | isCompactSummary is context, not a new user interaction. | `native_message_fragments_empty_replies_summary_and_history_dedup` |
| `claude_sidechain` | `testdata/attribution.json` | Only explicit isSidechain evidence establishes delegated kind. | `attribution_native_denials_instructions_children_and_retry_negative_control` |
| `claude_classifier` | `testdata/attribution.json` | Native automode-blocked qualifies classifier refusal without inspecting message quotations. | `attribution_native_denials_instructions_children_and_retry_negative_control` |
| `codex_model` | `testdata/codex.jsonl` | turn_context defines the active model; startup metadata does not. | `inherited_models_change_retrospective_group_counts` |
| `json_arguments` | `testdata/codex.jsonl` | Function arguments can be a JSON string, and decoding is needed to reach cmd. | `codex_context_arguments_and_telemetry` |
| `codex_base_instructions` | `testdata/codex.jsonl` | Hash the recorded base text without retaining another instruction body. | `codex_context_arguments_and_telemetry` |
| `codex_injected_instructions` | `testdata/attribution.json` | Injected AGENTS.md text supplies a hash only when base instructions are absent. | `attribution_native_denials_instructions_children_and_retry_negative_control` |
| `codex_text_outcome` | `testdata/outcomes.jsonl` | Codex has no error flag; anchored transport status supplies text evidence. | `codex_text_array_batch_failures_and_quoted_negative_control` |
| `batch_rejection` | `testdata/outcomes.jsonl` | A completed wrapper can contain rejected Promise items; the native call result still failed. | `codex_text_array_batch_failures_and_quoted_negative_control` |
| `result_container` | `testdata/outcomes.jsonl:7` | Codex can print complete JSON values whose i/index field labels a Promise result under result. Decode that immediate container while retaining the transport stdout boundary. | `codex_text_array_batch_failures_and_quoted_negative_control` |
| `mixed_outcome_failure` | `testdata/outcomes.jsonl:8` | A successful MCP item cannot erase a nonzero shell exit in the same native result. Failure evidence wins across transport types. | `codex_text_array_batch_failures_and_quoted_negative_control` |
| `native_hook_flag` | `testdata/hook-check.jsonl:2` | A native successful result can quote a hook denial. Its success Boolean excludes text-derived denial inference. | `claude_hook_check_errors_respect_native_success_flags` |
| `hook_check_error` | `testdata/hook-check.jsonl:1` | Claude marks a failed PreToolUse hook check as permission-rule, but its anchored hook error identifies the hook source even without DENIED. Its unknown reason stays explicit. | `claude_hook_check_errors_respect_native_success_flags` |
| `hook_prefix` | `testdata/outcomes.jsonl` | Only result envelopes with native hook prefixes qualify; transport stdout quotations do not. | `codex_text_array_batch_failures_and_quoted_negative_control` |
| `newline_hook` | `testdata/inventory.json[53]`, `testdata/truncated.jsonl:1,3` | Codex input_text blocks can start with Script error: followed by a newline and Command blocked by PreToolUse hook:. Preserve the hook denial without treating a successful transport's quoted output as a refusal. | `split_script_error_and_truncated_batch_are_denials_with_transport_quote_control` |
| `pi_model` | `testdata/pi.jsonl` | model_change.modelId carries into following messages. | `inherited_models_change_retrospective_group_counts` |
| `pi_message_model` | `tests/counting.rs::pi_empty_response_and_explicit_message_model_override` | Explicit message model overrides the previous model_change for following attempts. | `pi_empty_response_and_explicit_message_model_override` |
| `pi_message` | `tests/counting.rs::pi_empty_response_and_explicit_message_model_override` | An empty errored assistant response is still an attempt; a retry is a separate message. | `pi_empty_response_and_explicit_message_model_override` |
| `pi_call` | `testdata/pi.jsonl` | Pi uses toolCall content blocks and arguments instead of Claude tool_use/input. | `pi_model_tool_call_and_camel_case_flag` |
| `pi_flag` | `testdata/pi.jsonl` | Pi isError is camel case on the toolResult message, not its content block. | `pi_model_tool_call_and_camel_case_flag` |
| `pi_guard_reason` | `testdata/attribution.json` | An error result beginning with a known guard reason qualifies; nested diagnostics and quoted prose do not create attempts. | `attribution_native_denials_instructions_children_and_retry_negative_control` |
| `native_history_dedup` | `testdata/structure.jsonl` | Native identity deduplicates copies across files; identical text with different IDs remains distinct. | `native_message_fragments_empty_replies_summary_and_history_dedup` |
| `shell_ast` | `tests/counting.rs::brush_sites_include_nested_syntax_without_counting_quoted_program_names` | An AST distinguishes command positions, substitutions, and function bodies from a quoted rg argument. | `brush_sites_include_nested_syntax_without_counting_quoted_program_names` |
| `cjk_spacing` | `testdata/claude.jsonl` | unicode61 alone does not split Han character sequences; spacing plus phrase matching supports a two-character substring. | `lookup_cjk_latin_filters_and_show_references` |
| `auxiliary_coverage` | `testdata/inventory.json` | Known auxiliary roots remain context pointers and known-record coverage, not additional logical messages. | `inventory_variants_are_classified_with_future_shape_negative_control` |
| `truncated_rejection` | `testdata/truncated.jsonl` | A truncated batch can retain a complete rejection reason even when its surrounding JSON is incomplete. | `split_script_error_and_truncated_batch_are_denials_with_transport_quote_control` |
| `server_call_kind` | `testdata/inventory.json` | server_tool_use is not a local native tool-call attempt. | `raw_shape_counts_survive_redaction_and_server_tools_keep_their_own_kind` |
| `server_result_kind` | `testdata/inventory.json` | advisor_tool_result is a server result and must not inflate local native result counts. | `raw_shape_counts_survive_redaction_and_server_tools_keep_their_own_kind` |
| `structural_signature` | `testdata/structure.jsonl` | Redacting the composed discriminator/key list as body entropy destroys structural count keys; sanitize its pieces instead. | `raw_shape_counts_survive_redaction_and_server_tools_keep_their_own_kind` |
| `context_pointers` | `testdata/codex.jsonl` | Metadata and telemetry lines need exact show pointers while staying out of logical-message counts. | `lookup_cjk_latin_filters_and_show_references` |
| `first_record_time` | `tests/counting.rs::native_copy_ownership_uses_origin_time_before_filename` | Inherited old timestamps cannot replace the source file start time used for ownership ordering. | `native_copy_ownership_uses_origin_time_before_filename` |
| `native_origin_order` | `tests/counting.rs::native_copy_ownership_uses_origin_time_before_filename` | Filename sorting can attribute a native copy to a later session; compare start timestamps before deterministic ties. | `native_copy_ownership_uses_origin_time_before_filename` |
| `body_entropy` | `testdata/privacy.json` | Mixed-case base62/base64-like runs and fragments need entropy redaction without a recognizable credential label. Hex is tested under credential context. | `adversarial_hex_fragments_basic_and_passphrase_canaries_are_removed` |
| `body_identifiers` | `testdata/identifiers.jsonl:1,2` | Body lookups use wallet addresses, commit hashes, session IDs and code identifiers. Hex/0x/UUID, multi-component absolute paths, email and identifier-shaped runs survive entropy alone; URLs check their components independently. Credential patterns are applied first. | `body_identifiers_are_searchable_and_credential_context_stays_redacted` |
| `code_identifiers` | `testdata/identifiers.jsonl:1,2,3` | Adding dot and colon to the run alphabet must not hide lookup keys such as dotted calls or Rust namespace paths. Split on ., ::, / and -; every segment must match `[A-Za-z_]+[0-9]*`. Trailing sentence punctuation and an optional one- or two-hyphen flag prefix are excluded first. Digit-letter interleaving, internal empty segments and single colons outside digit-suffix locators retain entropy redaction, while credential context and known prefixes still mask identifier-shaped values. | `body_identifiers_are_searchable_and_credential_context_stays_redacted` |
| `path_locator` | `testdata/identifiers.jsonl:1,2` | Evidence uses absolute and relative path:line and path:line:column; strip at most two nonempty ASCII-digit suffixes once before both absolute-path and identifier-shape recognition. Credential context still runs first, and high-entropy non-identifier controls with digit suffixes remain redacted. | `body_identifiers_are_searchable_and_credential_context_stays_redacted` |
| `static_help` | Synthetic help/version and invalid-harness arguments in `tests/cli.rs` | Successful Clap output is the program's static help/version and stays unchanged; errors can echo values and still pass through redaction. | `cli_static_help_and_version_preserve_locators_and_errors_redact_values` |
| `identifier_punctuation` | `testdata/identifiers.jsonl:1` | A sentence-final dot or colon is punctuation around the lookup key, so it must not introduce an empty identifier segment. | `body_identifiers_are_searchable_and_credential_context_stays_redacted` |
| `identifier_flags` | `testdata/identifiers.jsonl:1,2` | Command-line flag names are lookup keys; ignore one or two leading hyphens while retaining the rejection of internal empty segments. | `body_identifiers_are_searchable_and_credential_context_stays_redacted` |
| `known_token_prefix` | `testdata/identifiers.jsonl:2` | A known token prefix must mask an alphabetic value even when the full run satisfies the identifier shape. Prefix recognition precedes entropy exemptions. | `body_identifiers_are_searchable_and_credential_context_stays_redacted` |
| `key_label` | `testdata/identifiers.jsonl:2` | A plain key: label supplies credential context even when the value has a preserved identifier shape. | `body_identifiers_are_searchable_and_credential_context_stays_redacted` |
| `key_field` | `testdata/identifiers.jsonl:3`, `testdata/credential-context.json` | A credential-named JSON field suppresses its entire value before entropy exemptions; ordinary structured fields retain their values. | `synthetic_secret_canaries_absent_from_storage_and_lookup_outputs` |
| `tool_output_prefix` | `testdata/output-prefix.json[0,1,2]`, UTF-8 boundary cases in `tests/lookup.rs` | Tool-result-only lookup evidence needs a bounded indexed prefix. Extract the first 2 KiB on a UTF-8 boundary, redact it and cap expansion; full raw ranges retain the tail. | `tool_output_prefix_is_bounded_and_raw_tail_remains_reachable` |
| `attachment_output` | `testdata/output-prefix.json[3,4]` | Claude diagnostics carry message text under attachment.files[].diagnostics[].source plus message. The explicit diagnostic type qualifies a tool output without inventing a native result or failure. | `diagnostic_attachments_are_tool_outputs_without_creating_results` |
| `pi_sections` | `testdata/pi-sections.jsonl:2` | Pi system messages can carry instruction text in message.sections values alongside content. These values belong to the same native message and pass body redaction. | `pi_sections_share_the_native_message_and_are_redacted` |
| `session_search` | Synthetic records in `tests/lookup.rs::ranked_search_pages_sessions_before_selecting_best_hits` | Repeated hits within one session must not consume the session page. Group by harness and native session ID before applying the limit, retaining best hits and precise source refs. | `ranked_search_pages_sessions_before_selecting_best_hits` |
| `basic_auth` | `testdata/privacy.json` | Authorization: Basic carries a second whitespace-delimited credential that a generic assignment pattern can leave behind. | `adversarial_hex_fragments_basic_and_passphrase_canaries_are_removed` |
| `metadata_identity` | `tests/lookup.rs::native_identifiers_and_cwd_remain_queryable_with_body_entropy_redaction` | Native session IDs and project paths must remain usable filters; recognizable patterns still apply. | `native_identifiers_and_cwd_remain_queryable_with_body_entropy_redaction` |
| `raw_scan` | `tests/lookup.rs::scan_matches_original_ranges_before_redacting_display` | The raw regex can locate a redacted body identifier while the displayed snippet remains redacted. | `scan_matches_original_ranges_before_redacting_display` |
| `deferred_tail` | `tests/foundations.rs::partial_tail_is_deferred_without_staleness_and_resumes_once` | A live partial tail is expected deferred work rather than budget exhaustion or a failed full index. | `partial_tail_is_deferred_without_staleness_and_resumes_once` |
| `continuation_location` | Synthetic filename in `tests/lookup.rs::writer_lock_budget_missing_root_and_scan_cursor_are_visible` | A refresh continuation is a filesystem location; applying content redaction can destroy its spelling and make it unusable. | `writer_lock_budget_missing_root_and_scan_cursor_are_visible` |
| `ctime_invalidation` | `tests/foundations.rs::same_size_rewrite_with_preserved_mtime_equals_clean_rebuild` | An equal-size rewrite may preserve mtime; ctime provides an additional change witness. | `same_size_rewrite_with_preserved_mtime_equals_clean_rebuild` |
| `initial_writer_lock` | `tests/lookup.rs::initial_schema_creation_respects_the_writer_lock` | Schema setup is a writer too and must wait until the owning lock permits it. | `initial_schema_creation_respects_the_writer_lock` |
| `codex_telemetry_coverage` | `testdata/inventory.json` | event_msg aliases affect known/unknown record coverage. Removing the alias does not create a logical message and is not claimed to do so. | `inventory_variants_are_classified_with_future_shape_negative_control` |
| `codex_tool_search` | `testdata/inventory.json` | The tool_search_call discriminator is a native attempt, separate from inter-agent communication. | `inventory_variants_are_classified_with_future_shape_negative_control` |
| `guard_stdout_control` | `tests/counting.rs::successful_stdout_guard_examples_are_not_denials` | Output with a successful or missing result flag can print a guard example; generic guard prefixes require a Claude/Pi failure flag. | `successful_stdout_guard_examples_are_not_denials` |
| `credential_assignments` | `testdata/privacy.json` | Quoted credential values can contain spaces; replacing only the first word leaks the remaining passphrase. Credential-named URL query values need pattern redaction even when too short for entropy detection. | `adversarial_hex_fragments_basic_and_passphrase_canaries_are_removed` |
| `harness_outcome_scope` | `tests/counting.rs::call_outcomes_are_isolated_by_harness` | Equal native call/session IDs in different harnesses must not share outcomes. | `call_outcomes_are_isolated_by_harness` |
| `count_program_binding` | `testdata/claude.jsonl`, combined filters in `tests/counting.rs` | Count's program predicate appears in SELECT before the anonymous WHERE filter parameters. A named :program parameter binds once by SQLite parameter identity; shared filters retain their own parameters for every metric. | `counts_state_units_denominators_unknowns_and_sql_is_read_only` |
| `mcp_transport_flag` | `tests/counting.rs::codex_mcp_transport_error_flag_is_text_evidence_with_content_control` | Codex output can serialize an MCP isError flag; preserve it as transport text evidence, with failure winning over later success and a content file-dump quotation as a negative control. | `codex_mcp_transport_error_flag_is_text_evidence_with_content_control` |
| `opaque_payload` | `testdata/opaque.jsonl` | Pi thinkingSignature can contain 10-16 MiB strings that the adapter ignores. Borrowing discarded thinking/image blocks avoids materializing them while preserving message and call counts. | `opaque_blocks_are_not_materialized_and_normalized_counts_are_preserved` |
| `script_header` | `testdata/envelopes.json` | Only a leading script header marks a transport failure; a later quoted header in a file dump does not. | `result_envelopes_exclude_quoted_markers_and_preserve_native_denials` |
| `labeled_transport` | `testdata/envelopes.json` | A custom label can wrap a complete shell/MCP transport object; inspect that object without traversing stdout. | `result_envelopes_exclude_quoted_markers_and_preserve_native_denials` |
| `legacy_denial` | `testdata/envelopes.json` | Claude error text can start with Permission to use, optionally preceded by Error:, without naming its mechanism; preserve native_denial rather than inventing classifier attribution. | `result_envelopes_exclude_quoted_markers_and_preserve_native_denials` |
| `hook_denied` | `testdata/envelopes.json` | A PreToolUse DENIED envelope need not contain hook error wording. | `result_envelopes_exclude_quoted_markers_and_preserve_native_denials` |
| `pi_leading_reason` | `testdata/envelopes.json` | Only the first nonempty Pi result block can supply a bare guard reason. | `result_envelopes_exclude_quoted_markers_and_preserve_native_denials` |
| `pi_quoted_reason` | `testdata/envelopes.json` | Quoted Pi error prose is not a bare guard refusal. | `result_envelopes_exclude_quoted_markers_and_preserve_native_denials` |
| `exit_anchor` | `testdata/envelopes.json` | A numeric exit must occupy a complete transport line; inline quotations remain unknown. | `result_envelopes_exclude_quoted_markers_and_preserve_native_denials` |
| `inventory_mapping` | `testdata/inventory.json` | Every variant must retain its own classification even when compensating mistakes preserve the total known count. | `inventory_variants_are_classified_with_future_shape_negative_control` |
| `count_group_model` | `tests/counting.rs` | Model attribution must survive grouped counts. | `counts_state_units_denominators_unknowns_and_sql_is_read_only` |
| `count_group_week` | `tests/counting.rs` | Counts group timestamps by their UTC ISO week. | `counts_state_units_denominators_unknowns_and_sql_is_read_only` |
| `count_duplicate_group` | `tests/counting.rs` | Repeated group names are an error rather than duplicate output keys. | `counts_state_units_denominators_unknowns_and_sql_is_read_only` |
| `sql_row_bound` | `tests/counting.rs` | A result above 10,000 rows fails instead of claiming partial success. | `doctor_reports_stored_coverage_gaps_and_sql_bounds` |
| `sql_deadline` | `tests/counting.rs` | SQLite work is interrupted within the two-second SQL budget, including before the first row. | `doctor_reports_stored_coverage_gaps_and_sql_bounds` |
| `history_parent` | `tests/foundations.rs` | Codex history_base.thread_id is recorded ancestry when other parent fields are absent. | `codex_context_arguments_and_telemetry` |
| `numeric_timestamp` | `tests/foundations.rs` | Pi nested message timestamps can be numeric milliseconds. | `pi_model_tool_call_and_camel_case_flag` |
| `shell_array` | `tests/foundations.rs` | Codex shell commands are argv arrays; quote each element before Brush parsing. | `codex_context_arguments_and_telemetry` |
| `pi_system` | `tests/foundations.rs` | A Pi system message is a native message, not an auxiliary record. | `pi_model_tool_call_and_camel_case_flag` |
| `fallback_payload` | `testdata/opaque.jsonl` | Opaque fallback blocks carry no searchable body and need no owned payload copy. | `opaque_blocks_are_not_materialized_and_normalized_counts_are_preserved` |
| `doctor_models` | `tests/counting.rs` | Doctor reports missing models among stored rows, including context. | `doctor_reports_stored_coverage_gaps_and_sql_bounds` |
| `doctor_denials` | `tests/counting.rs` | Doctor reports unknown reasons among stored denial rows. | `doctor_reports_stored_coverage_gaps_and_sql_bounds` |
| `native_denial_kind` | `tests/counting.rs::result_envelopes_exclude_quoted_markers_and_preserve_native_denials` | An explicit Claude toolDenialKind supplies the mechanism instead of the generic text-derived native_denial source. | `result_envelopes_exclude_quoted_markers_and_preserve_native_denials` |
| `session_access` | `tests/lookup.rs::session_access_uses_indexes_and_grep_bounds_the_first_sqlite_step` | Resolve native session strings and file paths through indexed identities before scanning, preserving both selection forms without a cross-table OR. EXPLAIN must show SEARCH through events_session; scanning that index is insufficient. | `session_access_uses_indexes_and_grep_bounds_the_first_sqlite_step` |
| `grep_sqlite_deadline` | `tests/lookup.rs::session_access_uses_indexes_and_grep_bounds_the_first_sqlite_step` | The first SQLite step can exhaust grep coverage before returning a row; a progress handler must interrupt it and preserve the continuation. | `session_access_uses_indexes_and_grep_bounds_the_first_sqlite_step` |
| `sql_duplicate_columns` | `tests/counting.rs` | SQL object output cannot preserve duplicate or redaction-colliding names; require unique aliases instead of overwriting columns. | `counts_state_units_denominators_unknowns_and_sql_is_read_only` |
| `sql_writer_coverage` | `tests/cli.rs` | SQL must mark cached reads incomplete/stale while the writer lock covers a rebuild. | `cli_writer_contention_returns_stale_without_waiting` |
| `cursor_high_water` | `tests/cli.rs::cli_streams_end_coverage_and_query_bound_cursors` | Later pages must exclude records added above the first page event high-water mark. | `cli_streams_end_coverage_and_query_bound_cursors` |
| `cursor_invalidation` | `tests/cli.rs::cli_streams_end_coverage_and_query_bound_cursors` | File replacement/deletion records the first removed event, invalidating only cursors whose high-water mark includes it. | `cli_streams_end_coverage_and_query_bound_cursors` |
| `cursor_rebuild` | `tests/cli.rs::cli_streams_end_coverage_and_query_bound_cursors` | Full rebuild records a zero-boundary invalidation before replacing indexed events. | `cli_streams_end_coverage_and_query_bound_cursors` |
| `cursor_search_order` | `tests/cli.rs::search_cursor_freezes_order_when_append_changes_fts_statistics` | Appends change global FTS statistics even below a row watermark; preserve the first request session order in its cursor. | `search_cursor_freezes_order_when_append_changes_fts_statistics` |
| `symbol_runs` | `testdata/credential-context.json` | Symbol-bearing unmarked passwords must remain one entropy candidate rather than short punctuation-split fragments. | `synthetic_secret_canaries_absent_from_storage_and_lookup_outputs` |
| `slash_run` | `testdata/credential-context.json` | A leading slash alone does not establish a path; preserve only multi-component path shapes and redact high-entropy slash-prefixed blobs. | `synthetic_secret_canaries_absent_from_storage_and_lookup_outputs` |
| `credential_name_suffix` | `testdata/credential-context.json` | Text assignments and JSON fields share credential-name recognition, including names ending in private_key, auth, sig, signature or bearer. | `synthetic_secret_canaries_absent_from_storage_and_lookup_outputs` |
| `url_segments` | `testdata/credential-context.json` | Slack-webhook-shaped path segments and high-entropy ref query values must be absent in storage and every display surface, even without credential names. | `synthetic_secret_canaries_absent_from_storage_and_lookup_outputs` |
| `url_identifier` | `testdata/credential-context.json` | URLs retain scheme/host and ordinary paths/query values for lookup; high-entropy path segments and non-credential query values are judged independently instead of exempting the whole carrier. | `synthetic_secret_canaries_absent_from_storage_and_lookup_outputs` |
| `email_identifier` | `testdata/credential-context.json` | Public email shapes must remain searchable when the broad entropy candidate includes their punctuation. | `synthetic_secret_canaries_absent_from_storage_and_lookup_outputs` |
| `code_mode_literals` | `testdata/code-mode.json` | Codex exec wrapper calls with literal cmd strings contain recoverable shell sources attached to the native wrapper call. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_opaque` | `testdata/code-mode.json` | Variables, interpolated templates and unsupported references with any receiver or none retain one opaque unparsed site per wrapper; bare exec_command and codex.exec_command must stay in the denominator. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_escape` | `testdata/code-mode.json` | JavaScript quote, hexadecimal, four-digit scalar Unicode and line-continuation escapes decode before Brush sees the shell source. Invalid, braced Unicode and surrogate escapes stay unparsed; the real-corpus literal comparison found no need to combine surrogate units or decode braced values. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_template` | `testdata/code-mode.json` | Template interpolation prevents exact static reconstruction; escaped interpolation markers remain literal text. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_duplicate` | `testdata/code-mode.json` | Duplicate cmd fields cannot be counted as multiple shell sources or guessed from their first value. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_override` | `testdata/code-mode.json` spread, computed, spread_before, computed_before, spread_object_after, computed_quoted_after, commented_spread_after, commented_computed_after, mixed_override, nested_override_data | A spread or computed property after a literal cmd can override it and makes the call opaque; the same members only before cmd leave the literal extractable. Comments between the comma and member do not change that order. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_field_array` | `testdata/code-mode.json` array_override_data | Flat array metadata is a field value; its comma-separated spreads are not later object properties overriding cmd. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_field_regex` | `testdata/code-mode.json` regex_override_data | Regular-expression metadata containing spread/computed-looking text is data, not later object properties overriding cmd. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_quoted_control` | `testdata/code-mode.json` | Quoted JavaScript examples and comments do not create tool-call sites. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_comments` | `testdata/code-mode.json` quoted_control | Comments containing call examples do not create shell sites; the corpus contains comment tokens in wrappers. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_quoted_key` | `testdata/code-mode.json` quoted_key_after_fields, escapes | Bare, double-quoted and single-quoted cmd keys denote the same field, including after other metadata. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_empty_literal` | `testdata/code-mode.json` empty_literal | A shell-tool reference whose literal yields no syntax site still contributes exactly one opaque site. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_value_end` | `testdata/code-mode.json` concatenation | A quoted prefix followed by an expression is not the command value; require a comma or object-field end before decoding. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_receiver` | `testdata/code-mode.json` bare_receiver, codex_receiver | Unsupported receiver spellings and receiverless calls retain one opaque site rather than disappearing from command-site counts. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `code_mode_regex_control` | `testdata/code-mode.json` | Regular-expression literals containing tool names are data, not tool-call sites. Their first body character excludes an unescaped star, keeping block comments in the comment branch. | `codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed` |
| `diagnostic_source` | `testdata/output-prefix.json[3]` | The diagnostic source names the checker even when its message does not; put source before the bounded message prefix. | `diagnostic_attachments_are_tool_outputs_without_creating_results` |
| `transport_stdout_control` | `testdata/envelopes.json` | Output, stdout and stderr remain payload data even when an object inside them resembles a shell transport; a real leading content error can still override a false MCP error flag. | `result_envelopes_exclude_quoted_markers_and_preserve_native_denials` |

## Codex output variants

The inventory's output-marker categories overlap and do not count distinct failed calls. Its hook example places Script error: and the hook prefix on one line; the verified witness has a newline between them, now represented explicitly in inventory fixture entry 53. The existing same-line fixture remains a separate supported form. Native Codex results have no error Boolean; decoded MCP transport isError remains text-derived evidence.

| Output variant | Synthetic fixture | Owning rule |
| --- | --- | --- |
| Nonzero Process exited with code N | `testdata/outcomes.jsonl:1` | `codex_text_outcome` |
| Serialized nonzero exit_code | `testdata/outcomes.jsonl:3` | `codex_text_outcome` |
| Script failed in an input_text array | `testdata/truncated.jsonl:1`, `testdata/inventory.json[53]` | `codex_text_outcome` |
| Script error: with a newline before the hook prefix | `testdata/truncated.jsonl:1`, `testdata/inventory.json[53]` | `newline_hook` |
| Same-line Script error: and hook prefix | `testdata/outcomes.jsonl:2` | `hook_prefix` |
| Serialized batch rejection | `testdata/outcomes.jsonl:3` | `batch_rejection` |
| i/index wrapper containing a Promise result | `testdata/outcomes.jsonl:7` | `result_container` |
| Exit zero alongside quoted failure markers | `testdata/outcomes.jsonl:4`, `testdata/truncated.jsonl:3` | `codex_text_outcome` |
| String output | `testdata/outcomes.jsonl:1,5,6` | `codex_text_outcome` |
| input_text array output | `testdata/outcomes.jsonl:2,3`, `testdata/truncated.jsonl:1,2` | `codex_text_outcome`, `newline_hook` |

## Inventory variants

Known auxiliary records receive context pointers without body storage. Thinking, images, fallback, compaction, and branch records do not reconstruct deferred content; their parent message can still be a logical attempt. Pi custom payloads remain unclassified. Known classification is based on the adapter root/payload discriminator; novel content fields within a known message do not yet have a separate unknown-block census.

| Harness | Observed shape | Fixture array index | Owning rule or retained context |
| --- | --- | ---: | --- |
| claude | `agent-name / none` | 0 | `auxiliary_coverage` |
| claude | `ai-title / none` | 1 | `auxiliary_coverage` |
| claude | `assistant / none` | 2 | `claude_message` |
| claude | `atis-latch / none` | 3 | `auxiliary_coverage` |
| claude | `attachment / none` | 4 | `auxiliary_coverage` |
| claude | `continued-in / none` | 5 | `auxiliary_coverage` |
| claude | `cost-state / none` | 6 | `auxiliary_coverage` |
| claude | `custom-title / none` | 7 | `auxiliary_coverage` |
| claude | `failed / none` | 8 | `auxiliary_coverage` |
| claude | `file-history-delta / none` | 9 | `auxiliary_coverage` |
| claude | `file-history-snapshot / none` | 10 | `auxiliary_coverage` |
| claude | `fork-context-ref / none` | 11 | `auxiliary_coverage` |
| claude | `last-prompt / none` | 12 | `auxiliary_coverage` |
| claude | `launched / none` | 13 | `auxiliary_coverage` |
| claude | `mode / none` | 14 | `auxiliary_coverage` |
| claude | `permission-mode / none` | 15 | `auxiliary_coverage` |
| claude | `pr-link / none` | 16 | `auxiliary_coverage` |
| claude | `queue-operation / none` | 17 | `auxiliary_coverage` |
| claude | `relocated / none` | 18 | `auxiliary_coverage` |
| claude | `result / none` | 19 | `auxiliary_coverage` |
| claude | `started / none` | 20 | `auxiliary_coverage` |
| claude | `system / api_error` | 21 | `auxiliary_coverage` |
| claude | `system / compact_boundary` | 22 | `auxiliary_coverage` |
| claude | `system / informational` | 23 | `auxiliary_coverage` |
| claude | `system / local_command` | 24 | `auxiliary_coverage` |
| claude | `system / model_refusal_fallback` | 25 | `auxiliary_coverage` |
| claude | `system / model_refusal_no_fallback` | 26 | `auxiliary_coverage` |
| claude | `system / scheduled_task_fire` | 27 | `auxiliary_coverage` |
| claude | `system / stop_hook_summary` | 28 | `auxiliary_coverage` |
| claude | `system / turn_duration` | 29 | `auxiliary_coverage` |
| claude | `user / none` | 30 | `claude_message` |
| claude | `worktree-state / none` | 31 | `auxiliary_coverage` |
| claude | `assistant / advisor_tool_result` | 32 | `server_result_kind` |
| claude | `assistant / fallback` | 33 | `claude_message` |
| claude | `assistant / server_tool_use` | 34 | `server_call_kind` |
| claude | `assistant / text` | 35 | `claude_message` |
| claude | `assistant / thinking` | 36 | `claude_message` |
| claude | `assistant / tool_use` | 37 | `claude_message` |
| claude | `user / image` | 38 | `claude_message` |
| claude | `user / text` | 39 | `claude_message` |
| claude | `user / tool_result` | 40 | `claude_flag` |
| codex | `compacted / none` | 41 | `context_pointers` |
| codex | `event_msg / item_completed` | 42 | `codex_telemetry_coverage` |
| codex | `event_msg / task_complete` | 43 | `codex_telemetry_coverage` |
| codex | `event_msg / task_started` | 44 | `codex_telemetry_coverage` |
| codex | `event_msg / thread_goal_updated` | 45 | `codex_telemetry_coverage` |
| codex | `event_msg / thread_settings_applied` | 46 | `codex_telemetry_coverage` |
| codex | `event_msg / token_count` | 47 | `codex_telemetry_coverage` |
| codex | `event_msg / turn_aborted` | 48 | `codex_telemetry_coverage` |
| codex | `inter_agent_communication_metadata / none` | 49 | `context_pointers` |
| codex | `response_item / agent_message` | 50 | `context_pointers` |
| codex | `response_item / compaction` | 51 | `context_pointers` |
| codex | `response_item / custom_tool_call` | 52 | `json_arguments` |
| codex | `response_item / custom_tool_call_output` | 53 | `codex_text_outcome` |
| codex | `response_item / function_call` | 54 | `json_arguments` |
| codex | `response_item / function_call_output` | 55 | `codex_text_outcome` |
| codex | `response_item / message` | 56 | `context_pointers` |
| codex | `response_item / reasoning` | 57 | `context_pointers` |
| codex | `response_item / tool_search_call` | 58 | `codex_tool_search` |
| codex | `response_item / tool_search_output` | 59 | `codex_text_outcome` |
| codex | `session_meta / none` | 60 | `codex_base_instructions` |
| codex | `token_usage_record / none` | 61 | `context_pointers` |
| codex | `turn_context / none` | 62 | `codex_model` |
| codex | `world_state / none` | 63 | `context_pointers` |
| pi | `compaction / none` | 64 | `context_pointers` |
| pi | `context_edit / none` | 65 | `context_pointers` |
| pi | `custom / none` | 66 | `unclassified custom payload` |
| pi | `message / assistant` | 67 | `pi_message` |
| pi | `message / bashExecution` | 68 | `pi_message` |
| pi | `message / system` | 69 | `pi_message` |
| pi | `message / toolResult` | 70 | `pi_flag` |
| pi | `message / user` | 71 | `pi_message` |
| pi | `model_change / none` | 72 | `pi_model` |
| pi | `session / none` | 73 | `context_pointers` |
| pi | `session_info / none` | 74 | `context_pointers` |
| pi | `thinking_level_change / none` | 75 | `context_pointers` |
| pi | `message / image` | 76 | `pi_message` |
| pi | `message / text` | 77 | `pi_message` |
| pi | `message / thinking` | 78 | `pi_message` |
| pi | `message / toolCall` | 79 | `pi_call` |

Exact pointer and append/truncate/inode-replacement invariants use `incremental_append_truncate_replace_equals_rebuild_and_exact_pointers`; they are storage invariants rather than additional format aliases. `inventory_variants_are_classified_with_future_shape_negative_control` expects 79 classified variants and one opaque Pi custom variant, with unknown-future roots as negative controls. The privacy byte-search fixture covers each storage/output boundary without retaining real corpus content.

Pi message/bashExecution is a user-run record; it receives only a context pointer and is neither indexed as a logical message nor counted as an agent call.
