#!/usr/bin/env bash

repo_root() {
  local source_dir
  source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
  cd -- "$source_dir/../.." && pwd
}

REPO_ROOT="${REPO_ROOT:-$(repo_root)}"
JUST_ROOT="${JUST_ROOT:-$REPO_ROOT/.just}"
JUST_CONFIG="${JUST_CONFIG:-$REPO_ROOT/just.yaml}"
JUST_CALLER_DIR="${JUST_CALLER_DIR:-$REPO_ROOT}"
JUST_STEP_LABELS=()
JUST_STEP_CWDS=()
JUST_STEP_COMMANDS=()
JUST_STEP_DETAILS=()
JUST_STEP_STATUSES=()
JUST_STEP_DURATIONS=()
JUST_STEP_KINDS=()
JUST_STEP_EXTRAS=()

die() {
  printf "error: %s\n" "$*" >&2
  exit 1
}

quote_arg() {
  local arg="$1"

  if [[ -z "$arg" ]]; then
    printf "''"
  elif [[ "$arg" =~ ^[A-Za-z0-9_./:=+,%@-]+$ ]]; then
    printf "%s" "$arg"
  else
    printf "%q" "$arg"
  fi
}

command_string() {
  local arg
  local separator=""

  for arg in "$@"; do
    printf "%s" "$separator"
    quote_arg "$arg"
    separator=" "
  done
}

record_step() {
  local label="$1"
  local cwd="$2"
  local command="$3"
  local details="$4"
  local status="$5"
  local duration="$6"
  local kind="${JUST_STEP_REPORT_KIND:-tool}"
  local extra="${JUST_STEP_REPORT_EXTRA:-}"

  JUST_STEP_LABELS+=("$label")
  JUST_STEP_CWDS+=("$(relative_path "$cwd")")
  JUST_STEP_COMMANDS+=("$command")
  JUST_STEP_DETAILS+=("$details")
  JUST_STEP_STATUSES+=("$status")
  JUST_STEP_DURATIONS+=("$duration")
  JUST_STEP_KINDS+=("$kind")
  JUST_STEP_EXTRAS+=("$extra")
}

step_status_label() {
  local status="$1"

  if [[ "$status" -eq 0 ]]; then
    printf "passed"
  else
    printf "failed(%s)" "$status"
  fi
}

print_lint_step_report() {
  local exit_status="$1"
  local index
  local total="${#JUST_STEP_LABELS[@]}"
  local passed=0
  local failed=0
  local status
  local preflight_count=0
  local tool_count=0

  [[ "$total" -gt 0 ]] || return "$exit_status"

  for status in "${JUST_STEP_STATUSES[@]}"; do
    if [[ "$status" -eq 0 ]]; then
      passed="$((passed + 1))"
    else
      failed="$((failed + 1))"
    fi
  done

  for ((index = 0; index < total; index++)); do
    if [[ "${JUST_STEP_KINDS[$index]}" == "preflight" ]]; then
      preflight_count="$((preflight_count + 1))"
    else
      tool_count="$((tool_count + 1))"
    fi
  done

  printf "\nCommand summary\n"
  printf "===============\n"
  printf "Total: %s, passed: %s, failed: %s\n" "$total" "$passed" "$failed"

  if [[ "$preflight_count" -gt 0 ]]; then
    printf "\n%-34s  %-10s  %-22s  %s\n" "Check" "Status" "Cwd" "Requirement"
    printf "%-34s  %-10s  %-22s  %s\n" "-----" "------" "---" "-----------"

    for ((index = 0; index < total; index++)); do
      [[ "${JUST_STEP_KINDS[$index]}" == "preflight" ]] || continue
      printf "%-34s  %-10s  %-22s  %s\n" \
        "${JUST_STEP_LABELS[$index]}" \
        "$(step_status_label "${JUST_STEP_STATUSES[$index]}")" \
        "${JUST_STEP_CWDS[$index]}" \
        "${JUST_STEP_EXTRAS[$index]}"
    done
  fi

  if [[ "$tool_count" -gt 0 ]]; then
    printf "\n%-18s  %-10s  %-22s  %-42s  %s\n" "Linter" "Status" "Cwd" "Cmd" "Files"
    printf "%-18s  %-10s  %-22s  %-42s  %s\n" "------" "------" "---" "---" "-----"

    for ((index = 0; index < total; index++)); do
      [[ "${JUST_STEP_KINDS[$index]}" != "preflight" ]] || continue
      printf "%-18s  %-10s  %-22s  %-42s  %s\n" \
        "${JUST_STEP_LABELS[$index]}" \
        "$(step_status_label "${JUST_STEP_STATUSES[$index]}")" \
        "${JUST_STEP_CWDS[$index]}" \
        "${JUST_STEP_COMMANDS[$index]}" \
        "${JUST_STEP_EXTRAS[$index]}"
    done
  fi

  return "$exit_status"
}

