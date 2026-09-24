# Changelog

## 1.0.0

This release adds the `attention` command, a Rust program that agent hooks and scripts call to record what a pane's agent is doing, and a plugin that reads those records across local panes and mux-attached GUIs. Plugin-only installs keep working with v1 flat markers.

The product version is 1.0.0. The record formats keep their own numbers: wire version 2, record schema 3, manifest schema 2, tab publication schema 2, CLI envelope schema 1. Untagged builds of the command reported `2.0.0` in `attention --version`; no 2.x release exists.

In these notes, "v1 flat markers" are the one-file-per-pane-id JSON files of 0.6, and "v2 records" are what the `attention` command writes. Neither name is a product version.

### Breaking

- **The plugin needs `wezterm.plugin.require`, so WezTerm `20230320-124340-559cb7b0` or newer.** 0.6 needed `20221119-145034-49b9839f`. Loading with `dofile` fails, because it passes no module path; load a clone with `loadfile(clone .. "/plugin/init.lua")("wezterm-attention", clone .. "/plugin/init.lua")` instead.
- **Panes in a mux-attached GUI need the `WEZTERM_PANE` user variable.** A GUI attached to a mux server numbers panes itself, so the plugin addresses such a pane only by the id it publishes. A remote pane that publishes nothing shows no indicator. The shell integration publishes it; see "Publishing the pane id" in the README.
- **The tab title changed shape.** 0.6 drew ` <indicator><number>: <dir> / <pane title> `. 1.0 draws ` <number>: <indicator><base title> `, and the base title is the tab's own title, else the directory, else a pane title that held for two polls, else the current pane title. `ctx.default_title` follows the same rule. `show_tab_index_in_tab_bar = false` drops the number.
- **`get_attention(id)` returns six values:** `type, frame, source, reserved, subagents, review`. 0.6 returned two. The fourth is always `false`; untagged builds returned a controller flag there.
- **The `review` flag lives in its own `<id>.review` file.** It no longer overwrites the marker file, so `Alt+B` works on a pane in any state. An old marker whose type is `review` is still read as the flag.
- **Installing the `attention` command changes what writers do.** Once it is built, the plugin exports `WEZTERM_ATTENTION_ROOT` to new panes, and the Pi extension writes v2 records through the command instead of v1 flat markers. Those writes need a launch claim: Pi started in a pane without one is refused and shows nothing.
- **`examples/hook.sh` writes only through the `attention` command**, and needs a launch claim. It no longer writes a flat marker; a copy of the 0.6 script still does.
- **In a pane with v2 records, v1 flat markers are not shown.** Once a launch claim has been published in a pane, a flat marker written under the same pane id is ignored. Custom producers should use `attention mark`.
- **The default state directory follows `XDG_STATE_HOME`.** The order is `WEZTERM_ATTENTION_DIR`, then `$XDG_STATE_HOME/wezterm-attention` when `XDG_STATE_HOME` is set, non-empty and absolute, then `~/.local/state/wezterm-attention`. 0.6 always used the last. If `XDG_STATE_HOME` is set where WezTerm starts, the directory moves; set `dir` to keep the old one.
- **`acknowledge_types` is not an option.** The option is `auto_clear`, as in 0.6; an earlier README named the wrong one. The plugin now logs `acknowledge_types` as unknown and names `auto_clear`.
- **Read commands print JSON by default.** `bindings`, `tabs`, `inspect` and `hooks describe` return the JSON envelope on terminals and pipes alike. This affects only scripts written against untagged builds; 0.6 had no command.
- **`attention mark clear --source NAME` also withdraws that source's activity**, when the activity the tab shows came from it, not only its review flag. The source name `user` is reserved for `Alt+B` and refused by every `mark` state.
- **Commands exit 0, 1 or 2, and never 3.** A query (`bindings`, `tabs`, `inspect`, `doctor`, `sweep`) exits 0 when its answer is complete and 1 when it is incomplete or failed; 2 is only for a command-line error of a non-hook command. Diagnostics alone no longer change the exit code, so a complete `bindings --all` with diagnostics exits 0, and a truncated one exits 1. Every other failure, including `mark`, `hooks claim` and `hooks publish`, exits 1, and so does `bin/attention` without the built binary for any non-hook command. Every error envelope has `complete=false`.
- **Hook commands never exit 2**, which Claude Code and Codex read as "block". `hooks event` exits 0 with the reason on stderr, or 1 under `--strict`, even for a command line it cannot parse or a malformed `--consumer` option. A usage error of `hooks describe`, `hooks claim` or `hooks publish`, or an unknown `hooks` subcommand, exits 1.
- **`sweep --apply` makes up its own operation id** and reports it in `result.operation_id`. Pass `--operation-id` only to retry an interrupted run; a reused id is a replay and ends nothing new.
- **`doctor` reports `unobserved`, not `healthy`, for a probe that had nothing to check**, and lists those probes in `result.unobserved`. A report where every probe but `versions` was unobserved says `unobserved`.
- **A shell claim from tmux, screen or another terminal inside a pane is refused** (`unsafe_tty`), so it can no longer take over the pane's claim.

