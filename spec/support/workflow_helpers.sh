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

assert_runtime_bundle_files() {
  local fixture="$1"
  local expected
  local actual

  expected="$(mktemp "$fixture/expected-runtime-files.XXXXXX")"
  actual="$(mktemp "$fixture/actual-runtime-files.XXXXXX")"

  cat >"$expected" <<'TEXT'
bin/workflow
commands/build
commands/check
commands/dev
commands/format
commands/help
commands/install
commands/lint
commands/migrate
commands/remove
commands/reset
commands/test
commands/update
kokjinsam.just
lib/install_hex_dep.exs
lib/lib.sh
lib/remove_hex_dep.exs
TEXT

  (
    cd "$fixture/.just" &&
      find . -type f -print |
      sed 's#^\./##' |
        sort
  ) >"$actual"

  if ! diff -u "$expected" "$actual" >&2; then
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
    "$fixture/scripts" \
    "$fixture/.vscode" \
    "$fixture/docs/specs" \
    "$fixture/apps/api/assets/css" \
    "$fixture/apps/api/assets/js" \
    "$fixture/apps/api/lib" \
    "$fixture/apps/api/scripts" \
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
YAML

  printf "{}\n" >"$fixture/package.json"
  printf "packages:\n  - apps/*\n" >"$fixture/pnpm-workspace.yaml"
  printf "export default {}\n" >"$fixture/oxlint.config.ts"
  printf "export default {}\n" >"$fixture/stylelint.config.js"
  printf "{}\n" >"$fixture/knip.json"
  printf "processes: {}\n" >"$fixture/process-compose.yaml"
  printf "{}\n" >"$fixture/.oxfmtrc.json"
  printf "line-length = 88\n" >"$fixture/ruff.toml"
  printf "fake tla2tools jar\n" >"$fixture/tla2tools.jar"
  printf "root = true\n" >"$fixture/.editorconfig"
  printf "{}\n" >"$fixture/.vscode/settings.json"
  printf "default:\n" >"$fixture/Justfile"

  printf "body { color: black; }\n" >"$fixture/apps/api/assets/css/app.css"
  printf "export const api = 1\n" >"$fixture/apps/api/assets/js/app.ts"
  printf "print('api')\n" >"$fixture/apps/api/scripts/tool.py"
  printf "print('hello')\n" >"$fixture/scripts/foo.py"
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
  printf -- "---\nconst title = 'Home'\n---\n<h1>{title}</h1>\n" >"$fixture/apps/website/src/page.astro"
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
    "$fixture/.just/commands/update" \
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
  "dialyzer --format short")
    exit "${JUST_FAKE_MIX_DIALYZER_STATUS:-0}"
    ;;
esac

exit 0
'

  # shellcheck disable=SC2016
  write_fake_command "$fake_bin/ruff" '
printf "ruff\t%s" "$PWD" >> "$JUST_FAKE_LOG"
for arg in "$@"; do
  printf "\t%s" "$arg" >> "$JUST_FAKE_LOG"
done
printf "\n" >> "$JUST_FAKE_LOG"

case "$*" in
  "format"*)
    exit "${JUST_FAKE_RUFF_FORMAT_STATUS:-0}"
    ;;
  "check"*)
    exit "${JUST_FAKE_RUFF_CHECK_STATUS:-0}"
    ;;
esac

exit 0
'

  # shellcheck disable=SC2016
  write_fake_command "$fake_bin/pyrefly" '
printf "pyrefly\t%s" "$PWD" >> "$JUST_FAKE_LOG"
for arg in "$@"; do
  printf "\t%s" "$arg" >> "$JUST_FAKE_LOG"
done
printf "\n" >> "$JUST_FAKE_LOG"
exit "${JUST_FAKE_PYREFLY_STATUS:-0}"
'

  # shellcheck disable=SC2016
  write_fake_command "$fake_bin/java" '
printf "java\t%s" "$PWD" >> "$JUST_FAKE_LOG"
for arg in "$@"; do
  printf "\t%s" "$arg" >> "$JUST_FAKE_LOG"
done
printf "\n" >> "$JUST_FAKE_LOG"

if [[ "${JUST_FAKE_JAVA_STATUS:-0}" != "0" ]]; then
  exit "$JUST_FAKE_JAVA_STATUS"
fi

if [[ "${1:-}" == "-cp" && "${3:-}" == "formatter.Main" && "$#" -ge 5 ]]; then
  cp "$4" "$5"
fi

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

  # shellcheck disable=SC2016
  write_fake_command "$fake_bin/sany" '
printf "sany\t%s" "$PWD" >> "$JUST_FAKE_LOG"
for arg in "$@"; do
  printf "\t%s" "$arg" >> "$JUST_FAKE_LOG"
done
printf "\n" >> "$JUST_FAKE_LOG"
exit "${JUST_FAKE_SANY_STATUS:-0}"
'

  # shellcheck disable=SC2016
  write_fake_command "$fake_bin/tlc" '
