# Changelog

## 1.0.0

This release adds the `attention` command, a Rust program that agent hooks and scripts call to record what a pane's agent is doing, and a plugin that reads those records across local panes and mux-attached GUIs. The plugin shows only what the command records, so the two are installed together.

The product version is 1.0.0. The record formats keep their own numbers: wire version 2, record schema 3, manifest schema 2, tab publication schema 2, CLI envelope schema 1. Untagged builds of the command reported `2.0.0` in `attention --version`; no 2.x release exists.

In these notes, "v1 flat markers" are the one-file-per-pane-id JSON files of 0.6, and "v2 records" are what the `attention` command writes. Neither name is a product version.

### Breaking

- **The plugin needs `wezterm.plugin.require`, so WezTerm `20230320-124340-559cb7b0` or newer.** 0.6 needed `20221119-145034-49b9839f`. Loading with `dofile` fails, because it passes no module path; load a clone with `loadfile(clone .. "/plugin/init.lua")("wezterm-attention", clone .. "/plugin/init.lua")` instead.
- **Panes in a mux-attached GUI need the `WEZTERM_PANE` user variable.** A GUI attached to a mux server numbers panes itself, so the plugin addresses such a pane only by the id it publishes. A remote pane that publishes nothing shows no indicator. The shell integration publishes it; see "Publishing the pane id" in the README.
- **The tab title changed shape.** 0.6 drew ` <indicator><number>: <dir> / <pane title> `. 1.0 draws ` <number>: <indicator><base title> `, and the base title is the tab's own title, else the directory, else a pane title that held for two polls, else the current pane title. `ctx.default_title` follows the same rule. `show_tab_index_in_tab_bar = false` drops the number.
- **`get_attention(id)` returns six values:** `type, frame, source, reserved, subagents, review`. 0.6 returned two. The fourth is always `false`; untagged builds returned a controller flag there.
- **The flat format is neither read nor collected.** The plugin reads only v2 records. The v1 flat markers at the top of the state directory, `<id>`, `<id>.ack`, `<id>.agents` and `<id>.review`, are ignored, and `attention sweep` no longer collects them: its `projection_collection` detail kind is gone. A producer that writes flat markers shows nothing; use `attention mark`. The plugin deletes nothing when a pane closes. `remove_marker`, the `dir` option of `get_attention`, and the `stale_after_ms` and `format_tab_title = false` options are removed; `apply_to_config` names the two options as ignored, with the reason. Remove the files 0.6 left behind by hand, as step 4 of "Upgrading from 0.6" shows.
- **The Pi extension writes only through the `attention` command.** Where `WEZTERM_ATTENTION_ROOT` is unset (the command is not built, or the pane was opened before it was), Pi records nothing and shows one warning saying so; `PI_WEZTERM_ATTENTION_TTL_MS` is gone with the flat writer. The plugin exports `WEZTERM_ATTENTION_ROOT` to new panes once the command is built. Pi's writes need a launch claim. On macOS Pi claims its own pane (see Added); elsewhere Pi started in a pane without a shell claim is refused and shows nothing.
- **The `review` flag is a review record owned by `user`**, which the plugin writes through the `attention` command. `Alt+B` works on a pane in any state, and toggles only that owner's review: a press on a tab where any pane carries your flag clears it from all of them, and otherwise flags the focused pane. A review another source published (`attention mark review --source NAME`, or Pi's bus as `pi-bus`) also shows ◆, and `Alt+B` leaves it; `attention mark clear --source NAME` withdraws it. A pane no launch has claimed cannot be flagged.
- **`examples/hook.sh` and `examples/hook.ts` are removed.** Register `attention hooks event PROVIDER EVENT`, or run `attention mark STATE --source NAME`, from the `attention` link on PATH. A copy of the 0.6 `hook.sh` still writes flat markers, which nothing reads.
- **The Pi extension no longer accepts `busy`, `ready`, `blocked` or `pending` on the `wezterm-attention:mark` bus.** 0.6 documented them as aliases; emit `thinking`, `stop` or `notify` instead.
- **`attention hooks publish --all-details` is removed.** `hooks publish` reports at most 50 diagnostics, and `complete` says whether they all fit. Passing the flag is a usage error.
- **The crate builds only for macOS and Linux.** Other targets fail to compile; the `ps axeww` process-probe fallback is removed.
- **The Rust crate is not a supported interface.** The supported interface is the `attention` command, with its JSON envelopes, diagnostic codes and exit codes, and the plugin's Lua API. A program that links the crate takes whatever the next commit changes.
- **The hidden `attention claim` command is gone.** It only said to use `attention hooks claim`. The results of `mark --json` and `hooks event --debug` no longer carry `repaired_projection`, which was always `false`, and `repaired_projection` is no longer a disposition.
- **The default state directory follows `XDG_STATE_HOME`.** The order is `WEZTERM_ATTENTION_DIR`, then `$XDG_STATE_HOME/wezterm-attention` when `XDG_STATE_HOME` is set, non-empty and absolute, then `~/.local/state/wezterm-attention`. 0.6 always used the last. If `XDG_STATE_HOME` is set where WezTerm starts, the directory moves; set `dir` to keep the old one. A `dir` option or `WEZTERM_ATTENTION_DIR` that is not UTF-8 is ignored with a warning by the plugin.
- **`acknowledge_types` is not an option.** The option is `auto_clear`, as in 0.6; an earlier README named the wrong one. The plugin now logs `acknowledge_types` as unknown and names `auto_clear`.
- **Hook commands registered for untagged builds must be registered again for an agent to claim its own pane.** A plain `attention hooks event PROVIDER EVENT` registration asserts no host pid, so every event of an agent no shell claimed for is refused with `self_claim_parent_unverified` and its tab shows nothing. Register each command as `WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event PROVIDER EVENT`, as the README shows. This affects only registrations written for untagged builds; 0.6 had no command.
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
- On macOS an agent claims its own pane, so zsh needs no `wezterm_attention_claim`. The first session-start hook of Claude Code, Codex or Pi in a pane with no claim proves the pane from the kernel's process and terminal records and the mux listing, then claims it for the agent's process. Later events are proven against that claim without asking the mux again. The claim's launch id is the agent's alone: `hooks claim` refuses (`claim_stale`) rather than hand it to a shell, and an inherited launch id never matches it. Register hook commands as `WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event PROVIDER EVENT`; the Pi extension sets the pid itself. A hook without that assertion, or whose parent is not the pid it asserts, is refused with `self_claim_parent_unverified`. `WEZTERM_ATTENTION_ENABLE_SELF_CLAIM=0` turns it off; Linux keeps the shell claim. The shell integration's prompt hook clears such an agent's last activity once the agent's process is proven gone, so an agent that exits while its activity still stands does not leave it on the tab.
- Claim records may name the process that owns them: `owner_pid`, `owner_started_sec`, `owner_started_usec` and `owner_boot_session_id`, all four or none. The record schema stays 3.
- A pane identity that survives detach, reattach and several mux sockets, republished after a GUI reattaches.
- A subagent count: `✓+2` on a tab while two subagents in its pane are working.
- `get_attention_view(pane)`, with bounded lifecycle observations and request evidence, and the `on_view_change` callback. See `docs/consumer-guide.md`.
- Opt-in delivery of the exact prompt or reply text to consumer executables: `--consumer … --include-prompt` or `--include-reply`.
- The drawn tab order, published to `tabs/` and read with `attention tabs`.
- `attention sweep`, which previews by default and, with `--apply`, ends bindings whose panes are verified gone, removes a closed pane's whole tree once its binding ended more than 30 days ago and its absence is confirmed again, and collects leftover files. A server counts as gone only when that is shown: its `gui-sock-<pid>` process has exited, whether or not the GUI left its socket file behind, or the process probe read every process of this user and none carries the pane. A socket that was removed, replaced (including by `chmod` or `touch` on a live socket) or refuses connections is never a sighting by itself: a live server whose accept queue is full refuses too. So on macOS, and on Linux whenever a process of the user is non-dumpable, a mux server's records stay after its socket is removed, until you remove them by hand.
- A `doctor` probe named `environment`, which checks inside a pane that the pane's socket has a server identity hooks can find. Where an agent can claim its own pane, an identity nothing has published yet is `unobserved` rather than a finding, because the agent's first session start publishes it.
- `result.timing_ms` on `inspect` as on `bindings`, and `result.diagnostic_count` / `total_diagnostic_count` on `tabs`.
- Options `show_directory`, `settled_title_fallback`, `show_provider`, `on_view_change` and `integration_root`.

