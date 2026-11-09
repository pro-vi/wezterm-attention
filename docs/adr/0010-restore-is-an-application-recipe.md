# ADR 0010: Restore is an application recipe; Attention never spawns a pane or sends input

- **Status:** Accepted
- **Date:** 2026-11-09
- **Deciders:** maintainer

## Context

When a mux server stops, every agent in its panes stops with it. Attention's records still name each pane's agent session, its directory and its profile, and a reader of the launch post asked whether restore scripts come with Attention. Restoring means opening panes and typing a resume command into each, which is acting on panes. Attention's command and plugin record and draw; nothing in `src/` or `plugin/` spawns a pane or sends input to one, and the consumer guide says Attention "does not control the provider".

## Decision

Restore is an application recipe, `examples/restore.mjs`, beside the checkpoint recipe `examples/checkpoint.mjs`. It reads a checkpoint and Attention's records, prints a plan, and acts only with `--apply`. Then it opens new windows, tabs and splits, and types each resume line into a pane it opened in the same run, without pressing Enter. Attention's command and plugin keep to recording and drawing: neither spawns a pane or sends input to one.

## Rationale

An `attention restore` command was the alternative. It would make the record writer an actor on panes, so a fault in it could type into a pane, and every consumer would have to trust it with that. As a recipe, the acting code runs only when someone runs it, is read in one file, and can be copied and changed by an application with its own policy, such as account launchers or permission flags (`planRestore` takes a `resumeLine`). The repository already ships its application recipes this way, tested against the real command's output (`tests/rust/lifecycle_spec/consumer_recipes.rs`).

## Consequences

- A restore is opt-in and visible: the dry run shows every pane, outcome and line, and the person presses Enter in each restored pane.
- Restore depends only on stock WezTerm's `cli` (ADR 0007) and on Node.
- The recipe is example code: changes to it are not changes to Attention's public contract, and the checkpoint file's format is the recipe's own.
- Attention's command gains nothing to keep restore working; a consumer that wants a different restore writes its own from the same records.

## Revisit Triggers

- Restore is needed where Node is not available.
- Several applications reimplement the same acting logic and drift apart, so a shared, versioned contract would cost less than copies.

## References

- `examples/restore.mjs` (`planRestore`, `applyRestore`), `examples/checkpoint.mjs`.
- `tests/javascript/restore_recipe.mjs`: only panes the run created are typed into; nothing typed carries Enter.
- [Restore panes after a restart](../setup.md#restore-panes-after-a-restart).