### Added

- The `attention` command (`scripts/install-cli.sh` builds it into `libexec/`, `bin/attention` launches it). It records provider callbacks with `attention hooks event PROVIDER EVENT`, custom activity with `attention mark`, and answers queries with `bindings`, `inspect`, `tabs`, `doctor` and `sweep`. `attention --version` names the commit it was built from.
- Claude Code and Codex hook support, described by `attention hooks describe --provider claude|codex --json`, with copyable `settings.json` and `hooks.json` blocks in the README.
- Launch claims from the shell: bash claims `claude`, `codex` and `pi` automatically; zsh uses `wezterm_attention_claim && <agent>`.
- A pane identity that survives detach, reattach and several mux sockets, republished after a GUI reattaches.
- A subagent count: `✓+2` on a tab while two subagents in its pane are working.
- `get_attention_view(pane)`, with bounded lifecycle observations and request evidence, and the `on_view_change` callback. See `docs/consumer-guide.md`.
- Opt-in delivery of the exact prompt or reply text to consumer executables: `--consumer … --include-prompt` or `--include-reply`.
- The drawn tab order, published to `tabs/` and read with `attention tabs`.
- `attention sweep`, which previews by default and, with `--apply`, ends bindings whose panes are verified gone, removes a closed pane's whole tree once its binding ended more than 30 days ago and its absence is confirmed again, and collects leftover files. A server counts as gone only when that is shown: its `gui-sock-<pid>` process has exited, whether or not the GUI left its socket file behind, or the process probe read every process of this user and none carries the pane. A socket that was removed, replaced (including by `chmod` or `touch` on a live socket) or refuses connections is never a sighting by itself: a live server whose accept queue is full refuses too. So on macOS, and on Linux whenever a process of the user is non-dumpable, a mux server's records stay after its socket is removed, until you remove them by hand.
- A `doctor` probe named `environment`, which checks inside a pane that the pane's socket has a server identity hooks can find.
- `result.timing_ms` on `inspect` as on `bindings`, and `result.diagnostic_count` / `total_diagnostic_count` on `tabs`. Query diagnostics name the record path or pane they are about.
- Options `show_directory`, `settled_title_fallback`, `show_provider`, `on_view_change` and `integration_root`; `attention.doctor(window)` in the Lua API.

### Changed

