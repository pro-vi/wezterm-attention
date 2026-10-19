# Troubleshooting

**Indicators not showing?**
- Run `attention doctor`: it checks the state directory's permissions and records, the mux socket, the pane's environment, agent processes and the command's version, whether a Codex shared server is running for hooks that call Attention, and whether each running Claude Code session in a pane is bound.
- If the window is attached to a mux server (`wezterm connect`, a unix domain), check the pane publishes its identity: `wezterm cli list --format json` shows the server-side pane id, and the pane must emit it as the `WEZTERM_PANE` user var. See [Publishing the pane id](tab-bar.md#publishing-the-pane-id). Without it the plugin deliberately does nothing for that pane.
- Check the plugin found the command: without it the WezTerm log says `libexec/attention-rs is missing`, and nothing is recorded.
- Ensure your hooks write to the same state directory as the plugin's `dir` setting (see [Configure](configuration.md#options) for the order).
- A `+N` with no glyph beside it is the [subagent count](tab-bar.md#subagent-activity) for a pane whose own activity is gone or already acknowledged. A `+?` is a pane whose subagent count could not be read.
- A ◆ that `Alt+B` does not clear is a review another source published; `attention mark clear --source NAME` withdraws it.
- `status_update_interval` defaults to 1000ms; indicators update on this interval. Lower it if indicators feel slow — the redraw request rides on the same tick.
- A Codex tab that shows nothing, or shows another pane's session: Codex 0.157 and later runs sessions in a shared background server that is in no pane, and Attention refuses their events with `session_detached` (or `claim_stale`, after a refused session start in a pane no shell claimed). Start Codex with `--no-daemon`; see [Codex hooks](setup.md#codex-hooks). `attention doctor` names such a server as `session_detached`, with the `codex_home` it serves. The refusal goes to the hook's stderr, and Codex 0.157.1 does not show the stderr of a hook that exits 0, which a refusal does, so the reason is neither on screen nor in Codex's logs. To read it, append `2>>/tmp/attention-hook.log` to the hook's command for a while; the command stays one `exec`.
- A resumed Codex session's tab shows nothing until you send its first message. Codex 0.157.1 runs `SessionStart` when the first turn starts, not when the session resumes, so this is not a refusal.
- A Claude tab that shows nothing for a whole session, with no finish sound: the session's `SessionStart` hook did not claim the pane, because Claude Code stopped it at its `timeout` or the start was refused, and only a session start claims, so every later event is refused (`claim_stale`). The refusal goes to the hook's stderr, which Claude Code 2.1.301 keeps in the session's transcript; its hooks documentation (read 2026-10-16) says the stderr of a hook that exits 0, as this one does, is not shown. `attention doctor` names such a session under its `claude_sessions` probe, with `context.provider_session_id` and `context.pane_id`. Run `/clear` or `/compact` in that pane, or quit and run `claude --resume <provider_session_id>`: any session start claims the pane once the agent that held it has exited. See [accepted limitations](accepted-limitations.md#a-session-whose-start-did-not-claim-its-pane-records-nothing-until-its-next-start).

**Indicators appear only when you switch tabs?**
- In `renderer = "manual"` mode, pass the event pane: `attention.poll(window, { active_pane = pane })`. The plugin resolves `window:active_pane()` at use time; the event pane is used only when the current pane is unavailable.
- Check the WezTerm error log. A failed redraw action is logged once for that window; polling and acknowledgement continue.

**Tab titles look wrong?**
- WezTerm only runs the **first** registered `format-tab-title` handler. If you have your own handler, set `renderer = "manual"` and use `wrap_title_formatter()` or the plugin API. Two handlers cannot coexist.
- Use `title_formatter` to customize the base title while keeping the plugin's indicators.

**Alt+M not working?**
- As for `Alt+B` below, with `mute_key` in place of `review_key`. A tab where no pane has published a launch is refused (`cannot toggle mute: no pane of this tab has published an agent launch`).
- The plugin adds its keys after yours, and WezTerm keeps the later of two bindings of one key (as of WezTerm 20260905-195314-b99b1ca2), so a binding of your own on `Alt+M` is replaced. Set `mute_key` to another key, or to `false`, to keep yours.

**Alt+B not working?**
- Check for keybind conflicts. Move the binding by setting `review_key = { key = "...", mods = "..." }`. The default, `Alt+B`, also moves back one word in bash 3.2 and in zsh 5.9's emacs mode, so choose another key if you use that.
- It works on a pane an agent is running in: the flag is a record of its own. If the tab still shows `✓` or `!` after a press, that activity outranks the flag; the ◆ appears once you have seen it.
- On a pane no launch has claimed, such as a plain shell where no agent has started, a press that would flag it is refused and logged once (`cannot toggle review: this pane has published no agent launch`), because no reader would show the flag. A press there on a tab that carries your flag still clears it. A mux-attached pane that has not published its identity refuses every press, since its tab cannot be found.
- A press runs the `attention` command. When it refuses, the reason is logged once for each kind of refusal on that pane (`attention plugin set-review failed for pane N: ...`) and the tab does not change: `claim_stale` means the pane's launch claim is not the launch the pane last published, which the next prompt republishes. `probe_unavailable` means a hook held the pane's records at that moment; press again. A command built before the plugin was updated answers nothing: the focused tab shows `⚠ rebuild attention` and the log says the command may predate the plugin. Run `scripts/install-cli.sh` again; the warning goes once a command answers.

**Typing lag or high GUI CPU with many tabs?**
- WezTerm rebuilds the whole tab bar whenever any pane sets its title, and each rebuild runs the plugin's `format-tab-title` once for every tab (upstream WezTerm `cab251610`). Agent titles rarely reach the screen, because a tab title or the pane's directory is drawn first (see [Custom tab titles](configuration.md#custom-tab-titles)), so turning them off removes rebuilds you never see the result of.
- Claude Code (checked on 2.1.291): set `"env": { "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1" }` in `~/.claude/settings.json`. The cost: a tab with no tab title and no known directory then shows the shell's title instead of the session topic.
- Codex (checked against its source at `985cf47a4`): in `$CODEX_HOME/config.toml`, give `[tui]` a `terminal_title` list without `"activity"`, for example `terminal_title = ["project-name"]`. The `"activity"` item is a spinner, so it rewrites the title while Codex works.
