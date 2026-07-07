#!/usr/bin/env bash
# shellcheck shell=bash

JUST_BUNDLE_ROOT="${SHELLSPEC_PROJECT_ROOT:-$(pwd)}"
DOT_JUST_ROOT="$JUST_BUNDLE_ROOT/dot-just"
RUN_TMP=""

ensure_run_tmp() {
  local tmp_base

  if [[ -n "${RUN_TMP:-}" ]]; then
    return 0
  fi

  tmp_base="${TMPDIR:-/tmp}"
  tmp_base="${tmp_base%/}"
  RUN_TMP="$(mktemp -d "$tmp_base/workflow-wrapper-tests.XXXXXX")"
  RUN_TMP="$(cd "$RUN_TMP" && pwd)"
}

cleanup_workflow_spec() {
  if [[ -n "${RUN_TMP:-}" ]]; then
    rm -rf "$RUN_TMP"
  fi
}

check_shell_syntax() {
  local script

  while IFS= read -r script; do
    bash -n "$script"
  done < <(find "$DOT_JUST_ROOT" -type f ! -name '*.exs' ! -name '*.just' | sort)
}

fail() {
  printf "error: %s\n" "$*" >&2
  return 1
}

print_file() {
  local file="$1"

  sed 's/^/    /' "$file" >&2
}

assert_status() {
  local expected="$1"
  local actual="$2"
  local output="$3"

  if [[ "$actual" -ne "$expected" ]]; then
    printf "expected exit %s, got %s\n" "$expected" "$actual" >&2
    print_file "$output"
    return 1
  fi
}

assert_nonzero_status() {
  local actual="$1"
  local output="$2"

  if [[ "$actual" -eq 0 ]]; then
    printf "expected nonzero exit, got 0\n" >&2
    print_file "$output"
    return 1
  fi
}

assert_output_contains() {
  local output="$1"
  local expected="$2"

  if ! grep -Fq -- "$expected" "$output"; then
    printf "expected output to contain: %s\n" "$expected" >&2
    print_file "$output"
    return 1
  fi
}

assert_output_not_contains() {
  local output="$1"
  local unexpected="$2"

  if grep -Fq -- "$unexpected" "$output"; then
    printf "expected output not to contain: %s\n" "$unexpected" >&2
    print_file "$output"
    return 1
  fi
}

assert_output_matches() {
  local output="$1"
  local pattern="$2"

  if ! grep -Eq -- "$pattern" "$output"; then
    printf "expected output to match regex: %s\n" "$pattern" >&2
    print_file "$output"
    return 1
  fi
}

assert_output_before() {
  local output="$1"
  local first="$2"
  local second="$3"
  local first_line
  local second_line

  first_line="$(grep -Fn -- "$first" "$output" | head -n 1 | cut -d: -f1)"
  second_line="$(grep -Fn -- "$second" "$output" | head -n 1 | cut -d: -f1)"

  if [[ -z "$first_line" || -z "$second_line" || "$first_line" -ge "$second_line" ]]; then
    printf "expected output to contain '%s' before '%s'\n" "$first" "$second" >&2
    print_file "$output"
    return 1
  fi
}

assert_file_contains() {
  local file="$1"
  local expected="$2"

  if ! grep -Fq -- "$expected" "$file"; then
    printf "expected %s to contain: %s\n" "$file" "$expected" >&2
    print_file "$file"
    return 1
  fi
}

assert_file_not_contains() {
  local file="$1"
  local unexpected="$2"

  if grep -Fq -- "$unexpected" "$file"; then
    printf "expected %s not to contain: %s\n" "$file" "$unexpected" >&2
    print_file "$file"
    return 1
  fi
}

log_line() {
  local tool="$1"
  local cwd="$2"
  local arg

  shift 2
  printf "%s\t%s" "$tool" "$cwd"

  for arg in "$@"; do
    printf "\t%s" "$arg"
  done
}

