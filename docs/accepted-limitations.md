# Accepted limitations

What this project knows is imperfect and has decided to ship anyway, with the
reason in each case. It is short on purpose: something leaves this file by being
fixed or by being reclassified as intended behaviour, not by being forgotten.

Most working notes — plans, review write-ups, triage records — stay on the
author's machine and are not in this repository. A few are tracked because
something still points at them: `hooks describe` names
`docs/reviews/lifecycle-contact-results.md` in its evidence output, for one. This
file is the maintained public account, and it is the one to read first.

## Panic paths are not denied by lint

`Cargo.toml` denies `dbg_macro`, `todo`, `unimplemented`, `unsafe_op_in_unsafe_fn`
and `unused_must_use`. It does not deny `clippy::unwrap_used` or
`clippy::expect_used`, which fire at 23 sites in the library.

Denying them today would mean adding 23 allow attributes, which announces an
intention while changing nothing. The sites need reading individually: some are
genuinely infallible and want a comment, some want an error path. Until that pass
happens, the table locks in the lints the crate already satisfies and says
nothing about the ones it does not.

A panic in a hook is not a clean abort. It interrupts processing wherever it
lands, so the state a consumer then reads can be missing, stale, or updated in
one file and not another. The record contract already declines to promise that
independently written files form one atomic snapshot; nothing rolls back on
failure. That is why this is a real gap rather than a stylistic preference.

## One timestamp field carries three roles

`observed_mono_ns` decides which of two competing writes publishes, serves as the
watermark an activity-clear compares against, and is the fence a parent `Stop`
writes to clear its subagents. `apply_activity` takes `surviving_order` from the
surviving activity and reuses it in the generated `subagent_clear` record, so the
coupling is semantic rather than a shared field name.

One consequence is known and characterised: when an incoming activity is
semantically equal to the published one, the write is skipped, the stored order
keeps its older value, and an event carrying a timestamp between the two can
still publish over it if it commits later. The interleaving is narrow and the
wrong tint clears on the next distinct activity.
`a_deduplicated_activity_does_not_advance_the_ordering_fence` in
`tests/rust/lifecycle_spec.rs` pins the current behaviour so a change to it is
deliberate.

Separating the roles is a record-contract change: a new field the Lua reader must
tolerate on records written before and after, schema validation, pruning in
`maintenance.rs`, reporting in `query.rs`, and a decision about which of the two
meanings the subagent-clear watermark actually wants. That is not a change to
make on the way out of the door.

## Consumer options belong to the hook invocation, not to each consumer

`--consumer` repeats, so one hook can deliver to several executables. The options
beside it do not repeat. `--consumer-timeout-ms` is one value applied to each
consumer in turn, and `--include-reply` and `--include-prompt` select content for
the single envelope every consumer receives.

Three things follow, and a consumer author should know all three before
registering a second executable:

- Consumers run in the order given, one after another, so the deadline multiplied
  by the number of consumers bounds the dispatch loop. It does not bound the hook:
  before the first consumer starts, the hook has already read its payload, applied
  the provider event and taken the record locks. That product is a floor for the
  hook's worst case, not the whole of it. All of it is spent inside a synchronous
  hook, with the agent waiting.
- A fast consumer and a slow one cannot be given different deadlines.
- Adding a consumer to a hook that already passes `--include-reply` or
  `--include-prompt` hands that text to the new executable as well. Content
  cannot be selected per consumer.

A failing consumer never stops the ones after it, and that part is genuinely per
consumer. `--strict` is not. It is one decision about the hook's exit code,
testing a single flag that any consumer which did not complete will raise -- and
that a native outcome of ignored, conflict or partial raises just the same. One
consumer cannot be marked as allowed to fail while another is not.

## The three validators disagree on how a version may be spelled

`schema` on an ordinary record and `wire` on a published identity carry the same
number in all three implementations, but not the same rule about its encoding.
`src/protocol.rs` requires an unsigned integer: `value.as_u64() == Some(...)`.
`plugin/protocol.lua` compares the decoded number, and the Python checker in
`tests/fixtures/v2/check.py` does the same. So `"schema": 3.0` is rejected by
Rust and accepted by the other two, and the disagreement is lexical -- it exists
only in the JSON text, and disappears once the value is decoded.

The branch already draws this distinction where it decided it mattered: the Lua
side scans lifecycle snapshots for canonical integers before decoding, and the
Python checker asserts the lifecycle schema's integer type. Ordinary records and
wire identity did not get the same treatment.

The Rust writer serialises through serde, which writes an integer, so it never
emits the disputed spelling.

The plugin also writes records, through the local `json_value` in
`plugin/overlays.lua` rather than `wezterm.json_encode`, and that renders a
number as `tostring(value)`. Those records carry `schema`, so what the plugin
emits depends on whether it is holding an integer or a float.

It is holding an integer. WezTerm converts a JSON number in
`lua-api-crates/serde-funcs/src/lib.rs`, trying `as_i64()` first and producing
`LuaValue::Integer`, and reaching `as_f64()` and `LuaValue::Number` only when
that fails. The manifest spells `record_schema` as `3`, so the plugin holds an
integer and `tostring` gives `3` under any Lua version. The float rendering that
would produce `3.0` does not arise here.

That leaves the divergence real but close to harmless: two readers accept a
spelling the third rejects, and nothing this project ships produces it. What is
still unverified is the installed WezTerm on a given machine, since the check
above was read from source rather than run, and there is no test that writes a
record through a real WezTerm and validates the resulting bytes with the Rust
validator. That round trip is the missing evidence, and the existing smoke does
not cover it -- it exercises reading and formatting, not the writer boundary.

Tightening the Lua and Python readers to match Rust is therefore a reasonable
change rather than a risky one, and it is deferred for sequencing rather than
danger: it wants the canonical rule stated, that write-and-validate round trip in
place, and raw-JSON fixtures for the integral-float and exponent spellings, since
a decoded fixture cannot express the difference.

## The acknowledgement write has no compare-and-swap

`write_v2_record` in `plugin/overlays.lua` reads the existing record only to
check that it is readable, then renames over it unconditionally. Two GUI writers
acknowledging at the same moment can lose one dismissal. This predates the v2
work and is not widened by it.

## The library's public surface is wider than its supported surface

`src/lib.rs` now names the supported entry points in its crate documentation and
classifies everything else as implementation. Read that first: it is the
declaration, and this section only explains why it is a declaration rather than
an enforced boundary.

`launch` is private and re-exported, which is the shape the rest of the crate
should follow. Most other modules are still public, `records` most consequentially
— it exposes locking, atomic replacement, path construction and durable deletion
because this crate's own tests drive them.

Documenting the boundary does not prevent an external program from compiling
against internals; only privacy does that. Making `records` private is not a
one-line change either, because the currently public, unstable
`read_pane_facts_with_ports` needs
publicly nameable reader types, and several test files mix white-box storage
tests with CLI subprocess tests in one module, so they cannot simply move inward.
The declaration turns an accidental commitment into an explicit unstable one; the
enforcement is separate work.