print_step_report() {
  local exit_status="$?"
  local index
  local total="${#JUST_STEP_LABELS[@]}"
  local passed=0
  local failed=0
  local status

  [[ "$total" -gt 0 ]] || return "$exit_status"

  if [[ "${JUST_STEP_REPORT_STYLE:-}" == "lint-table" ]]; then
    print_lint_step_report "$exit_status"
    return "$exit_status"
  fi

  for status in "${JUST_STEP_STATUSES[@]}"; do
    if [[ "$status" -eq 0 ]]; then
      passed="$((passed + 1))"
    else
      failed="$((failed + 1))"
    fi
  done

  printf "\nCommand summary\n"
  printf "===============\n"
  printf "Total: %s, passed: %s, failed: %s\n" "$total" "$passed" "$failed"

  for ((index = 0; index < total; index++)); do
    status="${JUST_STEP_STATUSES[$index]}"

    if [[ "$status" -eq 0 ]]; then
      printf "\n[ok] %s (%ss)\n" "${JUST_STEP_LABELS[$index]}" "${JUST_STEP_DURATIONS[$index]}"
    else
      printf "\n[failed exit %s] %s (%ss)\n" "$status" "${JUST_STEP_LABELS[$index]}" "${JUST_STEP_DURATIONS[$index]}"
    fi

    printf "  cwd: %s\n" "${JUST_STEP_CWDS[$index]}"
    printf "  cmd: %s\n" "${JUST_STEP_COMMANDS[$index]}"

    if [[ -n "${JUST_STEP_DETAILS[$index]}" ]]; then
      printf "%s\n" "${JUST_STEP_DETAILS[$index]}"
    fi
  done

  return "$exit_status"
}

run_step() {
  local label="$1"
  local cwd="$2"
  local command
  local display_command
  local details
  local started_at
  local finished_at
  local status
  local duration

  shift 2
  [[ "$#" -gt 0 ]] || die "run_step requires a command"

  command="$(command_string "$@")"
  display_command="${JUST_STEP_DISPLAY_COMMAND:-$command}"
  details="${JUST_STEP_DISPLAY_DETAILS:-}"
  printf "\n==> %s\n" "$label"

  started_at="$(date +%s)"

  if (cd "$cwd" && "$@"); then
    status=0
  else
    status="$?"
  fi

  finished_at="$(date +%s)"
  duration="$((finished_at - started_at))"
  record_step "$label" "$cwd" "$display_command" "$details" "$status" "$duration"

  return "$status"
}

collect_step() {
  if run_step "$@"; then
    return 0
  fi

  return 0
}

step_failure_count() {
  local status
  local failed=0

  for status in "${JUST_STEP_STATUSES[@]}"; do
    if [[ "$status" -ne 0 ]]; then
      failed="$((failed + 1))"
    fi
  done

  printf "%s\n" "$failed"
}

fail_if_steps_failed() {
  local failed

  failed="$(step_failure_count)"

  if [[ "$failed" -gt 0 ]]; then
    return 1
  fi

  return 0
}

trap print_step_report EXIT

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

require_config() {
  [[ -f "$JUST_CONFIG" ]] || die "missing just.yaml at $JUST_CONFIG"
}

