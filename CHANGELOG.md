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
<!-- pending: query lane -->
- **Query commands exit 0, 1 or 2 only.** `bindings`, `tabs`, `inspect`, `doctor` and `sweep` exit 0 when the answer is complete, 1 when it is incomplete or the command failed, and 2 for a command-line error. Diagnostics alone no longer change the exit code, and exit 3 is gone from these commands. Every error envelope has `complete=false`.
<!-- pending: exec lane -->
- **Hook commands never exit 2**, which Claude Code and Codex read as "block". They exit 0 with the reason on stderr, or 1 under `--strict`.

### Added

- The `attention` command (`scripts/install-cli.sh` builds it into `libexec/`, `bin/attention` launches it). It records provider callbacks with `attention hooks event PROVIDER EVENT`, custom activity with `attention mark`, and answers queries with `bindings`, `inspect`, `tabs`, `doctor` and `sweep`. `attention --version` names the commit it was built from.
- Claude Code and Codex hook support, described by `attention hooks describe --provider claude|codex --json`, with copyable `settings.json` and `hooks.json` blocks in the README.
- Launch claims from the shell: bash claims `claude`, `codex` and `pi` automatically; zsh uses `wezterm_attention_claim && <agent>`.
- A pane identity that survives detach, reattach and several mux sockets, republished after a GUI reattaches.
- A subagent count: `✓+2` on a tab while two subagents in its pane are working.
- `get_attention_view(pane)`, with bounded lifecycle observations and request evidence, and the `on_view_change` callback. See `docs/consumer-guide.md`.
- Opt-in delivery of the exact prompt or reply text to consumer executables: `--consumer … --include-prompt` or `--include-reply`.
- The drawn tab order, published to `tabs/` and read with `attention tabs`.
- `attention sweep`, which previews by default and, with `--apply --operation-id`, ends bindings whose panes are verified gone and collects leftover files.
- Options `show_directory`, `settled_title_fallback`, `show_provider`, `on_view_change` and `integration_root`; `attention.doctor(window)` in the Lua API.

### Changed

- A prompt tints the pane `thinking` straight away, instead of waiting for the first tool call.
- `thinking` from v2 records animates like the v1 spinner.
- A Claude turn that ends on an API error (`StopFailure`) shows `notify` instead of staying on `thinking` until its 30-minute timeout. A Codex `Interrupt` clears the activity.
- A sub-agent waiting for permission raises `notify` on its pane.
- Forked Claude and Codex sessions (`SessionStart` with source `fork`) are bound.
- A malformed optional field in a provider event is dropped with one diagnostic, instead of the whole event being ignored.
- Unknown options and wrong option types are named once in the WezTerm log and the default is used. An unknown `renderer` means `tab`.
- Exec, serial and WSL domains count as local, so their panes keep showing v1 flat markers by pane id, as in 0.6. On local panes the pane's own id wins over a user variable printed by terminal output.
- After a reattach, the plugin also republishes through WezTerm's implicit `unix` domain and any unix domain without a `socket_path`.
- The plugin creates the state directory and `tabs/` private (mode 0700).
<!-- pending: exec lane -->
- `attention` never runs `wezterm-gui` or `wezterm-mux-server` as the CLI, finds `wezterm` beside `$WEZTERM_EXECUTABLE` or in `/Applications/WezTerm.app`, and passes `--no-auto-start` to every `wezterm cli` call.

### Fixed

- Sourcing the bash integration twice no longer crashes the shell, and it works beside bash-preexec. It keeps `$?` and `$_` for later prompt commands and commands.
- The shell integration stays quiet outside a WezTerm pane, and `wezterm_attention_claim && claude` starts the agent there.
- `bin/attention` without the built binary exits 0 for hook commands (1 under `--strict`), and works through a symlinked `bin` directory.
- `scripts/install-cli.sh` builds into `./target` even when `CARGO_TARGET_DIR` or `build.target-dir` points elsewhere.
- Every text check refuses C1 control characters (U+0080–U+009F) as well as C0 and DEL, and the plugin strips control characters from tab text it draws or publishes.
- A long or control-character tab title no longer drops its whole window from `attention tabs`.
- Concurrent writers creating the same state directory no longer fail.
- A Codex `Stop` with `last_assistant_message: null` reports the reply as `absent`, not `invalid`.
- `examples/wezterm.lua` keeps `Alt+B`, loads without `follow-up.lua`, and runs git without repository hooks.

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

5. If you set `acknowledge_types`, rename it to `auto_clear`. If your `title_formatter` parsed `ctx.default_title` as `dir / title`, read `ctx.directory` and `tab.active_pane.title` instead.