assert_log_entry() {
  local fixture="$1"
  local expected

  shift
  expected="$(log_line "$@")"

  if ! grep -Fxq -- "$expected" "$fixture/fake.log"; then
    printf "expected command log entry:\n  %s\nactual log:\n" "$expected" >&2
    print_file "$fixture/fake.log"
    return 1
  fi
}

assert_log_not_contains() {
  local fixture="$1"
  local unexpected="$2"

  if grep -Fq -- "$unexpected" "$fixture/fake.log"; then
    printf "expected command log not to contain: %s\nactual log:\n" "$unexpected" >&2
    print_file "$fixture/fake.log"
    return 1
  fi
}

assert_log_equals() {
  local fixture="$1"
  local expected="$2"

  if ! diff -u "$expected" "$fixture/fake.log" >&2; then
    return 1
  fi
}

write_mix_exs() {
  local mix_exs="$1"
  local include_uniq="${2:-0}"

  if [[ "$include_uniq" -eq 1 ]]; then
    cat >"$mix_exs" <<'ELIXIR'
defmodule Api.MixProject do
  use Mix.Project

  def project do
    [
      app: :api,
      version: "0.1.0",
      deps: deps()
    ]
  end

    def application, do: []

    defp deps do
      [
        {:ex_slop, "0.4.2", only: [:dev, :test], runtime: false},
        {:excellent_migrations, "~> 0.1.10", only: [:dev, :test], runtime: false},
        {:code_style, path: "../../../../code_style", only: [:dev, :test], runtime: false},
        {:styler, "~> 1.5", only: [:dev, :test], runtime: false},
        {:phoenix, "~> 1.8"},
        {:uniq, "0.6.3"}
      ]
    end
  end
ELIXIR
  else
    cat >"$mix_exs" <<'ELIXIR'
defmodule Api.MixProject do
  use Mix.Project

  def project do
    [
      app: :api,
      version: "0.1.0",
      deps: deps()
    ]
  end

    def application, do: []

    defp deps do
      [
        {:ex_slop, "0.4.2", only: [:dev, :test], runtime: false},
        {:excellent_migrations, "~> 0.1.10", only: [:dev, :test], runtime: false},
        {:code_style, path: "../../../../code_style", only: [:dev, :test], runtime: false},
        {:styler, "~> 1.5", only: [:dev, :test], runtime: false},
        {:phoenix, "~> 1.8"}
      ]
    end
  end
ELIXIR
  fi
}

write_fake_command() {
  local path="$1"
  local body="$2"

  {
    printf "#!/usr/bin/env bash\n"
    printf "set -u\n"
    printf "%s\n" "$body"
  } >"$path"

  chmod +x "$path"
}