printf "tlc\t%s" "$PWD" >> "$JUST_FAKE_LOG"
for arg in "$@"; do
  printf "\t%s" "$arg" >> "$JUST_FAKE_LOG"
done
printf "\n" >> "$JUST_FAKE_LOG"
exit "${JUST_FAKE_TLC_STATUS:-0}"
'

  # shellcheck disable=SC2016
  write_fake_command "$fake_bin/alloy" '
printf "alloy\t%s" "$PWD" >> "$JUST_FAKE_LOG"
for arg in "$@"; do
  printf "\t%s" "$arg" >> "$JUST_FAKE_LOG"
done
printf "\n" >> "$JUST_FAKE_LOG"
exit "${JUST_FAKE_ALLOY_STATUS:-0}"
'

  # shellcheck disable=SC2016
  write_fake_command "$fake_bin/vacuum" '
printf "vacuum\t%s" "$PWD" >> "$JUST_FAKE_LOG"
for arg in "$@"; do
  printf "\t%s" "$arg" >> "$JUST_FAKE_LOG"
done
printf "\n" >> "$JUST_FAKE_LOG"
exit "${JUST_FAKE_VACUUM_STATUS:-0}"
'

  : >"$fixture/fake.log"
  printf "%s\n" "$fixture"
}

init_git_fixture() {
  git -C "$1" init --quiet
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
        JUST_FAKE_ALLOY_STATUS="${JUST_FAKE_ALLOY_STATUS:-0}" \
        JUST_FAKE_JAVA_STATUS="${JUST_FAKE_JAVA_STATUS:-0}" \
        JUST_FAKE_MIX_DIALYZER_STATUS="${JUST_FAKE_MIX_DIALYZER_STATUS:-0}" \
        JUST_FAKE_MIX_FORMAT_CHECK_STATUS="${JUST_FAKE_MIX_FORMAT_CHECK_STATUS:-0}" \
        JUST_FAKE_MIX_SOBELOW_STATUS="${JUST_FAKE_MIX_SOBELOW_STATUS:-0}" \
        JUST_FAKE_PNPM_FAIL_OXFMT="${JUST_FAKE_PNPM_FAIL_OXFMT:-0}" \
        JUST_FAKE_PNPM_FAIL_OXLINT="${JUST_FAKE_PNPM_FAIL_OXLINT:-0}" \
        JUST_FAKE_PNPM_FAIL_STYLELINT="${JUST_FAKE_PNPM_FAIL_STYLELINT:-0}" \
        JUST_FAKE_PNPM_UNSUPPORTED_OXFMT_FAIL="${JUST_FAKE_PNPM_UNSUPPORTED_OXFMT_FAIL:-0}" \
        JUST_FAKE_PROCESS_COMPOSE_STATUS="${JUST_FAKE_PROCESS_COMPOSE_STATUS:-0}" \
        JUST_FAKE_PYREFLY_STATUS="${JUST_FAKE_PYREFLY_STATUS:-0}" \
        JUST_FAKE_RUFF_FORMAT_STATUS="${JUST_FAKE_RUFF_FORMAT_STATUS:-0}" \
        JUST_FAKE_RUFF_CHECK_STATUS="${JUST_FAKE_RUFF_CHECK_STATUS:-0}" \
        JUST_FAKE_SANY_STATUS="${JUST_FAKE_SANY_STATUS:-0}" \
        JUST_FAKE_TLC_STATUS="${JUST_FAKE_TLC_STATUS:-0}" \
        JUST_FAKE_VACUUM_STATUS="${JUST_FAKE_VACUUM_STATUS:-0}" \
        TLA2TOOLS_JAR="$fixture/tla2tools.jar" \
        bash "$DOT_JUST_ROOT/commands/$command" "$@"
  ) >"$__captured_output" 2>&1
  __captured_status="$?"
  set -e

  printf -v "$__status_var" "%s" "$__captured_status"
  printf -v "$__output_var" "%s" "$__captured_output"
}

prepare_update_entrypoint() {
  local fixture="$1"

  cp "$JUST_BUNDLE_ROOT/kokjinsam.just" "$fixture/.just/kokjinsam.just"
  cp "$DOT_JUST_ROOT/bin/workflow" "$fixture/.just/bin/workflow"
  cp "$DOT_JUST_ROOT/commands/update" "$fixture/.just/commands/update"
  cp "$DOT_JUST_ROOT/lib/lib.sh" "$fixture/.just/lib/lib.sh"
  chmod +x "$fixture/.just/bin/workflow"

  cat >"$fixture/Justfile" <<'JUST'
import? '.just/kokjinsam.just'

default:
JUST
}

