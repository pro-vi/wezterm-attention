# ADR 0011: `provider` is an open identifier under CLI envelope schema 1

- **Status:** Accepted
- **Date:** 2026-11-25

## Context

`docs/consumer-guide.md` (`### Compatibility`) listed `provider` among the closed
sets of CLI envelope schema 1, where adding any new value required raising the
schema number and therefore the major product version.

Adding Antigravity CLI (`agy`) as a fourth provider alongside `claude`, `codex`,
and `pi` does not change the envelope layout, record schema (`record_schema: 3`),
wire version (`wire_version: 2`), `HookDelivery` schema (`1`), or `HoldCheck`
schema (`1`). Bumping CLI schema 1 to 2 and cutting major release `2.0.0` for
every new agent CLI would break every schema-1 consumer — including open-provider
readers such as `blackboard`, Attention's own `plugin/runtime.lua` `tab-source`
reader, `examples/*.mjs`, and `bootstrap`'s shell and Lua scripts — even when
none of their record or envelope shapes changed.

On 2026-11-24 the maintainer decided: *"no 2.0 pls.. a new provider should not
push a major version.."*

## Decision

- **Reclassify `provider` as an open identifier under CLI envelope schema 1**,
  alongside `diagnostic_codes` and `observation.kind`. Adding a provider raises
  the minor product version, never the CLI schema, record schema, wire version,
  or major version.
- **Keep record-layer validation manifest-driven.** `protocol/v2.json`
  `enums.providers`, `Provider::parse` in `src/protocol.rs`, `plugin/protocol.lua`,
  and `tests/fixtures/v2/check.py` still validate records against the build's
  declared provider set so matching writers and readers agree.
- **Upgrade discipline stays unchanged.** Update Attention's writer, Lua plugin,
  and strict downstream consumers (`wezpup`, `bootstrap`) before registering live
  hooks for a new provider, so no strict schema-1 reader encounters records for a
  provider it does not recognize yet.

## Rationale

An open-identifier rule matches how `diagnostic_codes` and `observation.kind`
already evolve in `docs/consumer-guide.md`: new values are declared in
`protocol/v2.json` and announced in `CHANGELOG.md` on a minor release, while
consumers that treat `provider` as an opaque label keep working untouched and
strict provider-specific controllers upgrade in lockstep before enabling hooks
for the new agent.

## Consequences

Positive:

- Adding `agy` (and future agent CLIs) ships as a `1.x` minor release with zero
  schema churn for `blackboard`, `plugin/runtime.lua`, `examples/*.mjs`, and
  provider-agnostic schema-1 consumers.
- `HookDelivery` and `HoldCheck` remain at schema `1` (additive fields such as
  optional `HookDelivery.observation_kind` stay within schema `1`).

Negative:

- Strict schema-1 consumers that enumerate known providers (`wezpup`
  `Provider::parse`, `bootstrap` `ci-lint.sh` f14 and hook consumers) must
  reject unknown providers gracefully or upgrade before activating hooks for a
  newly added provider; the schema number no longer signals a new provider by
  itself.

## Revisit Triggers

- A future provider requires a breaking change to record structure, identity
  hashing, or CLI envelope fields — that change still raises the corresponding
  schema or wire version and major release.

## References

- `docs/consumer-guide.md` (`### Compatibility`)
- `protocol/v2.json` (`enums.providers`), `Provider` in `src/protocol.rs`,
  `parse_v2_record` in `plugin/protocol.lua`, `parse_record` in
  `tests/fixtures/v2/check.py`