new_fixture() {
  local fixture
  local fake_bin

  ensure_run_tmp

  fixture="$(mktemp -d "$RUN_TMP/fixture.XXXXXX")"
  fixture="$(cd "$fixture" && pwd)"
  fake_bin="$fixture/fake-bin"

  mkdir -p \
    "$fake_bin" \
    "$fixture/.vscode" \
    "$fixture/apps/api/assets/css" \
    "$fixture/apps/api/lib" \
    "$fixture/apps/workspace/src" \
    "$fixture/apps/website/src" \
    "$fixture/.just/bin" \
    "$fixture/.just/commands" \
    "$fixture/.just/lib"

  cat >"$fixture/just.yaml" <<'YAML'
apps:
  api:
    path: apps/api
  workspace:
    path: apps/workspace
  website:
    path: apps/website

tools:
  tla: "1.8.0"
  alloy: "6.2.0"
YAML

  printf "{}\n" >"$fixture/package.json"
  printf "packages:\n  - apps/*\n" >"$fixture/pnpm-workspace.yaml"
  printf "export default {}\n" >"$fixture/oxlint.config.ts"
  printf "export default {}\n" >"$fixture/stylelint.config.js"
  printf "{}\n" >"$fixture/knip.json"
  printf "processes: {}\n" >"$fixture/process-compose.yaml"
  printf "{}\n" >"$fixture/.oxfmtrc.json"
  printf "root = true\n" >"$fixture/.editorconfig"
  printf "{}\n" >"$fixture/.vscode/settings.json"
  printf "default:\n" >"$fixture/Justfile"

  printf "body { color: black; }\n" >"$fixture/apps/api/assets/css/app.css"
  cat >"$fixture/apps/api/.credo.exs" <<'ELIXIR'
%{
  configs: [
    %{
      name: "default",
      plugins: [{ExSlop, []}],
      checks: [
        {ExcellentMigrations.CredoCheck.MigrationsSafety, []},
        {CodeStyle.Check.Design.NoDatabaseConstraints, []}
      ]
    }
  ]
}
ELIXIR
  printf "[plugins: [Phoenix.LiveView.HTMLFormatter, Styler]]\n" >"$fixture/apps/api/.formatter.exs"
  printf "defmodule Api do\nend\n" >"$fixture/apps/api/lib/api.ex"
  write_mix_exs "$fixture/apps/api/mix.exs"
  printf '{"scripts":{"build":"vite build","test":"vitest run","typecheck":"tsgo --noEmit"}}\n' >"$fixture/apps/workspace/package.json"
  printf "export const main = 1\n" >"$fixture/apps/workspace/src/main.tsx"
  printf "export const secondary = 2\n" >"$fixture/apps/workspace/src/secondary.ts"
  printf '{"scripts":{"build":"astro build","typecheck":"astro check"}}\n' >"$fixture/apps/website/package.json"
  printf "export const site = 3\n" >"$fixture/apps/website/src/site.ts"

  cp "$DOT_JUST_ROOT/lib/install_hex_dep.exs" "$fixture/.just/lib/install_hex_dep.exs"
  cp "$DOT_JUST_ROOT/lib/remove_hex_dep.exs" "$fixture/.just/lib/remove_hex_dep.exs"

  touch \
    "$fixture/.just/bin/workflow" \
    "$fixture/.just/commands/check" \
    "$fixture/.just/commands/build" \
    "$fixture/.just/commands/dev" \
    "$fixture/.just/commands/format" \
    "$fixture/.just/commands/help" \
    "$fixture/.just/commands/install" \
    "$fixture/.just/commands/lint" \
    "$fixture/.just/commands/migrate" \
    "$fixture/.just/commands/remove" \
    "$fixture/.just/commands/reset" \
    "$fixture/.just/commands/test" \
    "$fixture/.just/lib/lib.sh"

  # shellcheck disable=SC2016
  write_fake_command "$fake_bin/pnpm" '
printf "pnpm\t%s" "$PWD" >> "$JUST_FAKE_LOG"
for arg in "$@"; do
  printf "\t%s" "$arg" >> "$JUST_FAKE_LOG"
done
printf "\n" >> "$JUST_FAKE_LOG"

if [[ "${1:-}" == "exec" && "${2:-}" == "oxfmt" ]]; then
  if [[ "${JUST_FAKE_PNPM_FAIL_OXFMT:-0}" != "0" ]]; then
    exit "$JUST_FAKE_PNPM_FAIL_OXFMT"
  fi

  if [[ "${JUST_FAKE_PNPM_UNSUPPORTED_OXFMT_FAIL:-0}" != "0" ]]; then
    for arg in "$@"; do
      if [[ "$arg" == "--no-error-on-unmatched-pattern" ]]; then
        exit 0
      fi
    done

    exit "$JUST_FAKE_PNPM_UNSUPPORTED_OXFMT_FAIL"
  fi
fi

if [[ "${1:-}" == "exec" && "${2:-}" == "oxlint" && "${JUST_FAKE_PNPM_FAIL_OXLINT:-0}" != "0" ]]; then
  exit "$JUST_FAKE_PNPM_FAIL_OXLINT"
fi

if [[ "${1:-}" == "exec" && "${2:-}" == "stylelint" && "${JUST_FAKE_PNPM_FAIL_STYLELINT:-0}" != "0" ]]; then
  exit "$JUST_FAKE_PNPM_FAIL_STYLELINT"
fi

exit 0
'

  # shellcheck disable=SC2016
  write_fake_command "$fake_bin/mix" '
printf "mix\t%s" "$PWD" >> "$JUST_FAKE_LOG"
for arg in "$@"; do
  printf "\t%s" "$arg" >> "$JUST_FAKE_LOG"
done
printf "\n" >> "$JUST_FAKE_LOG"

case "$*" in
  "format --check-formatted"*)
    exit "${JUST_FAKE_MIX_FORMAT_CHECK_STATUS:-0}"
    ;;
  "sobelow --private --exit --threshold high")
    exit "${JUST_FAKE_MIX_SOBELOW_STATUS:-0}"
    ;;