run_just_in_fixture() {
  local __status_var="$1"
  local __output_var="$2"
  local fixture="$3"
  local __captured_output
  local __captured_status

  shift 3
  __captured_output="$(mktemp "$fixture/output.just.XXXXXX")"

  set +e
  (
    cd "$fixture" &&
      env \
        PATH="$fixture/fake-bin:$PATH" \
        KOKJINSAM_JUST_BASE_URL="file://$JUST_BUNDLE_ROOT" \
        JUST_FAKE_LOG="$fixture/fake.log" \
        just "$@"
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
  assert_output_contains "$output" "== Summary ==" || return
  assert_output_contains "$output" "[info] configured oxfmt: .oxfmtrc.json" || return
  assert_output_contains "$output" "[info] configured ruff: ruff.toml" || return
  assert_output_contains "$output" "[info] configured mix format: apps/api/.formatter.exs" || return
  assert_output_contains "$output" "[info] [pnpm exec oxfmt --check] [.oxfmtrc.json] configured files" || return
  assert_output_contains "$output" "[info] [ruff format --check] [ruff.toml] configured files" || return
  assert_output_contains "$output" "[info] [mix format --check-formatted] [apps/api/.formatter.exs] apps/api" || return
  assert_output_not_contains "$output" "Command summary" || return
  assert_output_not_contains "$output" "files:" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxfmt --check || return
  assert_log_entry "$fixture" ruff "$fixture" format --check || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" format --check-formatted || return
  assert_log_not_contains "$fixture" ".just/" || return
}

test_path_selected_summary_uses_selector_and_pluralization() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" format --check apps/workspace/src/main.tsx
  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured oxfmt: .oxfmtrc.json" || return
  assert_output_contains "$output" "[info] [pnpm exec oxfmt --check apps/workspace/src/main.tsx] [.oxfmtrc.json] apps/workspace/src/main.tsx" || return
  assert_output_not_contains "$output" "Command summary" || return

  run_wrapper status output "$fixture" "$fixture" lint apps/workspace/src
  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured oxlint: oxlint.config.ts" || return
  assert_output_contains "$output" "[info] [pnpm exec oxlint apps/workspace/src/main.tsx apps/workspace/src/secondary.ts] [oxlint.config.ts] apps/workspace/src/main.tsx apps/workspace/src/secondary.ts" || return
  assert_output_not_contains "$output" "Command summary" || return
}

test_path_selected_python_uses_ruff_config() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" format --check scripts/foo.py

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured ruff: ruff.toml" || return
  assert_output_contains "$output" "[info] [ruff format --check scripts/foo.py] [ruff.toml] scripts/foo.py" || return
  assert_log_entry "$fixture" ruff "$fixture" format --check scripts/foo.py || return
}

test_path_selected_tla_uses_tla_formatter() {
  local fixture
  local java_prefix
  local output
  local status

  fixture="$(new_fixture)"
  printf -- "---- MODULE Example ----\n====\n" >"$fixture/docs/specs/Example.tla"

  run_wrapper status output "$fixture" "$fixture" format docs/specs/Example.tla

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured TLA+ formatter: tla2tools.jar" || return
  assert_output_contains "$output" "[info] [java -cp tla2tools.jar formatter.Main docs/specs/Example.tla docs/specs/Example.tla] [tla2tools.jar] docs/specs/Example.tla" || return
  java_prefix="$(printf "java\t%s\t-cp\t%s\tformatter.Main\t%s\t" "$fixture" "$fixture/tla2tools.jar" "$fixture/docs/specs/Example.tla")"
  assert_file_contains "$fixture/fake.log" "$java_prefix" || return
}

test_tla_format_check_fails_clearly() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  printf -- "---- MODULE Example ----\n====\n" >"$fixture/docs/specs/Example.tla"

  run_wrapper status output "$fixture" "$fixture" format --check docs/specs/Example.tla

  assert_nonzero_status "$status" "$output" || return
  assert_output_contains "$output" "error: TLA+ formatter does not support check mode; run 'just format docs/specs/Example.tla'" || return
  assert_log_not_contains "$fixture" "java" || return
}

test_path_selected_python_without_config_skips_ruff() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  rm "$fixture/ruff.toml"

  run_wrapper status output "$fixture" "$fixture" format --check scripts/foo.py

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[warning] unconfigured ruff: no config found for scripts/foo.py" || return
  assert_output_contains "$output" "[warning] [skipped] [ruff format --check scripts/foo.py] [no config] scripts/foo.py" || return
  assert_log_not_contains "$fixture" "ruff" || return
}

test_elixir_app_js_path_uses_oxfmt() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" format --check apps/api/assets/js/app.ts

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured oxfmt: .oxfmtrc.json" || return
  assert_output_contains "$output" "[info] [pnpm exec oxfmt --check apps/api/assets/js/app.ts] [.oxfmtrc.json] apps/api/assets/js/app.ts" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxfmt --check --no-error-on-unmatched-pattern apps/api/assets/js/app.ts || return
  assert_log_not_contains "$fixture" "mix" || return
}