### Changed

- Usage errors for `bindings --limit`, `--realm` and `--provider`, `sweep --operation-id` without `--apply`, and `hooks publish --json --quiet` carry clap's message text. The `bad_usage` code, exit codes and JSON envelopes are unchanged; without `--json`, `sweep` prints clap's error text instead of `attention: bad_usage: …`.
- A prompt tints the pane `thinking` straight away, instead of waiting for the first tool call.
- `thinking` from v2 records animates like the v1 spinner.
- The tab text published in `tabs/*.json` always shows the spinner's first frame, so a spinning tab does not rewrite that file every second. The bar on screen still animates.
- Tab text is repaired before it is drawn or published: ill-formed UTF-8 from a title formatter becomes U+FFFD, and escape sequences and control characters are removed whole, so every window stays readable through `attention tabs`. A title formatter returns plain text: styling escapes in its return, such as `wezterm.format` output, are removed and their styling is dropped.
- In zsh, the prompt hook drops `WEZTERM_ATTENTION_LAUNCH_ID` once a claimed agent has exited, as bash already did.
- The plugin writes no pane record itself. `Alt+B` and the acknowledgement of a focused pane's `stop` or `notify` run the `attention` command, under the locks every writer of those records takes. An acknowledgement is written only while the activity the user was shown is still the pane's, so a newer one is never dismissed unseen, and each activity is acknowledged at most once: a failure is logged once and not retried on every poll.
- A Claude turn that ends on an API error (`StopFailure`) shows `notify` instead of staying on `thinking` until its 30-minute timeout. A Codex `Interrupt` clears the activity.
- A sub-agent waiting for permission raises `notify` on its pane.
- Forked Claude and Codex sessions (`SessionStart` with source `fork`) are bound.
- A malformed optional field in a provider event is dropped with one diagnostic, instead of the whole event being ignored.
- Unknown options and wrong option types are named once in the WezTerm log and the default is used. An unknown `renderer` means `tab`.
- Exec, serial and WSL domains count as local, so their panes are named by their own pane id, as in 0.6. On local panes the pane's own id wins over a user variable printed by terminal output.
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

