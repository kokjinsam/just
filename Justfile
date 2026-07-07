set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

shfmt_files := "bootstrap dot-just/bin/workflow dot-just/commands/* dot-just/lib/*.sh spec/support/*.sh"
shell_files := "bootstrap dot-just/bin/workflow dot-just/commands/* dot-just/lib/*.sh spec/*_spec.sh spec/support/*.sh"

default: help

help:
    @just --list

[positional-arguments]
format *args:
    @if [[ "$#" -eq 0 ]]; then \
        just --fmt; \
        shfmt -w {{ shfmt_files }}; \
      elif [[ "$#" -eq 1 && "$1" == "--check" ]]; then \
        just --fmt --check; \
        shfmt -d {{ shfmt_files }}; \
      else \
        printf "usage: just format [--check]\n" >&2; \
        exit 2; \
      fi

lint:
    @for file in {{ shell_files }}; do \
        bash -n "$file"; \
      done
    @shellcheck -x {{ shell_files }}

test:
    @shellspec

check:
    @just format --check
    @just lint
    @just test