test_elixir_app_directory_uses_available_formatters() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" format --check apps/api

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured oxfmt: .oxfmtrc.json" || return
  assert_output_contains "$output" "[info] configured ruff: ruff.toml" || return
  assert_output_contains "$output" "[info] configured mix format: apps/api/.formatter.exs" || return
  assert_output_contains "$output" "[info] [pnpm exec oxfmt --check apps/api] [.oxfmtrc.json] apps/api" || return
  assert_output_contains "$output" "[info] [ruff format --check apps/api/scripts/tool.py] [ruff.toml] apps/api/scripts/tool.py" || return
  assert_output_contains "$output" "[info] [mix format --check-formatted] [apps/api/.formatter.exs] apps/api" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxfmt --check --no-error-on-unmatched-pattern apps/api || return
  assert_log_entry "$fixture" ruff "$fixture" format --check apps/api/scripts/tool.py || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" format --check-formatted || return
}

test_format_directory_discovery_uses_gitignore() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  init_git_fixture "$fixture"
  mkdir -p "$fixture/apps/api/scripts/generated"
  printf "generated/\n" >"$fixture/apps/api/scripts/.gitignore"
  printf "print('ignored')\n" >"$fixture/apps/api/scripts/generated/ignored.py"
  printf "print('tracked')\n" >"$fixture/apps/api/scripts/generated/tracked.py"
  git -C "$fixture" add -f apps/api/scripts/generated/tracked.py

  run_wrapper status output "$fixture" "$fixture" format --check apps/api

  assert_status 0 "$status" "$output" || return
  assert_file_contains "$fixture/fake.log" "apps/api/scripts/tool.py" || return
  assert_file_contains "$fixture/fake.log" "apps/api/scripts/generated/tracked.py" || return
  assert_log_not_contains "$fixture" "apps/api/scripts/generated/ignored.py" || return
}

test_default_format_missing_configs_skips_without_failure() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  rm "$fixture/.oxfmtrc.json"
  rm "$fixture/ruff.toml"
  rm "$fixture/apps/api/.formatter.exs"

  run_wrapper status output "$fixture" "$fixture" format --check

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[warning] unconfigured oxfmt: no .oxfmtrc.json found" || return
  assert_output_contains "$output" "[warning] unconfigured ruff: no ruff.toml, .ruff.toml, or pyproject.toml config found" || return
  assert_output_contains "$output" "[warning] unconfigured mix format: no .formatter.exs found for apps/api" || return
  assert_output_contains "$output" "[warning] [skipped] [pnpm exec oxfmt --check] [no config] configured files" || return
  assert_output_contains "$output" "[warning] [skipped] [ruff format --check] [no config] configured files" || return
  assert_output_contains "$output" "[warning] [skipped] [mix format --check-formatted] [no config] apps/api" || return
  assert_log_not_contains "$fixture" "pnpm" || return
  assert_log_not_contains "$fixture" "ruff" || return
  assert_log_not_contains "$fixture" "mix" || return
}

test_default_lint_summary_uses_repo_defaults() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" lint

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "== Summary ==" || return
  assert_output_contains "$output" "[info] configured oxlint: oxlint.config.ts" || return
  assert_output_contains "$output" "[info] configured ruff: ruff.toml" || return
  assert_output_contains "$output" "[info] configured stylelint: stylelint.config.js" || return
  assert_output_contains "$output" "[info] configured mix format: apps/api/.formatter.exs" || return
  assert_output_contains "$output" "[info] configured credo: apps/api/.credo.exs" || return
  assert_output_contains "$output" "[info] [mix credo --strict] [apps/api/.credo.exs] apps/api" || return
  assert_output_contains "$output" "[info] [pnpm exec oxlint] [oxlint.config.ts] configured files" || return
  assert_output_contains "$output" "[info] [ruff check] [ruff.toml] configured files" || return
  assert_output_contains "$output" "[info] [pnpm exec stylelint .] [stylelint.config.js] configured files" || return
  assert_output_not_contains "$output" "Command summary" || return
  assert_output_not_contains "$output" "Linter" || return
  assert_output_not_contains "$output" "default lint paths" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxlint || return
  assert_log_entry "$fixture" ruff "$fixture" check || return
  assert_log_entry "$fixture" pnpm "$fixture" exec stylelint . || return
  assert_log_not_contains "$fixture" ".just/" || return
}

test_lint_enforces_spec_placement() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  printf -- "---- MODULE Misplaced ----\n====\n" >"$fixture/scripts/Misplaced.tla"
  printf "openapi: 3.1.0\ninfo:\n  title: Misplaced\n  version: 1.0.0\npaths: {}\n" >"$fixture/openapi.yaml"

  run_wrapper status output "$fixture" "$fixture" lint

  assert_nonzero_status "$status" "$output" || return
  assert_output_contains "$output" "[info] configured spec placement: docs/" || return
  assert_output_contains "$output" "Spec files must live under docs/: scripts/Misplaced.tla" || return
  assert_output_contains "$output" "Spec files must live under docs/: openapi.yaml" || return
  assert_output_contains "$output" "[error] [check specs live under docs/] [docs/] ." || return
}

