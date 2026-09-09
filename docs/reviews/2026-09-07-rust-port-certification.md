# Rust port certification — pre-activation

Date: 2026-09-07

Status: activated. `bin/attention` defaults to Rust; the Python writer and its Python test suite are retired.

## Gate evidence

- The disposable certification copy passed `tests/gate.sh` with Rust selected by default and both Python files absent.
- Its claimed-pane reattach rehearsal published the full identity from the default shim with zero bytes on pane stdin.
- The live-linked checkout passed the same default-shim manifest check and full gate after activation.
- Final post-review gate and cleanup passed `/tmp/wezterm-attention-cleanup-final-gate.log`: 83 Rust tests, 109 Lua tests, 29 Bun tests, the frozen Python-map check, two measurement-instrument tests, shared fixtures, TypeScript, Node runtime, installed-WezTerm smoke, Markdown maps, and `git diff --check`.
- Rollback files are byte copies at `rollback-pre-activation/bin/attention` and `rollback-pre-activation/libexec/attention.py` inside the certification copy. `tests/rust/retired-python-test-inventory.json` freezes the 81 retired test names and cites the successful pre-retirement gate artifact.
- The Rust-selected `doctor --json` result reported `matches: true`; embedded and on-disk manifest SHA-256 were both `080f515bed8b3a7c639693c04c611d350ff865c9dd3e59e1562a895d6b0f2a68`.
- No WezTerm config reload command was run during activation, triage, or cleanup.

## Measurement

The instrument is `tests/rust/measure.py`. Each implementation uses its own disposable state root and pty. A run uses 100 sequential Claude `PreToolUse` processes and one concurrent burst of 20 distinct child events.

| Measure | Python | Rust |
|---|---:|---:|
| Sequential median | 57.485 ms | 6.550 ms |
| Sequential p95 | 62.146 ms | 7.039 ms |
| Sequential max | 63.741 ms | 7.249 ms |
| Concurrent burst wall time | 186.644 ms | 57.217 ms |
| Concurrent process p95 | 151.644 ms | 35.859 ms |
| Concurrent process max | 159.874 ms | 36.202 ms |
| Data files | 28 | 28 |
| Data bytes | 17,718 | 17,748 |

The first run exposed a Rust-only durability mismatch: the 20-child Rust burst took 392.255 ms. Boundary timing showed about 16 ms under the shared launch lock for every child, split across two file-plus-directory synchronization pairs. On Apple platforms, Rust 1.94 maps `File::sync_all` to `F_FULLFSYNC`, while Python calls POSIX `fsync`. The Rust writer now calls `libc::fsync`, preserving the Python durability boundary. The final burst result above is the corrected activation measurement.

After activation, the instrument requires an explicit retired Python writer and compares its SHA-256 with the explicit Rust binary before running. This command reran the comparison without restoring a production Python path:

```bash
python3 tests/rust/measure.py \
  --python-writer /Users/provi/Development/_worktrees/wezterm-attention-cert-copy/rollback-pre-activation/libexec/attention.py
```

That post-activation run identified Python as `5f143756...` and Rust as `bc876ec8...`; sequential p95 was 48.427 ms versus 3.107 ms, and concurrent wall time was 169.161 ms versus 49.472 ms. The instrument refuses same-artifact identities.

The sequential p95 criterion passes, and Rust is faster in the concurrent burst. The file count is unchanged. The exact byte count is 30 bytes larger only in the flat `42` compatibility marker because Rust writes the required `updated_at_ms` field that the Python baseline omits. The measurement payload omits `agent_type`, so it does not resolve or conceal the pending `subagent_presence.source` decision.

## Python test disposition

The inventory contains every 81 test methods frozen from `tests/attention_cli_test.py`: 76 retained, 4 changed, and 1 removed. `tests/rust/check_python_test_map.py` now validates the disposition map against `tests/rust/retired-python-test-inventory.json` on every gate run.