esac

exit 0
'

  # shellcheck disable=SC2016
  write_fake_command "$fake_bin/process-compose" '
printf "process-compose\t%s" "$PWD" >> "$JUST_FAKE_LOG"
for arg in "$@"; do
  printf "\t%s" "$arg" >> "$JUST_FAKE_LOG"
done
printf "\n" >> "$JUST_FAKE_LOG"
exit "${JUST_FAKE_PROCESS_COMPOSE_STATUS:-0}"
'

  : >"$fixture/fake.log"
  printf "%s\n" "$fixture"
}

run_wrapper() {
  local __status_var="$1"
  local __output_var="$2"
  local fixture="$3"
  local caller="$4"
  local command="$5"
  local __captured_output
  local __captured_status

  shift 5
  __captured_output="$(mktemp "$fixture/output.$command.XXXXXX")"

  set +e
  (
    cd "$caller" &&
      env \
        PATH="$fixture/fake-bin:$PATH" \
        REPO_ROOT="$fixture" \
        JUST_CONFIG="$fixture/just.yaml" \
        JUST_CALLER_DIR="$caller" \
        JUST_FAKE_LOG="$fixture/fake.log" \
        JUST_FAKE_MIX_FORMAT_CHECK_STATUS="${JUST_FAKE_MIX_FORMAT_CHECK_STATUS:-0}" \
        JUST_FAKE_MIX_SOBELOW_STATUS="${JUST_FAKE_MIX_SOBELOW_STATUS:-0}" \
        JUST_FAKE_PNPM_FAIL_OXFMT="${JUST_FAKE_PNPM_FAIL_OXFMT:-0}" \
        JUST_FAKE_PNPM_FAIL_OXLINT="${JUST_FAKE_PNPM_FAIL_OXLINT:-0}" \
        JUST_FAKE_PNPM_FAIL_STYLELINT="${JUST_FAKE_PNPM_FAIL_STYLELINT:-0}" \
        JUST_FAKE_PNPM_UNSUPPORTED_OXFMT_FAIL="${JUST_FAKE_PNPM_UNSUPPORTED_OXFMT_FAIL:-0}" \
        JUST_FAKE_PROCESS_COMPOSE_STATUS="${JUST_FAKE_PROCESS_COMPOSE_STATUS:-0}" \
        bash "$DOT_JUST_ROOT/commands/$command" "$@"
  ) >"$__captured_output" 2>&1
  __captured_status="$?"
  set -e

  printf -v "$__status_var" "%s" "$__captured_status"
  printf -v "$__output_var" "%s" "$__captured_output"
}

parse_elixir_file() {
  local file="$1"

  elixir -e 'Code.string_to_quoted!(File.read!(List.first(System.argv())))' "$file" >/dev/null
}

test_root_selector_from_app_resolves_at_repo_root() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture/apps/api" format apps/workspace/src/main.tsx

  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxfmt --no-error-on-unmatched-pattern apps/workspace/src/main.tsx || return
  assert_log_not_contains "$fixture" "apps/api/apps/workspace" || return
}

test_local_and_explicit_relative_paths_are_invocation_relative() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture/apps/api" format mix.exs
  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" format mix.exs || return

  : >"$fixture/fake.log"
  run_wrapper status output "$fixture" "$fixture/apps/api" format ./mix.exs
  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" format mix.exs || return

  : >"$fixture/fake.log"
  run_wrapper status output "$fixture" "$fixture/apps/api" format ../workspace/src/main.tsx
  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxfmt --no-error-on-unmatched-pattern apps/workspace/src/main.tsx || return
}

