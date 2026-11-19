---
name: vct-dev
description: >-
  Subsystem invariants, cross-file couplings, external API quirks, and verified empirical constraints for VibeCodingTracker. Load before planning, editing, or reviewing code in session parsers, providers, TUI layouts, quota panels, pricing, session/remote ledgers, config, logging, or self-update.
---

# VibeCodingTracker Subsystem Reference

Load only the reference file matching the area you are touching. Repo-wide boundaries, build/test commands, confidentiality, and hermetic test rules live in `AGENTS.md`.

| Code area                                                                                                                              | Reference                                                                  |
| -------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| `src/core/src/session/`, `src/core/src/scan/`, `src/core/src/usage/`, `src/core/src/analysis/`, `src/core/src/utils/`                  | [references/providers-and-parsers.md](references/providers-and-parsers.md) |
| `src/tui/src/display/`, `src/tui/src/layouts/`, `src/tui/build.rs`                                                                     | [references/tui-and-layouts.md](references/tui-and-layouts.md)             |
| `src/core/src/quota/`, `src/core/src/models/quota.rs`                                                                                  | [references/quota.md](references/quota.md)                                 |
| `src/core/src/pricing/`                                                                                                                | [references/pricing.md](references/pricing.md)                             |
| `src/core/src/ledger/`, `src/core/src/remote/`, `src/core/src/cache/`                                                                  | [references/ledger-and-remotes.md](references/ledger-and-remotes.md)       |
| `src/core/src/config/`, `src/core/src/update/`, `src/core/src/logging/`, `src/core/build.rs`, `src/cli/`, `cli/nodejs/`, `cli/python/` | [references/config-update-runtime.md](references/config-update-runtime.md) |
