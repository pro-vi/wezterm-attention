# ADR 0009: A Codex host's home is named only when its start-up environment and its arg0 lock agree

- **Status:** Accepted
- **Date:** 2026-10-13

## Context

A program that acts on a Codex pane needs the Codex home the pane's agent uses.
`binding.config_dir` cannot give it: it is the `CODEX_HOME` the binding's first
start hook saw, frozen with the binding and absent for an agent started without
one, and a consumer keys launchers on that meaning.

As of Codex 0.162.0 and 0.162.1 the start-up environment does not fix that
home. `load_dotenv` in `codex-rs/arg0/src/lib.rs` reads the `.env` in the home
that environment names and lets it set any variable but `CODEX_*`, `HOME`
included, before Codex resolves its home again. With a generated `.env` that set
`HOME`, Codex loaded its configuration from, and wrote its session under, the
home the `.env` named while its start-up environment still named the first one;
a rule read from that environment alone named the wrong home, for `codex exec`
and for a TUI host Attention had admitted.

After that load, `prepare_path_entry_for_codex_aliases` resolves the home again
and holds `<codex home>/tmp/arg0/codex-arg0<suffix>/.lock` open while Codex
runs, unless the home lies under `TMPDIR`. In every generated case the one such
open file lay in the home Codex used.

## Decision

Scoped `inspect` gives a Codex binding's home in its `host` facet `record` as
exactly one of `config_dir` and `config_dir_refusal`, read inside the host
check (`effective_codex_home` in `src/host.rs`). It resolves the home the host's
start-up environment names by Codex's own rule, keeping only `CODEX_HOME` and
`HOME`; finds the arg0 helper locks among the host's open files by name; and
names the home only when the lock lies in that directory by device and inode,
the lock and the home are still at their paths, and the host's start time reads
the same afterwards. Every other outcome is one refusal from
`CodexHomeRefusal`, listed in `protocol/v2.json` as `enums.codex_home_refusals`.
Nothing is stored.

## Alternatives

- **The start-up environment alone.** It names the wrong home when a `.env`
  sets `HOME`.
- **The arg0 lock alone.** It was right in every measured case and needs no
  environment read, but it rests on where Codex places a helper. A later Codex
  that kept that file name in another directory would be reported as a wrong
  home rather than refused, and consumers admit official Codex builds they have
  not tested.
- **Refusing whenever the default home holds a `.env`.** It refuses a `.env`
  that sets nothing relevant, cannot see one removed after Codex started, and
  has to look at a file that may hold secrets.
- **Recording the home a hook sees.** A host recorded before the change has
  none until its next hook, and as of Codex 0.162.0 a hook's environment is a
  snapshot Codex replays through a login shell, not the process's own.

## Consequences

- A relative `CODEX_HOME` or `HOME`, a `.env` that moves `HOME`, a home under
  `TMPDIR` and a home whose path is not UTF-8 are refused although Codex uses
  them; the accepted limitations list them.
- A Codex that stops creating the lock is refused rather than read from its
  environment alone.
- `inspect` reads more of a verified host than before: two environment
  variables, the names of its open files, of which only the lock names are
  kept, and, when `HOME` is empty or unset, its user's home in the user
  database.
- Doctor's rule for shared Codex servers is separate and unchanged.

## Revisit

When a Codex release moves the arg0 helper or changes how it loads `.env`: the
consumer guide and `start_home` name the versions the rule was read from. When a
consumer needs a home for a host that has no lock, such as one under `TMPDIR`.
