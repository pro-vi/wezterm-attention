# Accepted limitations

What this project knows is imperfect and has decided to ship anyway, with the
reason in each case. It is short on purpose: something leaves this file by being
fixed or by being reclassified as intended behaviour, not by being forgotten.

Planning documents and review write-ups are not tracked in this repository. That
is why this file exists — a reader of a fresh clone should be able to find the
known compromises without access to the author's machine.

## Panic paths are not denied by lint

`Cargo.toml` denies `dbg_macro`, `todo`, `unimplemented`, `unsafe_op_in_unsafe_fn`
and `unused_must_use`. It does not deny `clippy::unwrap_used` or
`clippy::expect_used`, which fire at 23 sites in the library.

Denying them today would mean adding 23 allow attributes, which announces an
intention while changing nothing. The sites need reading individually: some are
genuinely infallible and want a comment, some want an error path. Until that pass
happens, the table locks in the lints the crate already satisfies and says
nothing about the ones it does not.

A hook binary that panics writes no record and the tab stays wrong, so this is a
real gap, not a stylistic preference.

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

## The acknowledgement write has no compare-and-swap

`write_v2_record` in `plugin/overlays.lua` reads the existing record only to
check that it is readable, then renames over it unconditionally. Two GUI writers
acknowledging at the same moment can lose one dismissal. This predates the v2
work and is not widened by it.

## The library's public surface is wider than its supported surface

`src/lib.rs` keeps `launch` private and re-exports its entry points, which is the
shape the rest of the crate should follow. Most other modules are public,
including storage machinery that exists to implement the supported operations
rather than to be called directly. Narrowing this after downstream Rust code
adopts it becomes a migration; it is listed here so the choice is visible rather
than accidental.