- `attention mark` and the prompt-return activity clear no longer report an error after their write was applied, when the re-read of the current-binding pointer fails.
- `attention mark clear --source NAME` also withdraws an activity that `attention mark` wrote before any provider session bound the launch. Another source's activity stays.
- A sub-agent waiting for permission keeps the tab on `notify` while the lead keeps calling tools, for example Codex polling `wait_agent`. The notify ends when that sub-agent calls its next tool or stops, when the user prompts, or when the lead's turn ends, however long the sub-agent waits. A sub-agent record that cannot be read does not stop the notify; the hook reports `partial`.
- Sourcing the bash integration again from an rc file that assigns `PROMPT_COMMAND` puts its prompt hook back.
- Ctrl-C while a claim or prompt publication is running no longer stops the bash and zsh integrations for the rest of the shell, except under bash-preexec on bash 3.2 (see accepted limitations). In zsh, a Ctrl-C during the publication at an agent's exit still drops the agent's launch id.
- A long command line no longer holds up the command in the shell integration. Zsh checks a 512 KiB line in about 4 ms instead of 13 s. Bash reads a long assignment before the command word, such as `PAYLOAD='<big json>' curl …`, in about half a second for 32 KiB instead of about 22 s; the time still grows faster than the length, to about 2 s at 100 KiB.
- `scripts/install-cli.sh` installs the binary cargo reports building, also when `CARGO_BUILD_TARGET` or `build.target` is set, and refuses with a message when cargo reports none.
- A window's first tab order waits for the GUI's source answer, including across failed runs, for up to the whole retry backoff (about 47 s), so `attention tabs` never lists a window twice. If every retry fails, held and new windows are published without a source, and this is logged once.
- A local pane's published identity must belong to the GUI's own mux, so output from another mux cannot make a local pane show or acknowledge that mux's records.
- A window the plugin published without a source before a config reload is published under its source after it, and the plugin removes the unsourced file it wrote for that window, so `attention tabs` does not list the window twice after the command is built and the config reloaded. A file another GUI has rewritten since stays.
- The plugin ignores a `WEZTERM_ATTENTION_DIR` or `XDG_STATE_HOME` that is not UTF-8, and says so in its log, as the attention command refuses it where it decides the state root, instead of reading a directory the command never writes. The plugin's `dir` option is held to the same rule.
- With the `attention` command configured, the Pi extension refuses a `WEZTERM_ATTENTION_DIR` or `XDG_STATE_HOME` that is not UTF-8, where it decides the state root, and reports it instead of starting the command. Pi hands the command the value re-encoded as valid UTF-8, so the command would have written Pi's records to a directory no reader uses.