test_default_format_summary_uses_repo_defaults() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" format --check

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "cmd: pnpm exec oxfmt --check" || return
  assert_output_matches "$output" "files: repo defaults \\([0-9]+ files\\)" || return
}

test_path_selected_summary_uses_selector_and_pluralization() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" format --check apps/workspace/src/main.tsx
  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "cmd: pnpm exec oxfmt --check apps/workspace/src/main.tsx" || return
  assert_output_contains "$output" "files: apps/workspace/src/main.tsx (1 file)" || return

  run_wrapper status output "$fixture" "$fixture" lint apps/workspace/src
  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "pnpm exec oxlint apps/workspace/src" || return
  assert_output_contains "$output" "apps/workspace/src (2 files)" || return
}

test_default_lint_summary_uses_repo_defaults() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" lint

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "Check" || return
  assert_output_contains "$output" "Requirement" || return
  assert_output_contains "$output" "Linter" || return
  assert_output_contains "$output" "Files" || return
  assert_output_before "$output" "Check" "Linter" || return
  assert_output_matches "$output" "repo defaults \\([0-9]+ files\\)" || return
  assert_output_not_contains "$output" "default lint paths" || return
}

test_lint_repo_root_selector_runs_default_contract() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" lint .

  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" credo --strict || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxlint apps/workspace/src/main.tsx apps/workspace/src/secondary.ts apps/website/src/site.ts oxlint.config.ts stylelint.config.js || return
  assert_log_entry "$fixture" pnpm "$fixture" exec stylelint apps/api/assets/css/app.css || return
}

test_lint_accepts_end_of_options_path() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  printf "export const optionPath = 1\n" >"$fixture/-x.js"

  run_wrapper status output "$fixture" "$fixture" lint -- -x.js

  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxlint ./-x.js || return
  assert_output_not_contains "$output" "path does not exist" || return
}

test_lint_preflight_failure_reports_and_continues() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  printf "[]\n" >"$fixture/apps/api/.formatter.exs"

  run_wrapper status output "$fixture" "$fixture" lint apps/api

  assert_nonzero_status "$status" "$output" || return
  assert_output_matches "$output" "Styler Formatter[[:space:]]+failed\\(1\\)" || return
  assert_output_matches "$output" "Credo[[:space:]]+passed" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" credo --strict || return
}

test_lint_aggregates_after_failing_sobelow() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  JUST_FAKE_MIX_SOBELOW_STATUS=44 run_wrapper status output "$fixture" "$fixture" lint

  assert_nonzero_status "$status" "$output" || return
  assert_output_matches "$output" "Sobelow[[:space:]]+failed\\(44\\)" || return
  assert_output_matches "$output" "ExSlop Dependency[[:space:]]+passed" || return
  assert_output_matches "$output" "ExSlop Credo Plugin[[:space:]]+passed" || return
  assert_output_matches "$output" "ExcellentMigrations Dependency[[:space:]]+passed" || return
  assert_output_matches "$output" "CodeStyle Dependency[[:space:]]+passed" || return
  assert_output_matches "$output" "Styler Formatter[[:space:]]+passed" || return
  assert_output_matches "$output" "ExDNA[[:space:]]+passed" || return
  assert_output_matches "$output" "Reach[[:space:]]+passed" || return
  assert_output_matches "$output" "Oxlint[[:space:]]+passed" || return
  assert_output_matches "$output" "Stylelint[[:space:]]+passed" || return
  assert_output_not_contains "$output" "Dialyzer" || return
  assert_log_not_contains "$fixture" "ex_slop" || return
  assert_log_not_contains "$fixture" "dialyzer" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" reach.check --arch --smells --strict || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxlint apps/workspace/src/main.tsx apps/workspace/src/secondary.ts apps/website/src/site.ts oxlint.config.ts stylelint.config.js || return
  assert_log_entry "$fixture" pnpm "$fixture" exec stylelint apps/api/assets/css/app.css || return
}