- A prompt tints the pane `thinking` straight away, instead of waiting for the first tool call.
- `thinking` from v2 records animates like the v1 spinner.
- The tab text published in `tabs/*.json` always shows the spinner's first frame, so a spinning tab does not rewrite that file every second. The bar on screen still animates.
- Tab text is repaired before it is drawn or published: ill-formed UTF-8 from a title formatter becomes U+FFFD, and escape sequences and control characters are removed whole, so every window stays readable through `attention tabs`. A title formatter returns plain text: styling escapes in its return, such as `wezterm.format` output, are removed and their styling is dropped.
- In zsh, the prompt hook drops `WEZTERM_ATTENTION_LAUNCH_ID` once a claimed agent has exited, as bash already did.
- Alt+B clear-all never loses a review: a review written while the clear ran is kept, and a review a crash left moved aside is put back by the GUI that moved it, or by any GUI once it is older than a clear can take.
- A Claude turn that ends on an API error (`StopFailure`) shows `notify` instead of staying on `thinking` until its 30-minute timeout. A Codex `Interrupt` clears the activity.
- A sub-agent waiting for permission raises `notify` on its pane.
- Forked Claude and Codex sessions (`SessionStart` with source `fork`) are bound.
- A malformed optional field in a provider event is dropped with one diagnostic, instead of the whole event being ignored.
- Unknown options and wrong option types are named once in the WezTerm log and the default is used. An unknown `renderer` means `tab`.
- Exec, serial and WSL domains count as local, so their panes keep showing v1 flat markers by pane id, as in 0.6. On local panes the pane's own id wins over a user variable printed by terminal output.
- After a reattach, the plugin also republishes through WezTerm's implicit `unix` domain and any unix domain without a `socket_path`.
- The plugin creates the state directory and `tabs/` private (mode 0700).
- `attention` never runs `wezterm-gui` or `wezterm-mux-server` as the CLI. It finds `wezterm` on PATH, beside `$WEZTERM_EXECUTABLE`, or in `/Applications/WezTerm.app`, passes `--no-auto-start` to every `wezterm cli` call, and kills a call past its deadline with its whole process group. A failed pane listing names the executable and says how it failed, for example `timed out after 5000 ms`.
- The process probe no longer runs `ps` on macOS or Linux: it reads this user's process environments directly, never their arguments. Linux (glibc, including aarch64) is supported; macOS and Linux are the tested platforms.
- `inspect` computes `binding_health` and `reader_confidence` with the same rule as `bindings`, so it can now report `conflicted`.
- Realm-wide `bindings` applies `--realm` and `--provider` before asking any socket, asks sockets in parallel, and reports an unreadable state directory as an incomplete answer.
- Without `--json`, `doctor`, `sweep`, `mark` and `hooks publish` print each diagnostic on stderr as `attention: <code>: <message>`; stdout still carries only the status word.
- JSON output escapes U+0080–U+009F. `attention --version` says `-dirty` only when source, protocol or build files differ from the commit.
- Sweep checks a pane before taking its locks, so hooks are not held up behind a slow mux.

### Fixed

- `attention mark clear --source NAME` also withdraws an activity that `attention mark` wrote before any provider session bound the launch. Another source's activity stays.
- A sub-agent waiting for permission keeps the tab on `notify` while the lead keeps calling tools, for example Codex polling `wait_agent`. The notify ends when that sub-agent calls its next tool or stops, when the user prompts, or when the lead's turn ends, however long the sub-agent waits. A sub-agent record that cannot be read does not stop the notify; the hook reports `partial`.
- Sourcing the bash integration again from an rc file that assigns `PROMPT_COMMAND` puts its prompt hook back.
- Ctrl-C while a claim or prompt publication is running no longer stops the bash and zsh integrations for the rest of the shell, except under bash-preexec on bash 3.2 (see accepted limitations). In zsh, a Ctrl-C during the publication at an agent's exit still drops the agent's launch id.
- `scripts/install-cli.sh` installs the binary cargo reports building, also when `CARGO_BUILD_TARGET` or `build.target` is set, and refuses with a message when cargo reports none.
- A window's first tab order waits for the GUI's source answer, including across failed runs, for up to the whole retry backoff (about 47 s), so `attention tabs` never lists a window twice. If every retry fails, held and new windows are published without a source, and this is logged once.
- Alt+B clear recovery restores only its own leftovers or ones older than a clear can take, so it never undoes another GUI's clear.
- A local pane's published identity must belong to the GUI's own mux, so output from another mux cannot make a local pane show or acknowledge that mux's records.
- A window the plugin published without a source before a config reload is published under its source after it, and the plugin removes the unsourced file it wrote for that window, so `attention tabs` does not list the window twice after the install steps. A file another GUI has rewritten since stays.
- The plugin and the Pi extension ignore a `WEZTERM_ATTENTION_DIR` or `XDG_STATE_HOME` that is not UTF-8, and say so in their logs, as the attention command refuses it, instead of each reading a directory the command never writes. The plugin's `dir` option is held to the same rule.

