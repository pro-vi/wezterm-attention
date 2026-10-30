# Building on Attention

Attention has two parts. A small Rust command, `attention`, runs as a hook from Claude Code, Codex or Pi and records what a pane's agent is doing against a pane identity that survives detach, reattach and multiple mux sockets. A Lua reader in WezTerm polls those records and renders the tab. Programs other than WezTerm can read the same records: `attention bindings --json`, `attention tabs` and `attention inspect` return validated facts, so a script does not have to scrape a terminal to find out which pane an agent is in.

The records the `attention` command writes are called **v2 records** in these docs, after the manifest that defines them, `protocol/v2.json`; the name is for the record format, not a version of this project. They are the only format the plugin reads, and only the `attention` command writes them: see [Record contract](record-contract.md).

The supported interface is the `attention` command — its JSON envelopes, diagnostic codes and exit codes, specified in the [consumer guide](consumer-guide.md) — and the plugin's [Lua API](configuration.md#public-api). The Rust crate the command is built from is not supported for use outside this repository; its items may change in any release.

Known compromises are listed in [accepted limitations](accepted-limitations.md).

## Recording attention from your own tools

Write records through the `attention` command; never construct their JSON yourself. Use `attention hooks event PROVIDER EVENT` for provider callbacks, and `attention mark STATE --source NAME` for anything else, where `STATE` is `thinking`, `stop`, `notify`, `review` or `clear`. Both write into the pane's current launch claim. On macOS an agent's registered hooks claim the pane for their agent themselves (see [Claude Code hooks](setup.md#claude-code-hooks)). `attention mark`, and every producer on Linux, needs the launch id of a claiming shell, so run it from one: in bash, set `WEZTERM_ATTENTION_COMMANDS` to the space-separated command names to claim, which replaces the built-in `claude codex pi`, so list those too if you still want them claimed; in zsh, start it as `wezterm_attention_claim && <command>`. See [Mux setup](mux-setup.md).

`--source` defaults to `manual`. `--frame`, `--label` and `--ttl-ms` are stored on the activity that `thinking`, `stop` and `notify` write: a fixed spinner frame for `thinking`, a label that `attention inspect`, `get_attention_view` and a formatter's `ctx.attention.label` return, which the default tab does not draw, and a time after which the activity expires; `attention mark --help` says what each does. Sending the same mark again with `--ttl-ms` before that limit runs out renews it: the answer is `renewed`, the event id stays, and the limit starts again from the new write. A repeated mark sent after its limit ran out is written as a new event, and a repeated mark without a limit is `skipped`. To keep a mark alive while work runs, send it every few seconds with a limit a few times longer. `review` and `clear` write no activity and refuse them. `attention mark clear --source NAME` removes that source's review flag and, when the activity the tab currently shows was published by that source, clears that activity too; it reports `applied` when it did either and `skipped` otherwise. The source name `user` belongs to the plugin's review key, and every `mark` state refuses it.

## The drawn tab order

A WezTerm window attached to a mux server mirrors the server's tabs under numbers of its own, and those are the numbers the tab bar prints. They are not the order of `wezterm cli list`. Nothing outside the GUI process can see the drawn order, so the tab bar publishes it — one file per window, under the state directory, named by the incarnation of the GUI's own mux socket and the window id:

```text
<state directory>/tabs/<incarnation id>-<window id>.json
```

```json
{ "schema": 2,
  "window_id": 0,
  "published_at_ms": 1789884000123,
  "source": { "socket_path": "/…/gui-sock-4946", "realm_id": "…", "incarnation_id": "…" },
  "tabs": [ { "number": 11, "text": " 11: ✓ braid ", "marker_ids": ["v2:…:…:16"] } ] }
```

`source` names the GUI that drew the window, because a window id means something only inside one GUI process. The plugin learns that identity by asking the `attention` command shortly after startup. A window's first order is held until that answer arrives and then written under its source; when the command is not built, or no answer can come, it is written as a schema-1 file at `tabs/<window id>.json` with no `source`. A window keeps the one file it was first written to for as long as it is open, with one exception: after a config reload that finds the command built, a window first written without a `source` is written under its source, and the plugin removes the schema-1 file it wrote for that window unless another GUI has rewritten it since.

`number` is the number the bar printed, `text` is the whole string it drew (escape sequences and control characters removed, cut to 256 bytes, and a spinner always shown at its first frame so the file does not change every second), and `marker_ids` are the IDs the plugin already uses for those panes — already translated out of the window's local numbering, because only the window could translate them. A pane that a launch has claimed is `v2:<realm_id>:<incarnation_id>:<pane_id>`; a pane no launch has claimed is its canonical decimal pane id. A pane appears once a poll has identified it. The file is written when a window's composed list changes and at no other time, so `published_at_ms` says when the bar last drew something different.

Read it with `attention tabs`, which returns every window in the same JSON envelope as `bindings`. **It is honest about when it was written, not guaranteed current**: nothing refreshes it while the bar is idle, and no consumer should act on a number it has not checked. Use it to describe tabs and to resolve "the second `api` tab"; to act on one, ask the GUI, where `mux_window:tabs_with_info()` returns the drawn order live.

The publisher is the handler the plugin registers, so `renderer = "manual"` — where your own formatter draws the tabs and the plugin registers nothing — publishes nothing. `wrap_title_formatter` does not publish either: with both handlers registered, the same window would draw two different texts and each repaint would rewrite the file twice.

Every setup publishes, including a plain local WezTerm where the drawn number equals the derived one. A consumer cannot tell a simple setup from a publisher that is not running — the file is absent in both — and deriving is right in one case and wrong in the other.

## Other use cases

- **Build systems** — write `notify` on failure, `stop` on success
- **Test runners** — animated `thinking` while running, `stop` or `notify` on completion
- **Long-running scripts** — any background job that wants your attention when done
- **Manual triage** — `Alt+B` to flag tabs for review during code review sessions

## Built on Attention

These are built outside this repository from its public facts; each names what it reads.

- **A prompt-cache countdown in the status bar.** It dates the pane's last request from the newest `written_at_unix_ns` among the `lifecycle.observations` of `get_attention_view(pane)` whose `actor.kind` is `lead`, and keys the countdown by `binding_id`, so a new session starts a new one. The timestamp is a decimal string of nanoseconds, too large for a Lua number.
- **A pane jump picker that names the agent in every pane.** It reads `provider` from `get_attention_view(pane)`, and trusts it only when `binding_phase` is `active` and `reader_confidence` is `confirmed`.
- **An unanswered-question highlight.** It is driven by `on_view_change` and looks for entries in `lifecycle.requests` whose `kind` is `question`, leaving out one with an `answer_observation_ids` entry or published before `lifecycle.latest_prompt`. [`examples/follow-up.lua`](../examples/follow-up.lua) is the starting point.
- **An exact reply relay between panes.** An executable registered on the `Stop` hook with `--consumer … --include-reply` receives the agent's final message as `reply.text`, exactly as the provider sent it, together with the scope of the pane it came from.
