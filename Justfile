set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

shfmt_files := "bootstrap dot-just/bin/workflow dot-just/commands/* dot-just/lib/*.sh spec/support/*.sh"
shell_files := "bootstrap dot-just/bin/workflow dot-just/commands/* dot-just/lib/*.sh spec/*_spec.sh spec/support/*.sh"

default: help

help:
    @just --list

[positional-arguments]
format *args:
    @if [[ "$#" -eq 0 ]]; then \
        cargo fmt --all; \
        just --fmt; \
        shfmt -w {{ shfmt_files }}; \
      elif [[ "$#" -eq 1 && "$1" == "--check" ]]; then \
        cargo fmt --all -- --check; \
        just --fmt --check; \
        shfmt -d {{ shfmt_files }}; \
      else \
        printf "usage: just format [--check]\n" >&2; \
        exit 2; \
      fi

lint:
    @cargo clippy --all-targets --all-features
    @cargo doc --all-features --no-deps
    @for file in {{ shell_files }}; do \
        bash -n "$file"; \
      done
    @shellcheck -x {{ shell_files }}

dependency-policy:
    @required="cargo-deny 0.20.2"; \
      install="cargo install cargo-deny --version 0.20.2 --locked"; \
      actual="$(cargo deny --version 2>/dev/null || true)"; \
      if [[ "$actual" != "$required" ]]; then \
        printf "%s is required (found: %s); install it with:\n  %s\n" \
          "$required" "${actual:-unknown}" "$install" >&2; \
        exit 1; \
      fi; \
      cargo deny --locked --all-features check

test:
    @cargo test --all-targets
    @shellspec

check:
    @just format --check
    @just lint
    @just dependency-policy
    @just test
