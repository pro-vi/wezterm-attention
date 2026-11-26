# ADR 0011: `provider` is an open identifier under CLI envelope schema 1

- **Status:** Accepted
- **Date:** 2026-11-25

## Context

`docs/consumer-guide.md` (`### Compatibility`) listed `provider` among the closed
sets of CLI envelope schema 1, where adding any new value required raising the
schema number and therefore the major product version.

Adding Antigravity CLI (`agy`) alongside `claude`, `codex`, and `pi` changes no
envelope layout, record schema (`record_schema: 3`), wire version (`wire_version: 2`),
`HookDelivery` schema (`1`), or `HoldCheck` schema (`1`). Bumping CLI schema 1 to
2 and cutting `2.0.0` per agent CLI would break provider-agnostic schema-1 readers
(`plugin/runtime.lua`, `examples/*.mjs`, and status scripts) even when none of the
fields they read changed.

## Decision

- **Reclassify `provider` as an open identifier under CLI envelope schema 1**,
  alongside `diagnostic_codes` and `observation.kind`. Adding a provider raises
  the minor product version and keeps every schema and wire version unchanged.
- **Keep record-layer validation manifest-driven.** Matching writers, plugin
  readers, and fixture validators still check records against the build's declared
  `enums.providers` set.
- **Keep lockstep upgrade order.** Update the `attention` writer, Lua plugin, and
  any strict provider-enumerating consumers before enabling live hooks for a new
  provider.

## Rationale

Bumping CLI schema 1 to 2 for each new agent CLI penalizes every provider-agnostic
consumer when no envelope field changed. Matching `diagnostic_codes` and
`observation.kind` lets open readers keep working untouched while strict
provider-specific controllers still upgrade before enabling live hooks.

## Consequences

- New providers ship as `1.x` minor releases without schema churn for
  provider-agnostic schema-1 consumers.
- The schema number no longer signals a new provider by itself, so strict
  provider-enumerating consumers must treat unknown values as unrecognized or
  upgrade before activating hooks for a newly added provider.

## Revisit Triggers

- Strict downstream consumers need a machine-readable schema bump to detect new
  providers rather than reading `hooks describe` or `protocol/v2.json`.

## References

- `docs/consumer-guide.md` (`### Compatibility`)
- `protocol/v2.json` (`enums.providers`), `Provider` in `src/protocol.rs`,
  `parse_v2_record` in `plugin/protocol.lua`, `parse_record` in
  `tests/fixtures/v2/check.py`