| Python test | Disposition | Rust-era proof or recorded cut |
|---|---|---|
| `AttentionWriterTest.test_claim_is_private_durable_and_published` | retained | `claim_is_private_durable_and_published` |
| `AttentionWriterTest.test_duplicate_claim_republishes_without_rewrite` | retained | `duplicate_claim_republishes_without_rewrite` |
| `AttentionWriterTest.test_next_launch_rotates_claim_path_identity` | retained | `delayed_older_claim_loses` |
| `AttentionWriterTest.test_unbound_mark_writes_v2_and_v1_then_duplicate_repairs_projection` | changed | `manual_mark_targets_the_launch_and_duplicate_repairs_legacy_projection`; v1 projection also gains updated_at_ms per O8 |
| `AttentionWriterTest.test_older_activity_cannot_replace_newer` | retained | `fresh_manual_activity_is_fenced_by_an_existing_clear` and monotonic commit guards |
| `AttentionWriterTest.test_delayed_older_process_loses_to_newer_process` | retained | `stopped_child_fences_an_older_inflight_tool_event` |
| `AttentionWriterTest.test_delayed_older_claim_cannot_replace_newer_launch` | retained | `delayed_older_claim_loses` |
| `AttentionWriterTest.test_old_launch_writer_cannot_replace_current_v1_projection` | retained | `manual_mark_targets_the_launch_and_duplicate_repairs_legacy_projection` |
| `AttentionWriterTest.test_mark_targets_valid_current_binding` | retained | `manual_mark_targets_the_launch_and_duplicate_repairs_legacy_projection` |
| `AttentionWriterTest.test_relative_state_root_is_rejected_before_write` | retained | `shared_protocol_rows_match_the_independent_checker_verdicts` |
| `AttentionWriterTest.test_lock_contention_is_bounded_and_diagnosed` | retained | `lock_contention_is_bounded_and_diagnosed` |
| `AttentionWriterTest.test_realm_publish_is_v2_only_for_matching_claim_and_tty` | retained | `realm_publish_skips_only_the_row_without_a_tty` |
| `AttentionWriterTest.test_unsafe_tty_skips_only_that_pane` | retained | `realm_publish_skips_only_the_row_without_a_tty` |
| `AttentionWriterTest.test_source_review_does_not_replace_user_review` | retained | `every_review_writer_obeys_claim_and_owner_locks` |
| `AttentionWriterTest.test_future_and_invalid_existing_records_are_never_mutated` | retained | `shared_protocol_rows_match_the_independent_checker_verdicts` and `unknown_binding_file_and_future_child_are_preserved` |
| `AttentionWriterTest.test_future_claim_is_not_replaced` | retained | `shared_protocol_rows_match_the_independent_checker_verdicts` |
| `AttentionWriterTest.test_malformed_pointer_is_not_treated_as_unbound` | retained | `bindings_query_rejects_foreign_end_pointer_and_claim_records` |
| `AttentionWriterTest.test_invalid_outgoing_frame_is_rejected_before_write` | retained | `shared_protocol_rows_match_the_independent_checker_verdicts` |
| `AttentionWriterTest.test_future_review_is_not_deleted` | retained | `future_review_is_never_deleted_or_replaced` |
| `AttentionWriterTest.test_future_review_is_not_replaced` | retained | `future_review_is_never_deleted_or_replaced` |
| `AttentionWriterTest.test_future_realm_manifest_is_not_replaced` | retained | `shared_protocol_rows_match_the_independent_checker_verdicts` |
| `AttentionWriterTest.test_production_validator_matches_shared_record_cases` | retained | `shared_protocol_rows_match_the_independent_checker_verdicts` |
| `AttentionWriterTest.test_opened_tty_descriptor_is_revalidated` | retained | `opened_tty_descriptor_is_revalidated` |
| `AttentionWriterTest.test_provider_fixture_matrices_are_exhaustive_and_parse_as_declared` | retained | `provider_fixtures_equal_the_closed_action_vocabulary` |
| `AttentionWriterTest.test_provider_supersede_matrix_initial_confirm_conflict_and_replace` | retained | `nested_startup_conflicts_and_nested_resume_replaces` |
| `AttentionWriterTest.test_child_stop_active_stop_order_and_v1_projection` | retained | `stopped_child_fences_an_older_inflight_tool_event` |
| `AttentionWriterTest.test_stop_for_absent_agent_fences_delayed_first_write_and_duplicate_repairs_v1` | retained | `stopped_child_fences_an_older_inflight_tool_event` |
| `AttentionWriterTest.test_subagent_stop_preserves_sibling_and_parent_clear_allows_newer_work` | retained | `codex_parent_stop_clears_children_with_the_same_observation` |
| `AttentionWriterTest.test_parent_stop_with_no_agents_still_writes_one_clear_watermark` | retained | `codex_parent_stop_clears_children_with_the_same_observation` |
| `AttentionWriterTest.test_binding_keeps_optional_expected_id_and_accepts_null_model` | retained | `provider_fixtures_equal_the_closed_action_vocabulary` |
| `AttentionWriterTest.test_duplicate_subagent_stop_repairs_without_sampling_unix_time` | retained | `stopped_child_fences_an_older_inflight_tool_event` |
| `AttentionWriterTest.test_older_inflight_child_tool_call_cannot_reactivate_stopped_presence` | retained | `stopped_child_fences_an_older_inflight_tool_event` |
| `AttentionWriterTest.test_two_subagents_write_concurrently_without_losing_projection_members` | retained | `every_review_writer_obeys_claim_and_owner_locks` plus shared nested lock path |
| `AttentionWriterTest.test_future_child_and_activity_clear_records_are_never_replaced` | retained | `unknown_binding_file_and_future_child_are_preserved` |
| `AttentionWriterTest.test_same_agent_tool_call_refreshes_ttl_without_changing_count` | retained | `stopped_child_fences_an_older_inflight_tool_event` |
| `AttentionWriterTest.test_stale_binding_child_event_never_changes_current_v1_projection` | retained | `stopped_child_fences_an_older_inflight_tool_event` |
| `AttentionWriterTest.test_matching_and_stale_session_end_write_only_exact_binding` | retained | `same_session_resume_reopens_an_older_end` |
| `AttentionWriterTest.test_same_session_resume_reopens_older_end_and_accepts_a_new_end` | retained | `same_session_resume_reopens_an_older_end` |
| `AttentionWriterTest.test_pi_bus_review_is_owner_isolated_and_agent_end_is_inert` | retained | `pi_review_and_clear_share_the_current_binding_without_ending_it` |
| `AttentionWriterTest.test_pi_bus_clear_fences_activity_repairs_v1_and_clears_its_review` | retained | `stale_pi_clear_preserves_newer_activity_and_review` |
| `AttentionWriterTest.test_equal_pi_bus_clear_repairs_projection_without_new_v2_event` | retained | `stale_pi_clear_preserves_newer_activity_and_review` |
| `AttentionWriterTest.test_hook_captures_monotonic_time_before_reading_stdin` | retained | `hook_observation_is_captured_before_waiting_for_stdin` |
| `AttentionWriterTest.test_real_cli_provider_dispatch_writes_binding_with_debug_only_on_stderr` | retained | `hooks_event_debug_uses_stderr_and_lifecycle_errors_are_non_strict` |
| `AttentionWriterTest.test_bindings_returns_validated_axes_without_resume_commands` | retained | `bindings_query_returns_all_identity_axes_and_rejects_path_mismatch` |
| `AttentionWriterTest.test_duplicate_provider_session_bindings_are_both_conflicted` | retained | `bindings_query_returns_all_identity_axes_and_rejects_path_mismatch`; duplicate health is derived by the same query pass |
| `AttentionWriterTest.test_doctor_names_cli_scope_and_unobserved_gui_vars` | retained | `doctor_reports_embedded_manifest_digest_and_confirmed_binding`; CLI-only scope labels are presentation checked by the disposable gate |
| `AttentionWriterTest.test_doctor_contains_process_failure_and_reports_unsafe_permissions` | retained | `doctor_rejects_a_valid_record_at_the_wrong_depth` and `unavailable_process_probe_never_counts_as_absence` |
| `AttentionWriterTest.test_sweep_preview_writes_nothing_and_two_monotonic_absences_end_exact_binding` | retained | `sweep_preview_writes_nothing_and_two_absences_end_one_binding` |
| `AttentionWriterTest.test_live_claim_clears_first_absence_probe` | retained | `live_pane_clears_the_first_absence_probe` |
| `AttentionWriterTest.test_subagent_compaction_advances_floor_before_delete_and_replays_same_operation` | retained | `compaction_advances_floor_before_delete_and_replays_operation` |
| `AttentionWriterTest.test_partial_compaction_retry_keeps_floor_and_finishes_covered_delete` | retained | `compaction_advances_floor_before_delete_and_replays_operation` |
| `AttentionWriterTest.test_compaction_apply_revalidates_a_reactivated_child` | retained | `compaction_apply_revalidates_reactivated_child` |
| `AttentionWriterTest.test_eligible_active_child_blocks_unsafe_subagent_cap_prune` | retained | `equal_order_active_child_blocks_the_whole_cap_group` |
| `AttentionWriterTest.test_binding_retention_uses_end_written_unix_time_and_never_prunes_current` | retained | `old_noncurrent_binding_is_pruned_but_current_binding_is_preserved` |
| `AttentionWriterTest.test_sweep_preserves_untrustworthy_age_future_schema_and_unknown_files` | retained | `unknown_binding_file_and_future_child_are_preserved` and `retention_preserves_unknown_files_inside_an_old_binding` |
| `AttentionWriterTest.test_sweep_negative_wall_age_reports_clock_skew_and_never_prunes` | retained | `negative_wall_age_reports_clock_skew_and_preserves_child` |
| `AttentionWriterTest.test_binding_retention_preserves_schema_valid_mismatched_child_record` | retained | `foreign_retention_floor_never_deletes_children` |
| `AttentionWriterTest.test_delayed_resume_cannot_replace_newer_current_binding` | retained | `sweep_apply_uses_the_binding_reread_after_selection` |
| `AttentionWriterTest.test_compact_cannot_replace_a_reopened_current_binding` | retained | `sweep_apply_uses_the_binding_reread_after_selection` |
| `AttentionWriterTest.test_manual_activity_after_clear_mints_a_new_visible_event` | retained | `fresh_manual_activity_is_fenced_by_an_existing_clear` |
| `AttentionWriterTest.test_equal_order_active_child_blocks_floor_for_whole_group` | retained | `equal_order_active_child_blocks_the_whole_cap_group` |
| `AttentionWriterTest.test_binding_history_cap_is_calculated_per_realm` | retained | `binding_history_cap_is_calculated_per_realm` |
| `AttentionWriterTest.test_old_launch_pointer_is_not_current_after_claim_rotation` | retained | `bindings_query_rejects_foreign_end_pointer_and_claim_records` |
| `AttentionWriterTest.test_old_launch_history_can_expire_after_claim_rotation` | retained | `old_noncurrent_binding_is_pruned_but_current_binding_is_preserved` |
| `AttentionWriterTest.test_absence_requires_identity_scoped_process_negative` | retained | `absence_process_negative_is_scoped_to_socket_and_pane` |
| `AttentionWriterTest.test_doctor_reports_future_claim_without_any_binding` | retained | `doctor_reports_a_future_claim_without_any_binding` |
| `CliShapeTest.test_system_clock_orders_separate_writer_processes` | changed | `hook_observation_is_captured_before_waiting_for_stdin`; Rust persists CLOCK_MONOTONIC_RAW rather than importing a Python clock in two processes |
| `CliShapeTest.test_help_and_obsolete_commands` | retained | `rust_cli_help_errors_and_empty_hook_input_keep_the_documented_shape` |
| `CliShapeTest.test_wrapper_reports_missing_python_without_traceback` | removed | cut — the activated shim has no Python branch; the replacement missing-binary error is certified in the disposable activation copy |
| `CliShapeTest.test_zsh_manual_claim_rotates_each_explicit_launch` | changed | `zsh_explicit_claim_uses_selected_ids_and_clears_inherited_id_on_failure`; Rust now mints the ID |
| `CliShapeTest.test_zsh_claim_failure_clears_an_inherited_launch` | retained | `zsh_explicit_claim_uses_selected_ids_and_clears_inherited_id_on_failure` |
| `CliShapeTest.test_bash_keeps_a_committed_launch_when_publication_is_pending` | retained | `bash_automatic_claim_preserves_debug_trap_and_keeps_pending_publication_id` |
| `CliShapeTest.test_bash_debug_hook_claims_one_supported_command` | changed | `bash_automatic_claim_preserves_debug_trap_and_keeps_pending_publication_id`; Rust now mints the ID |
| `CliShapeTest.test_bash_debug_hook_handles_assignments_and_preserves_quoted_trap` | retained | `bash_automatic_claim_preserves_debug_trap_and_keeps_pending_publication_id` |
| `CliShapeTest.test_json_operational_error_is_structured_exit_three` | retained | `rust_cli_help_errors_and_empty_hook_input_keep_the_documented_shape` |
| `CliShapeTest.test_publication_details_are_bounded_unless_explicit` | retained | `json_publication_and_binding_output_report_bounded_completeness` |
| `CliShapeTest.test_provider_hook_default_strict_and_debug_contracts` | retained | `hooks_event_debug_uses_stderr_and_lifecycle_errors_are_non_strict` |
| `CliShapeTest.test_provider_hook_empty_stdin_never_waits_and_strict_reports_usage` | retained | `rust_cli_help_errors_and_empty_hook_input_keep_the_documented_shape` |
| `CliShapeTest.test_provider_hook_help_names_subagent_stop_but_not_subagent_start` | retained | `rust_cli_help_errors_and_empty_hook_input_keep_the_documented_shape` |
| `CliShapeTest.test_bindings_json_is_bounded_and_complete_reports_truncation` | retained | `json_publication_and_binding_output_report_bounded_completeness` |
| `CliShapeTest.test_documentation_uses_the_command_as_sole_v2_writer` | retained | `tests/gate.sh` production-writer and Markdown checks |

## Activation

The user accepted the corrected measurements and explicitly authorized activation. The certified Rust-default shim, no-Python gate, documentation, and surviving CLI tests were moved into the live-linked checkout as one activation bundle. The final live checkout gate passed.