test_default_lint_runs_vacuum_for_openapi_specs() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  mkdir -p "$fixture/docs/api"
  printf "openapi: 3.1.0\ninfo:\n  title: API\n  version: 1.0.0\npaths: {}\n" >"$fixture/docs/api/openapi.yaml"

  run_wrapper status output "$fixture" "$fixture" lint

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured OpenAPI lint: vacuum" || return
  assert_output_contains "$output" "[info] [vacuum lint docs/api/openapi.yaml] [openapi] docs/api/openapi.yaml" || return
  assert_log_entry "$fixture" vacuum "$fixture" lint "$fixture/docs/api/openapi.yaml" || return
}

test_lint_selected_python_uses_ruff_config() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" lint scripts/foo.py

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured ruff: ruff.toml" || return
  assert_output_contains "$output" "[info] [ruff check scripts/foo.py] [ruff.toml] scripts/foo.py" || return
  assert_log_entry "$fixture" ruff "$fixture" check scripts/foo.py || return
  assert_log_not_contains "$fixture" "pnpm" || return
}

test_lint_selected_python_without_config_skips_ruff() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  rm "$fixture/ruff.toml"

  run_wrapper status output "$fixture" "$fixture" lint scripts/foo.py

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[warning] unconfigured ruff: no config found for scripts/foo.py" || return
  assert_output_contains "$output" "[warning] [skipped] [ruff check scripts/foo.py] [no config] scripts/foo.py" || return
  assert_log_not_contains "$fixture" "ruff" || return
}

test_elixir_app_lint_directory_uses_available_linters() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" lint apps/api

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] [mix credo --strict] [apps/api/.credo.exs] apps/api" || return
  assert_output_contains "$output" "[info] [pnpm exec oxlint apps/api/assets/js/app.ts] [oxlint.config.ts] apps/api/assets/js/app.ts" || return
  assert_output_contains "$output" "[info] [ruff check apps/api/scripts/tool.py] [ruff.toml] apps/api/scripts/tool.py" || return
  assert_output_contains "$output" "[info] [pnpm exec stylelint apps/api/assets/css/app.css] [stylelint.config.js] apps/api/assets/css/app.css" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" credo --strict || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxlint apps/api/assets/js/app.ts || return
  assert_log_entry "$fixture" ruff "$fixture" check apps/api/scripts/tool.py || return
  assert_log_entry "$fixture" pnpm "$fixture" exec stylelint apps/api/assets/css/app.css || return
}

test_elixir_app_lint_directory_discovery_uses_gitignore() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  init_git_fixture "$fixture"
  mkdir -p "$fixture/apps/api/assets/js/generated"
  printf "generated/\n" >"$fixture/apps/api/assets/js/.gitignore"
  printf "export const ignored = 1\n" >"$fixture/apps/api/assets/js/generated/ignored.ts"
  printf "export const tracked = 1\n" >"$fixture/apps/api/assets/js/generated/tracked.ts"
  git -C "$fixture" add -f apps/api/assets/js/generated/tracked.ts

  run_wrapper status output "$fixture" "$fixture" lint apps/api

  assert_status 0 "$status" "$output" || return
  assert_file_contains "$fixture/fake.log" "apps/api/assets/js/app.ts" || return
  assert_file_contains "$fixture/fake.log" "apps/api/assets/js/generated/tracked.ts" || return
  assert_log_not_contains "$fixture" "apps/api/assets/js/generated/ignored.ts" || return
}

test_lint_repo_root_selector_runs_default_contract() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" lint .

  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" credo --strict || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxlint || return
  assert_log_entry "$fixture" ruff "$fixture" check || return
  assert_log_entry "$fixture" pnpm "$fixture" exec stylelint . || return
  assert_log_not_contains "$fixture" ".just/" || return
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

test_lint_directory_discovery_uses_gitignore() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  init_git_fixture "$fixture"
  mkdir -p "$fixture/apps/workspace/src/generated"
  printf "generated/\n" >"$fixture/apps/workspace/src/.gitignore"
  printf "export const ignored = 1\n" >"$fixture/apps/workspace/src/generated/ignored.ts"
  printf "export const tracked = 1\n" >"$fixture/apps/workspace/src/generated/tracked.ts"
  git -C "$fixture" add -f apps/workspace/src/generated/tracked.ts

  run_wrapper status output "$fixture" "$fixture" lint apps/workspace/src

  assert_status 0 "$status" "$output" || return
  assert_file_contains "$fixture/fake.log" "apps/workspace/src/main.tsx" || return
  assert_file_contains "$fixture/fake.log" "apps/workspace/src/secondary.ts" || return
  assert_file_contains "$fixture/fake.log" "apps/workspace/src/generated/tracked.ts" || return
  assert_log_not_contains "$fixture" "apps/workspace/src/generated/ignored.ts" || return
}

