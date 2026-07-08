#!/usr/bin/env bash
# shellcheck shell=bash

Include "spec/support/workflow_helpers.sh"

Describe "workflow scripts"
  AfterAll "cleanup_workflow_spec"

  It "syntax-checks shipped workflow scripts"
    When call check_shell_syntax
    The status should equal 0
  End

  It "resolves repo-root-looking selectors from app directories"
    When call test_root_selector_from_app_resolves_at_repo_root
    The status should equal 0
  End

  It "resolves local and explicit relative paths from the invocation directory"
    When call test_local_and_explicit_relative_paths_are_invocation_relative
    The status should equal 0
  End

  It "reports repo defaults for default format checks"
    When call test_default_format_summary_uses_repo_defaults
    The status should equal 0
  End

  It "reports path-selected format and lint summaries"
    When call test_path_selected_summary_uses_selector_and_pluralization
    The status should equal 0
  End

  It "formats selected Python paths with Ruff config"
    When call test_path_selected_python_uses_ruff_config
    The status should equal 0
  End

  It "formats selected TLA+ files with tla2tools formatter"
    When call test_path_selected_tla_uses_tla_formatter
    The status should equal 0
  End

  It "fails clearly for TLA+ format check mode"
    When call test_tla_format_check_fails_clearly
    The status should equal 0
  End

  It "skips selected Python paths when Ruff config is missing"
    When call test_path_selected_python_without_config_skips_ruff
    The status should equal 0
  End

  It "formats selected JS paths inside Elixir apps with oxfmt"
    When call test_elixir_app_js_path_uses_oxfmt
    The status should equal 0
  End

  It "formats selected Elixir app directories with available formatters"
    When call test_elixir_app_directory_uses_available_formatters
    The status should equal 0
  End

  It "formats directory selections through git-aware discovery"
    When call test_format_directory_discovery_uses_gitignore
    The status should equal 0
  End

  It "reports missing default formatter configs as skips"
    When call test_default_format_missing_configs_skips_without_failure
    The status should equal 0
  End

  It "reports repo defaults for default lint"
    When call test_default_lint_summary_uses_repo_defaults
    The status should equal 0
  End

  It "enforces spec placement during lint"
    When call test_lint_enforces_spec_placement
    The status should equal 0
  End

  It "lints OpenAPI specs with Vacuum"
    When call test_default_lint_runs_vacuum_for_openapi_specs
    The status should equal 0
  End

  It "lints selected Python paths with Ruff config"
    When call test_lint_selected_python_uses_ruff_config
    The status should equal 0
  End

  It "skips selected Python lint paths when Ruff config is missing"
    When call test_lint_selected_python_without_config_skips_ruff
    The status should equal 0
  End

  It "lints selected Elixir app directories with available linters"
    When call test_elixir_app_lint_directory_uses_available_linters
    The status should equal 0
  End

  It "lints selected Elixir app directories through git-aware discovery"
    When call test_elixir_app_lint_directory_discovery_uses_gitignore
    The status should equal 0
  End

  It "runs the default lint contract for the repo-root selector"
    When call test_lint_repo_root_selector_runs_default_contract
    The status should equal 0
  End

  It "accepts paths after the end-of-options marker"
    When call test_lint_accepts_end_of_options_path
    The status should equal 0
  End

  It "lints directory selections through git-aware discovery"
    When call test_lint_directory_discovery_uses_gitignore
    The status should equal 0
  End

  It "lints explicit ignored file selectors"
    When call test_lint_explicit_ignored_file_selector_wins
    The status should equal 0
  End

  It "reports lint preflight failures and continues"
    When call test_lint_preflight_failure_reports_and_continues
    The status should equal 0
  End

  It "aggregates lint results after Sobelow fails"
    When call test_lint_aggregates_after_failing_sobelow
    The status should equal 0
  End

  It "aggregates lint results after Ruff fails"
    When call test_lint_aggregates_after_failing_ruff
    The status should equal 0
  End

  It "aggregates formatter failures in check mode"
    When call test_format_check_aggregates_formatter_failures
    The status should equal 0
  End

  It "aggregates Ruff formatter failures in check mode"
    When call test_format_check_aggregates_ruff_failures
    The status should equal 0
  End

  It "checks formal specs through asdf shims"
    When call test_check_specs_uses_asdf_shims
    The status should equal 0
  End

  It "checks spec directory selections through git-aware discovery"
    When call test_check_specs_directory_discovery_uses_gitignore
    The status should equal 0
  End

  It "leaves OpenAPI linting to lint"
    When call test_check_specs_does_not_lint_openapi_specs
    The status should equal 0
  End

  It "checks TypeScript apps with compact summary"
    When call test_check_types_uses_compact_summary
    The status should equal 0
  End

  It "checks selected Python paths with Pyrefly"
    When call test_check_types_runs_pyrefly_for_selected_python
    The status should equal 0
  End

  It "checks Dialyzer for Elixir apps"
    When call test_check_dialyzer_uses_elixir_roots
    The status should equal 0
  End

  It "checks Dialyzer only for the selected Elixir app"
    When call test_check_dialyzer_selected_path_uses_only_matching_elixir_root
    The status should equal 0
  End

  It "reports Dialyzer failures"
    When call test_check_dialyzer_reports_failures
    The status should equal 0
  End

  It "checks Knip with compact summary"
    When call test_check_knip_uses_compact_summary
    The status should equal 0
  End

  It "installs project dependencies with plain install"
    When call test_install_runs_project_dependency_installs
    The status should equal 0
  End

  It "adds npm dependencies through a direct install selector"
    When call test_install_npm_dep_uses_direct_selector
    The status should equal 0
  End

  It "migrates databases for Elixir apps with migrations"
    When call test_migrate_db_runs_for_elixir_apps_with_migrations
    The status should equal 0
  End

  It "resets databases for Elixir apps with migrations"
    When call test_reset_db_runs_for_elixir_apps_with_migrations
    The status should equal 0
  End

  It "installs Hex dependencies through a temp mix.exs and compact summary"
    When call test_install_hex_dep_uses_temp_mix_exs_and_compact_summary
    The status should equal 0
  End

  It "updates managed recipes through the exported just recipe"
    When call test_update_recipes_dispatches_from_just
    The status should equal 0
  End

  It "fails clearly for unknown update targets"
    When call test_update_unknown_target_fails_clearly
    The status should equal 0
  End

  It "removes Hex dependencies and leaves parsable Elixir"
    When call test_remove_hex_dep_uses_temp_mix_exs_and_leaves_parsable_elixir
    The status should equal 0
  End

  It "runs standard app test commands"
    When call test_test_runs_standard_app_tests
    The status should equal 0
  End

  It "runs standard app build commands"
    When call test_build_runs_standard_app_builds
    The status should equal 0
  End

  It "formats shell-wrapper targets with the unmatched-pattern guard"
    When call test_shell_wrapper_format_target_uses_unmatched_pattern_guard
    The status should equal 0
  End
End
