# Format normalization

All fixtures are synthetic. This table owns each rule's reason; code comments identify the corresponding fixture. The independent format inventory has not arrived yet, so this initial table covers the shared brief's observed variants.

| Rule | Observed variant and reason | Fixture | Regression test |
| --- | --- | --- | --- |
| Claude blocks | User messages can contain tool results; their outer user wrapper is not the result's role. | `testdata/claude.jsonl` | `claude_blocks_flags_and_synthetic_model` |
| Claude model | `message.model` changes per message; `<synthetic>` is a local-error placeholder, not a model switch. | `testdata/claude.jsonl` | `claude_blocks_flags_and_synthetic_model` |
| Claude error flag | `tool_result.is_error` gives explicit flag evidence. | `testdata/claude.jsonl` | `claude_blocks_flags_and_synthetic_model` |
| Codex context | Model metadata belongs to `turn_context`, not `session_meta`. | `testdata/codex.jsonl` | `codex_context_arguments_and_telemetry` |
| Codex arguments | Function arguments are JSON encoded inside a string. | `testdata/codex.jsonl` | `codex_context_arguments_and_telemetry` |
| Codex telemetry | `event_msg` user/agent messages mirror canonical response items and must not create another message. | `testdata/codex.jsonl` | `codex_context_arguments_and_telemetry` |
| Codex instructions | Recorded `base_instructions.text` is hashed without storing the instruction body separately. | `testdata/codex.jsonl` | `codex_context_arguments_and_telemetry` |
| Pi model | `model_change.modelId` sets the model for following messages. | `testdata/pi.jsonl` | `pi_model_tool_call_and_camel_case_flag` |
| Pi calls | Assistant content uses `toolCall`, unlike Claude's `tool_use`. | `testdata/pi.jsonl` | `pi_model_tool_call_and_camel_case_flag` |
| Pi error flag | `toolResult.isError` uses camel case and occurs on the message. | `testdata/pi.jsonl` | `pi_model_tool_call_and_camel_case_flag` |
| Exact pointers | Content blocks retain the complete source JSONL range, including the newline. | All three fixtures | `incremental_append_truncate_replace_equals_rebuild_and_exact_pointers` |
| Partial records | Incomplete trailing JSONL content is deferred; append, truncation, and inode replacement invalidate only their file's owned rows. | Runtime copies of `testdata/codex.jsonl` | `incremental_append_truncate_replace_equals_rebuild_and_exact_pointers` |

The ablation record is produced during final acceptance; a regression test's presence alone is not an ablation claim.