test_lint_explicit_ignored_file_selector_wins() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  init_git_fixture "$fixture"
  mkdir -p "$fixture/apps/workspace/src/generated"
  printf "generated/\n" >"$fixture/apps/workspace/src/.gitignore"
  printf "export const ignored = 1\n" >"$fixture/apps/workspace/src/generated/ignored.ts"

  run_wrapper status output "$fixture" "$fixture" lint apps/workspace/src/generated/ignored.ts

  assert_status 0 "$status" "$output" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxlint apps/workspace/src/generated/ignored.ts || return
}

test_lint_preflight_failure_reports_and_continues() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  printf "[]\n" >"$fixture/apps/api/.formatter.exs"

  run_wrapper status output "$fixture" "$fixture" lint apps/api

  assert_nonzero_status "$status" "$output" || return
  assert_output_contains "$output" "[error] [check Styler Formatter] [apps/api/.formatter.exs] Styler in .formatter.exs plugins" || return
  assert_output_contains "$output" "[info] [mix credo --strict] [apps/api/.credo.exs] apps/api" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" credo --strict || return
}

test_lint_aggregates_after_failing_sobelow() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  JUST_FAKE_MIX_SOBELOW_STATUS=44 run_wrapper status output "$fixture" "$fixture" lint

  assert_nonzero_status "$status" "$output" || return
  assert_output_contains "$output" "[error] [mix sobelow --private --exit --threshold high] [apps/api/mix.exs] apps/api" || return
  assert_output_contains "$output" "[info] [check ExSlop Dependency] [apps/api/mix.exs] {:ex_slop, ...} in mix.exs" || return
  assert_output_contains "$output" "[info] [check ExSlop Credo Plugin] [apps/api/.credo.exs] {ExSlop, ...} in .credo.exs plugins" || return
  assert_output_contains "$output" "[info] [check ExcellentMigrations Dependency] [apps/api/mix.exs] {:excellent_migrations, ...} in mix.exs" || return
  assert_output_contains "$output" "[info] [check CodeStyle Dependency] [apps/api/mix.exs] {:code_style, ...} in mix.exs" || return
  assert_output_contains "$output" "[info] [check Styler Formatter] [apps/api/.formatter.exs] Styler in .formatter.exs plugins" || return
  assert_output_contains "$output" "[info] [mix ex_dna --min-mass 40 --max-clones 0] [apps/api/mix.exs] apps/api" || return
  assert_output_contains "$output" "[info] [mix reach.check --arch --smells --strict] [apps/api/mix.exs] apps/api" || return
  assert_output_contains "$output" "[info] [pnpm exec oxlint] [oxlint.config.ts] configured files" || return
  assert_output_contains "$output" "[info] [ruff check] [ruff.toml] configured files" || return
  assert_output_contains "$output" "[info] [pnpm exec stylelint .] [stylelint.config.js] configured files" || return
  assert_output_not_contains "$output" "Dialyzer" || return
  assert_log_not_contains "$fixture" "ex_slop" || return
  assert_log_not_contains "$fixture" "dialyzer" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" reach.check --arch --smells --strict || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxlint || return
  assert_log_entry "$fixture" ruff "$fixture" check || return
  assert_log_entry "$fixture" pnpm "$fixture" exec stylelint . || return
  assert_log_not_contains "$fixture" ".just/" || return
}

test_lint_aggregates_after_failing_ruff() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  JUST_FAKE_RUFF_CHECK_STATUS=42 run_wrapper status output "$fixture" "$fixture" lint scripts/foo.py apps/workspace/src/main.tsx

  assert_nonzero_status "$status" "$output" || return
  assert_output_contains "$output" "[info] [pnpm exec oxlint apps/workspace/src/main.tsx] [oxlint.config.ts] apps/workspace/src/main.tsx" || return
  assert_output_contains "$output" "[error] [ruff check scripts/foo.py] [ruff.toml] scripts/foo.py" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec oxlint apps/workspace/src/main.tsx || return
  assert_log_entry "$fixture" ruff "$fixture" check scripts/foo.py || return
}

test_format_check_aggregates_formatter_failures() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  JUST_FAKE_PNPM_FAIL_OXFMT=31 run_wrapper status output "$fixture" "$fixture" format --check apps/workspace/src/main.tsx apps/api/mix.exs

  assert_nonzero_status "$status" "$output" || return
  assert_output_contains "$output" "[error] [pnpm exec oxfmt --check apps/workspace/src/main.tsx] [.oxfmtrc.json] apps/workspace/src/main.tsx" || return
  assert_output_contains "$output" "[info] [mix format --check-formatted mix.exs] [apps/api/.formatter.exs] apps/api/mix.exs" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" format --check-formatted mix.exs || return
}