- Sourcing the bash integration twice no longer crashes the shell, and it works beside bash-preexec. It keeps `$?` and `$_` for later prompt commands and commands.
- The shell integration stays quiet outside a WezTerm pane, and `wezterm_attention_claim && claude` starts the agent there.
- `bin/attention` without the built binary exits 0 for hook commands (1 under `--strict`), and works through a symlinked `bin` directory.
- `scripts/install-cli.sh` builds into `./target` even when `CARGO_TARGET_DIR` or `build.target-dir` points elsewhere.
- Every text check refuses C1 control characters (U+0080–U+009F) as well as C0 and DEL, and the plugin strips control characters from tab text it draws or publishes.
- A long or control-character tab title no longer drops its whole window from `attention tabs`.
- Concurrent writers creating the same state directory no longer fail.
- A Codex `Stop` with `last_assistant_message: null` reports the reply as `absent`, not `invalid`.
- `examples/wezterm.lua` holds only the Attention setup: loading the plugin, the optional `follow-up.lua` view callback, `apply_to_config` with its options, and an `update-status` handler that polls. It loads without `follow-up.lua`.
- In bash, a command word whose quotes are kept by a backslash (`\"claude\"`) is not claimed as `claude`: bash runs a program with that literal name.
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
- A tab-order file naming any pane whose absence sweep could not decide, including one whose pane listing was malformed or whose process probe did not answer, makes `sweep` incomplete, as the pane's binding does. A file naming a pane whose realm or incarnation is not recorded is kept with the reason `not_recorded` and leaves `sweep` complete.
- `sweep --apply` against a mux that accepts connections and never answers waits out one listing deadline for that socket, not two per pane.
- An absence probe that sweep keeps because it is outside the state root is listed as an `absence` detail with action `keep`, as pane retention lists it, instead of leaving no detail.
- A non-UTF-8 `XDG_STATE_HOME` is refused as a non-UTF-8 `WEZTERM_ATTENTION_DIR` is, when it is the variable that decides the state root, rather than skipped in favour of `~/.local/state/wezterm-attention`. Both refusals say the value is not UTF-8. A relative one is ignored, as every relative `XDG_STATE_HOME` is.
- `bindings --socket` and `inspect` find other bindings of a provider session through a session index (`v2/sessions/`, record kinds `session_binding` and `session_index`) instead of reading every binding in the store: at 3000 stored bindings, 0.4 ms instead of about 390 ms. Each bind writes its entry before the binding, so a bind that fails between the two leaves no binding the index does not name. A store that already has bindings keeps the full walk until one `attention sweep --apply` without `--realm` has given every binding its entry.
- A binding that sweep ended after a reboot reads as ended in `bindings`, `inspect`, the plugin's `binding_phase` and later sweeps, which report `already_ended` rather than writing its end again, and its pane tree is then retained by the usual rule. A binding-end record names the binding event it ends in the new optional field `binding_event_id`, which orders it whatever the restarted monotonic clock says.
- The example status bar shortens a branch name by cell width, so a non-ASCII branch name no longer stops the right status updating.
- `examples/wezterm.lua` loads in `wezterm-mux-server`, which reads the same file but has no `wezterm.gui`, so a mux server's panes get the attention environment and are claimed.
- The gate's performance comparison measures the binaries cargo reports building; `scripts/build-attention.sh` holds the build step the installer and the gate share.
- The lock that `mark review`, `mark clear` and Pi's review events leave in `reviews/` no longer keeps an old pane's tree from retention.
- Sweep removes nothing through a symlinked directory below the state root: subagent compaction and a cleared absence probe are refused there, as binding and pane removals already were.
- No hook or command removes anything through a symlinked directory below the state root either. A claim keeps the pane's absence probe there, and a clear (`mark clear`, or a Pi review clear or bus clear) keeps what stands at the review's name; the claim or the clear still lands.
- `bindings --socket` reports a directory it could not read as `state_permissions` with its `path`, as realm-wide `bindings` does. A `binding_conflict` diagnostic names the provider session and its pane addresses, and a sweep diagnostic about a tab-order file's pane names the file and the pane.