- Sourcing the bash integration twice no longer crashes the shell, and it works beside bash-preexec. It keeps `$?` and `$_` for later prompt commands and commands.
- The shell integration stays quiet outside a WezTerm pane, and `wezterm_attention_claim && claude` starts the agent there.
- `bin/attention` without the built binary exits 0 for hook commands (1 under `--strict`), and works through a symlinked `bin` directory.
- `scripts/install-cli.sh` builds into `./target` even when `CARGO_TARGET_DIR` or `build.target-dir` points elsewhere.
- Every text check refuses C1 control characters (U+0080–U+009F) as well as C0 and DEL, and the plugin strips control characters from tab text it draws or publishes.
- A long or control-character tab title no longer drops its whole window from `attention tabs`.
- Concurrent writers creating the same state directory no longer fail.
- A Codex `Stop` with `last_assistant_message: null` reports the reply as `absent`, not `invalid`.
- `examples/wezterm.lua` keeps `Alt+B`, loads without `follow-up.lua`, and runs git without repository hooks.
- A query against a stale socket no longer starts a new mux server, and a missing `wezterm` on PATH no longer leads to running the mux server in its place.
- On Linux, closed panes can be verified absent; the crate builds on aarch64 Linux.
- A non-UTF-8 environment variable is skipped instead of stopping every command. A non-UTF-8 `WEZTERM_ATTENTION_DIR` is refused as a relative one is, rather than skipped in favour of the default state root.
- A closed stdout (`| head`) no longer makes a command panic.
- A symlinked `tabs/` directory is refused, and sweep never deletes through it.
- Temporaries left by an interrupted write no longer keep a binding or pane tree from retention.
- The process probe matches a socket path by its resolved directory, so a process that names the socket through `/tmp` on macOS or a symlinked directory is still found.
- Every command reads a recorded server the same way. An exited GUI's bindings end and, after the retention age, its pane trees go, recognised by its `gui-sock-<pid>` process being gone. A socket that is gone (`socket_gone`, a new diagnostic code), replaced (`incarnation_changed`) or refusing connections (`socket_refused`, a new diagnostic code) with no proof that its server exited keeps its records, and the same proofs, the process listing included, reclaim it; `doctor` and `sweep` report that history as one diagnostic per code listing each incarnation, its directory and pane count, and stay complete however much there is. `inspect`, `bindings --socket` and `tabs` (`window_check.reason: socket_gone`) report a removed socket the same way, not as `probe_unavailable`; `inspect` answers a refusing scope as `unavailable`, as it does a removed one, and `tabs` also calls a GUI whose `gui-sock-<pid>` process is gone `socket_gone`.
- `doctor` is incomplete and exits 1 when a mux whose socket still carries its incarnation does not answer its pane listing, as `sweep` already was.
- Sweep's absence and retention diagnostics name the realm, incarnation, pane and binding, and each appears once per run.
- A tab-order file naming a pane whose mux did not answer makes `sweep` incomplete (exit 1), as a binding's pane does.
- A tab-order file naming any pane whose absence sweep could not decide, including one whose pane listing was malformed or whose process probe did not answer, makes `sweep` incomplete, as the pane's binding does.
- `sweep --apply` against a mux that accepts connections and never answers waits out one listing deadline for that socket, not two per pane.
- An absence probe that sweep keeps because it is outside the state root is listed as an `absence` detail with action `keep`, as pane retention lists it, instead of leaving no detail.
- A non-UTF-8 `XDG_STATE_HOME` is refused as a non-UTF-8 `WEZTERM_ATTENTION_DIR` is, when it is the variable that decides the state root, rather than skipped in favour of `~/.local/state/wezterm-attention`. Both refusals say the value is not UTF-8.
- `bindings --socket` and `inspect` find other bindings of a provider session through a session index (`v2/sessions/`, record kinds `session_binding` and `session_index`) instead of reading every binding in the store: at 3000 stored bindings, 0.4 ms instead of about 390 ms. Each bind writes its entry. A store that already has bindings keeps the full walk until one `attention sweep --apply` has given every binding its entry.
- A binding that sweep ended after a reboot reads as ended in `bindings`, `inspect` and later sweeps, which report `already_ended` rather than writing its end again, and its pane tree is then retained by the usual rule. A binding-end record names the binding event it ends in the new optional field `binding_event_id`, which orders it whatever the restarted monotonic clock says.
- The example status bar shortens a branch name by cell width, so a non-ASCII branch name no longer stops the right status updating.
- `examples/wezterm.lua` loads in `wezterm-mux-server`, which reads the same file but has no `wezterm.gui`, so a mux server's panes get the attention environment and are claimed.
- The gate's performance comparison measures the binaries cargo reports building; `scripts/build-attention.sh` holds the build step the installer and the gate share.
- The lock that `mark review`, `mark clear` and Pi's review events leave in `reviews/` no longer keeps an old pane's tree from retention.
- Sweep removes nothing through a symlinked directory below the state root: subagent compaction and a cleared absence probe are refused there, as binding and pane removals already were.
- `bindings --socket` reports a directory it could not read as `state_permissions` with its `path`, as realm-wide `bindings` does. A `binding_conflict` diagnostic names the provider session and its pane addresses, and a sweep diagnostic about a tab-order file's pane names the file and the pane.