test_format_check_aggregates_ruff_failures() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  JUST_FAKE_RUFF_FORMAT_STATUS=42 run_wrapper status output "$fixture" "$fixture" format --check scripts/foo.py apps/api/mix.exs

  assert_nonzero_status "$status" "$output" || return
  assert_output_contains "$output" "[error] [ruff format --check scripts/foo.py] [ruff.toml] scripts/foo.py" || return
  assert_output_contains "$output" "[info] [mix format --check-formatted mix.exs] [apps/api/.formatter.exs] apps/api/mix.exs" || return
  assert_log_entry "$fixture" ruff "$fixture" format --check scripts/foo.py || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" format --check-formatted mix.exs || return
}

test_check_specs_uses_asdf_shims() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  cat >"$fixture/docs/specs/Example.tla" <<'TLA'
---- MODULE Example ----
VARIABLE x
Init == x = 0
Next == x' = x
====
TLA
  printf "INIT Init\nNEXT Next\n" >"$fixture/docs/specs/Example.cfg"
  printf "sig Example {}\nrun {} for 1\n" >"$fixture/docs/specs/Structure.als"

  run_wrapper status output "$fixture" "$fixture" check specs docs/specs

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "== Summary ==" || return
  assert_output_contains "$output" "[info] [sany docs/specs/Example.tla] [tlaplus] docs/specs/Example.tla" || return
  assert_output_contains "$output" "[info] [tlc -config docs/specs/Example.cfg docs/specs/Example.tla] [tlaplus] docs/specs/Example.cfg" || return
  assert_output_contains "$output" "[info] [alloy commands docs/specs/Structure.als] [alloy] docs/specs/Structure.als" || return
  assert_output_not_contains "$output" "Command summary" || return
  assert_log_entry "$fixture" sany "$fixture" docs/specs/Example.tla || return
  assert_log_entry "$fixture" tlc "$fixture" -config docs/specs/Example.cfg docs/specs/Example.tla || return
  assert_log_entry "$fixture" alloy "$fixture" commands docs/specs/Structure.als || return
}

test_check_specs_directory_discovery_uses_gitignore() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  init_git_fixture "$fixture"
  mkdir -p "$fixture/docs/specs/generated"
  printf "generated/\n" >"$fixture/docs/specs/.gitignore"
  printf -- "---- MODULE Included ----\n====\n" >"$fixture/docs/specs/Included.tla"
  printf -- "---- MODULE Ignored ----\n====\n" >"$fixture/docs/specs/generated/Ignored.tla"
  printf -- "---- MODULE Tracked ----\n====\n" >"$fixture/docs/specs/generated/Tracked.tla"
  git -C "$fixture" add -f docs/specs/generated/Tracked.tla

  run_wrapper status output "$fixture" "$fixture" check specs docs/specs

  assert_status 0 "$status" "$output" || return
  assert_file_contains "$fixture/fake.log" "docs/specs/Included.tla" || return
  assert_file_contains "$fixture/fake.log" "docs/specs/generated/Tracked.tla" || return
  assert_log_not_contains "$fixture" "docs/specs/generated/Ignored.tla" || return
}

test_check_specs_does_not_lint_openapi_specs() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  mkdir -p "$fixture/docs/api"
  printf "openapi: 3.1.0\ninfo:\n  title: API\n  version: 1.0.0\npaths: {}\n" >"$fixture/docs/api/openapi.yaml"

  run_wrapper status output "$fixture" "$fixture" check specs docs/api/openapi.yaml

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "No .tla, .cfg, or .als files found." || return
  assert_log_not_contains "$fixture" "vacuum" || return
}

test_check_types_uses_compact_summary() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" check types

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured typecheck: apps/workspace/package.json" || return
  assert_output_contains "$output" "[info] configured typecheck: apps/website/package.json" || return
  assert_output_contains "$output" "[info] configured pyrefly: auto" || return
  assert_output_contains "$output" "[info] [pnpm run typecheck] [apps/workspace/package.json] apps/workspace" || return
  assert_output_contains "$output" "[info] [pnpm run typecheck] [apps/website/package.json] apps/website" || return
  assert_output_contains "$output" "[info] [pyrefly check] [auto] ." || return
  assert_output_not_contains "$output" "Command summary" || return
  assert_log_entry "$fixture" pnpm "$fixture/apps/workspace" run typecheck || return
  assert_log_entry "$fixture" pnpm "$fixture/apps/website" run typecheck || return
  assert_log_entry "$fixture" pyrefly "$fixture" check || return
}

