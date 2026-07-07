# Kok Jin Sam's Just Workflow

This is a personal `just` command surface for polyglot monorepos. It is not a
general-purpose `just` framework.

## Install

From a consuming repository root:

```bash
curl -fsSL https://raw.githubusercontent.com/kokjinsam/just/main/bootstrap | bash
```

Then import the managed workflow from the local `Justfile` and keep the default
recipe in that root file:

```just
import? '.just/kokjinsam.just'

default: help
```

The bootstrap installs managed workflow files under `.just/`:

```text
.just/kokjinsam.just
.just/bin/workflow
.just/commands/*
.just/lib/*
```

Regression tests for the shared workflow live only in this source repo:

```bash
just test
```

## Repository Config

Each consuming repository owns its local `just.yaml`. The shared scripts read it
to discover app paths:

```yaml
apps:
  my-app:
    path: apps/my-app
  workspace:
    path: apps/workspace
  website:
    path: apps/website
```

## Updating

After installation, refresh the managed recipes from the consuming repository
root:

```bash
just update recipes
```

To refresh from a specific branch, tag, or commit:

```bash
KOKJINSAM_JUST_REF=v0.1.0 just update recipes
```

For the first install, run the bootstrap:

```bash
KOKJINSAM_JUST_REF=v0.1.0 \
  curl -fsSL https://raw.githubusercontent.com/kokjinsam/just/main/bootstrap | bash
```