### Upgrading from 0.6

1. Update the plugin with `wezterm.plugin.update_all()`, then build the command in the copy WezTerm loads; the README's Install section has the command. **Run `scripts/install-cli.sh` again after every `update_all`**: updating the plugin replaces the Lua files and does not rebuild the command.
2. Register the Claude Code and Codex hooks from the README, and remove any 0.6 hook scripts that wrote flat marker files for the same events.
3. Source the shell integration, guarded, as `docs/mux-setup.md` shows.
4. Remove the flat files 0.6 left behind, unless some program you still use writes v1 flat markers. Pane ids restart from 0 with every mux, so an old completion can show on a new, unfocused pane that reuses its id. First stop every WezTerm process, mux servers included. Then, in the state directory 0.6 used (`~/.local/state/wezterm-attention`, or the `dir` you set), list and remove the bare-numeric files `<id>`, `<id>.ack`, `<id>.agents` and `<id>.review`. The `.review` files are your own `Alt+B` flags.

   ```sh
   root="$HOME/.local/state/wezterm-attention"   # or the dir you configured
   find "$root" -maxdepth 1 -type f | grep -E '/[0-9]+(\.(ack|agents|review))?$'
   find "$root" -maxdepth 1 -type f | grep -E '/[0-9]+(\.(ack|agents|review))?$' |
     while IFS= read -r file; do rm -f -- "$file"; done
   ```

5. If you set `acknowledge_types`, rename it to `auto_clear`. If your `title_formatter` parsed `ctx.default_title` as `dir / title`, read `ctx.directory` and `tab.active_pane.title` instead. If your `title_formatter` returned `wezterm.format` output for styling, return plain text instead; set colors through the plugin's `colors` option.
