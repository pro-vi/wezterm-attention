# ADR 0001: Check tab publications in their publishing GUI's namespace

- **Status:** Accepted
- **Date:** 2026-09-22

## Context

A GUI window's numeric ID is local to its process. The panes named by its saved
tab order can belong to remote muxes and can survive that GUI window. Matching
those panes against an arbitrary mux cannot establish the original window's
presence. The publisher must record the namespace that gave the window its ID.

## Decision

Source-identified tab publications use independent schema 2 and filenames containing
the publishing socket's incarnation and window ID. `read_tab_source` derives the
source using the existing socket identity algorithm. The Lua publisher acquires it
in a separate asynchronous status callback, outside pane-state polling and rendering.

`read_checked_tab_publications` queries that exact GUI socket without auto-start,
checks its incarnation before and after, and shares one inventory per source per
invocation. `WindowCheck` reports `present`, `not_listed`, or `unavailable`.
Because CLI output lists panes, `not_listed` does not prove an empty window closed.
The check does not certify saved tab order or current-workspace visibility.

Failed checks remain per-window observations; global completeness describes reading
the published set. Queries do not delete files. Schema-1 files remain readable but
unattributed. No default socket, surviving remote pane, heartbeat age, or matching
numeric ID supplies the missing identity.

## Consequences and revisit

Readers must support schema 2 before the updated publisher is loaded. Legacy files
can coexist with source-identified files and must not be guessed away. Revisit if
WezTerm exposes an authoritative, source-identified live GUI tab-order API, or if
measurements justify a different inventory transport.

Enforced by `window_check_rejects_source_replacement_before_and_during_listing`,
`failed_window_inventory_never_means_absence_or_hides_another_source`,
`lua_tab_publication_round_trips_identity`, `sweep_uses_validated_tab_publication_path`,
and `disposable_gui_publishes_its_own_source` in the Rust integration suites.
