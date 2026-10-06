# ADR 0007: Attention depends only on stock WezTerm

- **Status:** Accepted
- **Date:** 2026-10-05
- **Deciders:** maintainer

## Context

Some Attention costs trace back to WezTerm itself; for example, every pane's title change rebuilds the whole tab bar, which runs Attention's `format-tab-title` once for each tab. Such a cost can be fixed inside WezTerm, in a patched build. A fix made there would work only for people running that build, and Attention would then depend on code it does not ship.

## Decision

Attention works on stock WezTerm: an upstream `wezterm/wezterm` release at or after `20230320-124340-559cb7b0`, the first release with `wezterm.plugin.require`, with no patches applied. A fix for an Attention problem is made in Attention. A WezTerm change may make Attention cheaper to run, but no Attention behavior may depend on one.

Stock is checked by running `tests/shell/run_wezterm_smoke.sh` with an upstream release first on `PATH`, and `tests/shell/examples_spec.sh` with `ATTENTION_TEST_WEZTERM` set to that release's `wezterm`.

## Consequences

- Costs that sit in WezTerm, such as a tab bar rebuild on every title write, stay outside Attention's reach. Attention can document a setting that avoids them but cannot remove them.
- The smoke and examples checks cover loading and formatting on a stock release, not a live GUI window.

## Revisit Triggers

- A capability Attention needs exists only in a WezTerm nightly or a later release: raise the minimum release, do not patch.
- Upstream stops publishing releases.

## References

- `README.md`, Install: the minimum release.
- `tests/shell/run_wezterm_smoke.sh`, `tests/shell/examples_spec.sh`.
- Checked on upstream `20240203-110809-5046fc22`, 2026-10-05: the smoke test printed `ok`.