resolve_path() {
  local path="$1"
  local caller_path
  local root_head
  local root_path

  if [[ "$path" = /* ]]; then
    printf "%s\n" "$path"
  elif [[ "$path" == ./* || "$path" == ../* ]]; then
    printf "%s/%s\n" "$JUST_CALLER_DIR" "$path"
  else
    caller_path="$JUST_CALLER_DIR/$path"
    root_path="$REPO_ROOT/$path"
    root_head="${path%%/*}"

    if [[ -e "$caller_path" ]]; then
      printf "%s\n" "$caller_path"
    elif [[ -e "$root_path" || -e "$REPO_ROOT/$root_head" ]]; then
      printf "%s\n" "$root_path"
    else
      printf "%s\n" "$caller_path"
    fi
  fi
}

relative_path() {
  local path="$1"

  if [[ "$path" == "$REPO_ROOT" ]]; then
    printf ".\n"
  elif [[ "$path" == "$REPO_ROOT/"* ]]; then
    printf "%s\n" "${path#"$REPO_ROOT"/}"
  else
    printf "%s\n" "$path"
  fi
}

yaml_app_entries() {
  require_config

  awk '
    function trim(value) {
      gsub(/^[ \t\r\n]+|[ \t\r\n]+$/, "", value)
      gsub(/^"|"$/, "", value)
      return value
    }

    /^[[:space:]]*#/ || /^[[:space:]]*$/ {
      next
    }

    /^[^[:space:]][^:]*:[[:space:]]*$/ {
      section = $0
      sub(/:.*/, "", section)
      section = trim(section)
      app = ""
      next
    }

    section == "apps" && /^[[:space:]][[:space:]][A-Za-z0-9_.-]+:[[:space:]]*$/ {
      line = $0
      sub(/:.*/, "", line)
      app = trim(line)
      next
    }

    section == "apps" && app != "" && /^[[:space:]][[:space:]][[:space:]][[:space:]]path:[[:space:]]*/ {
      line = $0
      sub(/[[:space:]]*#.*/, "", line)
      sub(/^[^:]*:/, "", line)
      line = trim(line)

      if (line != "") {
        print app " " line
      }
    }
  ' "$JUST_CONFIG"
}

app_entries() {
  local app
  local path

  while read -r app path; do
    [[ -n "$app" ]] || continue
    [[ -n "$path" ]] || continue

    if [[ -d "$REPO_ROOT/$path" ]]; then
      printf "%s %s\n" "$app" "$REPO_ROOT/$path"
    else
      printf "Skipping missing app path from just.yaml: %s (%s)\n" "$app" "$path" >&2
    fi
  done < <(yaml_app_entries)

  return 0
}

configured_app_names() {
  yaml_app_entries | awk '{print $1}' | paste -sd ' ' -
}

app_root() {
  local expected_app="$1"
  local app
  local path

  while read -r app path; do
    if [[ "$app" == "$expected_app" ]]; then
      [[ -d "$REPO_ROOT/$path" ]] ||
        die "app '$app' path does not exist: $path"
      printf "%s\n" "$REPO_ROOT/$path"
      return
    fi
  done < <(yaml_app_entries)

  die "unknown app '$expected_app'; expected one of: $(configured_app_names)"
}

app_entry_for_path() {
  local path="$1"
  local app
  local root

  while read -r app root; do
    if [[ "$path" == "$root" || "$path" == "$root/"* ]]; then
      printf "%s %s\n" "$app" "$root"
      return 0
    fi
  done < <(app_entries)

  return 1
}

package_has_script() {
  local root="$1"
  local script="$2"

  node -e '
    const fs = require("node:fs");
    const packagePath = process.argv[1];
    const script = process.argv[2];
    const pkg = JSON.parse(fs.readFileSync(packagePath, "utf8"));

    process.exit(pkg.scripts && pkg.scripts[script] ? 0 : 1);
  ' "$root/package.json" "$script"
}

elixir_roots() {
  local app
  local root

  while read -r app root; do
    [[ -f "$root/mix.exs" ]] && printf "%s %s\n" "$app" "$root"
  done < <(app_entries)

  return 0
}

node_apps() {
  local app
  local root

  while read -r app root; do
    [[ -f "$root/package.json" ]] && printf "%s %s\n" "$app" "$root"
  done < <(app_entries)

  return 0
}

elixir_root_for_path() {
  local path="$1"
  local app
  local root

  while read -r app root; do
    if [[ "$path" == "$root" || "$path" == "$root/"* ]]; then
      printf "%s\n" "$root"
      return 0
    fi
  done < <(elixir_roots)

  return 1
}

node_install_roots() {
  if [[ -f "$REPO_ROOT/package.json" || -f "$REPO_ROOT/pnpm-workspace.yaml" ]]; then
    printf ". %s\n" "$REPO_ROOT"
  else
    node_apps
  fi

  return 0
}