test_format_check_aggregates_formatter_failures() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  JUST_FAKE_PNPM_FAIL_OXFMT=31 run_wrapper status output "$fixture" "$fixture" format --check apps/workspace/src/main.tsx apps/api/mix.exs

  assert_nonzero_status "$status" "$output" || return
  assert_output_contains "$output" "[failed exit 31] Oxfmt Check" || return
  assert_output_contains "$output" "[ok] Mix Format Check" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" format --check-formatted mix.exs || return
}

test_install_hex_dep_uses_temp_mix_exs_and_compact_summary() {
  local fixture
  local output
  local status
  local mix_exs
  local expected_log

  fixture="$(new_fixture)"
  mix_exs="$fixture/apps/api/mix.exs"
  expected_log="$(mktemp "$fixture/expected.log.XXXXXX")"

  run_wrapper status output "$fixture" "$fixture" install deps hex:uniq 0.6.3 --app api

  assert_status 0 "$status" "$output" || return
  assert_file_contains "$mix_exs" "{:uniq, \"0.6.3\"}" || return
  assert_output_contains "$output" "[ok] Add Hex Dependency" || return
  assert_output_contains "$output" "cmd: elixir ../../.just/lib/install_hex_dep.exs mix.exs uniq 0.6.3" || return
  assert_output_contains "$output" "app: api" || return
  assert_output_contains "$output" "package: hex:uniq" || return
  assert_output_contains "$output" "version: 0.6.3" || return
  {
    log_line mix "$fixture/apps/api" format mix.exs
    printf "\n"
    log_line mix "$fixture/apps/api" deps.get
    printf "\n"
  } >"$expected_log"
  assert_log_equals "$fixture" "$expected_log" || return
}

test_remove_hex_dep_uses_temp_mix_exs_and_leaves_parsable_elixir() {
  local fixture
  local output
  local status
  local mix_exs
  local expected_log

  fixture="$(new_fixture)"
  mix_exs="$fixture/apps/api/mix.exs"
  expected_log="$(mktemp "$fixture/expected.log.XXXXXX")"
  write_mix_exs "$mix_exs" 1

  run_wrapper status output "$fixture" "$fixture" remove deps hex:uniq --app api

  assert_status 0 "$status" "$output" || return
  assert_file_not_contains "$mix_exs" ":uniq" || return
  parse_elixir_file "$mix_exs" || return
  assert_output_contains "$output" "[ok] Remove Hex Dependency" || return
  assert_output_contains "$output" "cmd: elixir ../../.just/lib/remove_hex_dep.exs mix.exs uniq" || return
  assert_output_contains "$output" "app: api" || return
  assert_output_contains "$output" "package: hex:uniq" || return
  {
    log_line mix "$fixture/apps/api" format mix.exs
    printf "\n"
    log_line mix "$fixture/apps/api" deps.clean uniq --unlock
    printf "\n"
    log_line mix "$fixture/apps/api" deps.unlock --unused
    printf "\n"
  } >"$expected_log"
  assert_log_equals "$fixture" "$expected_log" || return
}

test_test_runs_standard_app_tests() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" test

  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" test || return
  assert_log_entry "$fixture" pnpm "$fixture/apps/workspace" run test || return
  assert_log_not_contains "$fixture" "$fixture/apps/website"$'\trun\ttest' || return
}

test_build_runs_standard_app_builds() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" build

  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" compile --warnings-as-errors || return
  assert_log_entry "$fixture" pnpm "$fixture/apps/workspace" run build || return
  assert_log_entry "$fixture" pnpm "$fixture/apps/website" run build || return
}

test_shell_wrapper_format_target_uses_unmatched_pattern_guard() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  JUST_FAKE_PNPM_UNSUPPORTED_OXFMT_FAIL=9 run_wrapper status output "$fixture" "$fixture" format .just/commands/lint

  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxfmt --no-error-on-unmatched-pattern .just/commands/lint || return
}