test_check_types_runs_pyrefly_for_selected_python() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" check types scripts/foo.py

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured pyrefly: auto" || return
  assert_output_contains "$output" "[info] [pyrefly check scripts/foo.py] [auto] scripts/foo.py" || return
  assert_log_entry "$fixture" pyrefly "$fixture" check scripts/foo.py || return
  assert_log_not_contains "$fixture" $'run\ttypecheck' || return
}

test_check_dialyzer_uses_elixir_roots() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" check dialyzer

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[info] configured dialyzer: apps/api/mix.exs" || return
  assert_output_contains "$output" "[info] [mix dialyzer --format short] [apps/api/mix.exs] apps/api" || return
  assert_output_not_contains "$output" "Command summary" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" dialyzer --format short || return
  assert_log_not_contains "$fixture" "pnpm" || return
}

test_check_dialyzer_reports_failures() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  JUST_FAKE_MIX_DIALYZER_STATUS=43 run_wrapper status output "$fixture" "$fixture" check dialyzer

  assert_nonzero_status "$status" "$output" || return
  assert_output_contains "$output" "[error] [mix dialyzer --format short] [apps/api/mix.exs] apps/api" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" dialyzer --format short || return
}

test_check_knip_uses_compact_summary() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" check knip

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "== Summary ==" || return
  assert_output_contains "$output" "[info] [pnpm exec knip] [knip.json] ." || return
  assert_output_not_contains "$output" "Command summary" || return
  assert_log_entry "$fixture" pnpm "$fixture" exec knip || return
}

test_install_runs_project_dependency_installs() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" install

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[ok] Install Elixir Deps" || return
  assert_output_contains "$output" "[ok] Install Node Deps" || return
  assert_log_entry "$fixture" mix "$fixture/apps/api" deps.get || return
  assert_log_entry "$fixture" pnpm "$fixture" --config.confirmModulesPurge=false install || return
}

test_install_npm_dep_uses_direct_selector() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" install npm:lucide-react 0.475.0 --app workspace

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "[ok] Install npm Dependency" || return
  assert_output_contains "$output" "app: workspace" || return
  assert_output_contains "$output" "package: npm:lucide-react" || return
  assert_output_contains "$output" "version: 0.475.0" || return
  assert_log_entry "$fixture" pnpm "$fixture" --config.confirmModulesPurge=false --filter ./apps/workspace add lucide-react@0.475.0 || return
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

  run_wrapper status output "$fixture" "$fixture" install hex:uniq 0.6.3 --app api

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

test_update_recipes_dispatches_from_just() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"
  prepare_update_entrypoint "$fixture"
  mkdir -p "$fixture/.just/tests"
  printf "source-only test helper\n" >"$fixture/.just/tests/check_scripts"

  run_just_in_fixture status output "$fixture" update recipes

  assert_status 0 "$status" "$output" || return
  assert_output_contains "$output" "Installed Kok Jin Sam's just workflow into $fixture/.just" || return
  assert_file_contains "$fixture/.just/kokjinsam.just" "update *args:" || return
  assert_file_contains "$fixture/.just/bin/workflow" "remove | update | check" || return
  assert_runtime_bundle_files "$fixture" || return
}

test_update_unknown_target_fails_clearly() {
  local fixture
  local output
  local status

  fixture="$(new_fixture)"

  run_wrapper status output "$fixture" "$fixture" update nope

  assert_nonzero_status "$status" "$output" || return
  assert_output_contains "$output" "error: unknown update target: nope" || return
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

  run_wrapper status output "$fixture" "$fixture" remove hex:uniq --app api

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
  assert_output_contains "$output" "== Summary ==" || return
  assert_output_contains "$output" "[info] configured mix test: apps/api/mix.exs" || return
  assert_output_contains "$output" "[info] configured node test: apps/workspace/package.json" || return
  assert_output_contains "$output" "[warning] unconfigured node test: no test script in apps/website/package.json" || return
  assert_output_contains "$output" "[info] [mix test] [apps/api/mix.exs] apps/api" || return
  assert_output_contains "$output" "[info] [pnpm run test] [apps/workspace/package.json] apps/workspace" || return
  assert_output_contains "$output" "[warning] [skipped] [pnpm run test] [apps/website/package.json] apps/website" || return
  assert_output_not_contains "$output" "Command summary" || return
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
  assert_output_contains "$output" "== Summary ==" || return
  assert_output_contains "$output" "[info] configured mix build: apps/api/mix.exs" || return
  assert_output_contains "$output" "[info] configured node build: apps/workspace/package.json" || return
  assert_output_contains "$output" "[info] configured node build: apps/website/package.json" || return
  assert_output_contains "$output" "[info] [mix compile --warnings-as-errors] [apps/api/mix.exs] apps/api" || return
  assert_output_contains "$output" "[info] [pnpm run build] [apps/workspace/package.json] apps/workspace" || return
  assert_output_contains "$output" "[info] [pnpm run build] [apps/website/package.json] apps/website" || return
  assert_output_not_contains "$output" "Command summary" || return
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