### Upgrading from 0.6

1. Update the plugin with `wezterm.plugin.update_all()`, then build the command in the copy WezTerm loads; the README's Install section has the command. **Run `scripts/install-cli.sh` again after every `update_all`**: updating the plugin replaces the Lua files and does not rebuild the command.
2. Register the Claude Code and Codex hooks from the README, and remove any 0.6 hook scripts that wrote flat marker files for the same events.
3. Source the shell integration, guarded, as `docs/mux-setup.md` shows.
4. Remove the flat files 0.6 left behind. Nothing reads or collects them any more, so they only take up space, and the `.review` files among them, your 0.6 `Alt+B` flags, are not carried over. In the state directory 0.6 used (`~/.local/state/wezterm-attention`, or the `dir` you set), list and then remove the regular files at its top level named `<id>`, `<id>.ack`, `<id>.agents` and `<id>.review`, where `<id>` is all digits. The commands touch nothing below `v2/` or `tabs/`, and nothing else at the top level.

   ```sh
   root="$HOME/.local/state/wezterm-attention"   # or the dir you configured
   find "$root" -maxdepth 1 -type f | grep -E '/[0-9]+(\.(ack|agents|review))?$'
   find "$root" -maxdepth 1 -type f | grep -E '/[0-9]+(\.(ack|agents|review))?$' |
     while IFS= read -r file; do rm -f -- "$file"; done
   ```

5. If you set `acknowledge_types`, rename it to `auto_clear`. Remove `stale_after_ms`, and replace `format_tab_title = false` with `renderer = "manual"`. If your `title_formatter` parsed `ctx.default_title` as `dir / title`, read `ctx.directory` and `tab.active_pane.title` instead. If your `title_formatter` returned `wezterm.format` output for styling, return plain text instead; set colors through the plugin's `colors` option.
