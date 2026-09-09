# What these eight changes mean for you

Keep separate binding/child files. Keep protection against stale child records. Treat history as optional unless retaining existing records is enough. The Rust draft needs revision before a build.

Q1–Q8 match your questions. They are independent choices, not upgrade steps. The command, file and title examples below are illustrations, not live captures.

## Q1 · Separate files and recovery

> **You asked:** “i think we should keep the original design, it also helps recover right?”

**Yes. Separate files help isolate read failures. Keeping old binding directories preserves earlier session records. Both layouts can recover after a reconnect.**

Example: one child record cannot be read.

```text
Original binding folder
  binding.json       readable
  activity.json      readable
  agents/child-A     UNREADABLE
  agents/child-B     readable

Fresh activity and child B are still available.
Child A is unavailable; a valid cached copy
can be reused under the existing scope rules.
```

```text
Proposed combined file
  state.json         UNREADABLE

No fresh provider state from this file.
A valid earlier cache can still be available.
```

| Question | Answer |
|---|---|
| Why the original design? | Update separate facts without rewriting everything. Keep an old session's writes away from a new session's files. |
| Trade-off | More files and checks when combining them. Several related updates are not one atomic file replacement. |
| What would consolidation lose? | Some partial-failure isolation. This Rust draft also removes old binding directories, but that is a separate choice from combining files. |
| Direction | Keep the original separation. Reduce repeated implementation code without changing that storage decision. |

Separate files are not backups. Disk loss can affect either layout. Reconnect recovery also needs identity republication; file separation alone does not restore missing GUI identity.

[Source: record-level cache and view composition in init.lua:678,1407](../../plugin/init.lua)

## Q2 · Was the process scan periodic?

> **You asked:** “how was it done? peridic scan?”

**The repository does not schedule a periodic process scan. The scan is reached by explicit inspection or maintenance commands.**

`bindings` and `doctor` can inspect process evidence. Only an applied `sweep` can infer an ended binding from absence. Normal rendering does neither.

```text
First explicit applied sweep
  pane absent + scoped process absent
  -> save first absence observation

Different applied sweep, at least 60 seconds later
  still absent + process still absent
  -> mark that binding ended

Next applied sweep finds pane present
  -> clear the first observation
Probe unavailable -> do not infer ending
```

Preview mode writes nothing. Repeating the same operation ID cannot count as the second observation. An external scheduler could run these commands periodically, but this repository does not install one.

| Question | Answer |
|---|---|
| Why the original design? | An agent can disappear without sending its ending hook. Repeated checks help distinguish absence from a failed probe or a GUI detach. |
| Trade-off of removing it | Less maintenance code, but more records with an unknown final state. |
| What would we lose? | The command's ability to mark abandoned bindings ended when no ending hook arrived. |
| Direction | Reassess it as explicit maintenance. It is not an ongoing polling cost to remove. |

The automatic reattach publisher is different. It lists server panes and sends identity through their terminal output. It does not run this ending decision.

[Source: attention.py:1726,1983,2300,2392](../../libexec/attention.py)

## Q3 · Is history cheap enough to keep?

> **You said:** “if history is cheap we can build but if expensive lets wait”

**Keeping binding records that were already written is less work than adding a new event-history system. Their total storage and query cost still need measurement.**

```text
One pane, three successive sessions
  session A   older binding directory retained
  session B   older binding directory retained
  session C   current binding
```

| Kind of history | What it gives you | Additional work |
|---|---|---|
| Retained binding snapshots | Which sessions occupied the pane; latest saved facts for each | Existing layout already retains them. Listing and safe cleanup still cost code and I/O. |
| Recent lifecycle events | Which accepted changes happened while a consumer was away | Sequence numbers, cursors, bounded retention, gaps and replay rules. |

The proposed 64-event history was new work in the Rust draft. It was not part of the implemented binding catalogue.

| Question | Answer |
|---|---|
| Why retain old bindings? | Find a previous session's ID and saved details for a restore tool, or see which session previously occupied the pane. |
| Trade-off | More retained files, plus eventual cleanup. Keeping files alone does not create an event timeline. |
| What would we lose by waiting on event history? | Catching up on missed transitions. Current state and retained session snapshots would still be readable. |
| Recommendation | Retain existing binding snapshots. Defer the new event-history system unless a consumer needs it. Measure stored size and listing time before adding richer history features. |

[Source: binding replacement and retention in attention.py:1139,2364](../../libexec/attention.py)

## Q4 · What does attention “compaction” mean?

> **You asked:** “by compaction you dont mean the session compaction right? why does attention need to compact? it sounds like a failure mode we should still handle tho”

**It means cleanup of attention's old child records. It does not summarize or shorten the agent's conversation.**

The failure to prevent is a finished child appearing active again because an older callback arrives late.

```text
Example event order (observations)
  100  Child A works
  200  Child A stops
  100  Old work arrives late

With stopped evidence: reject 100; child stays stopped.
Delete that evidence:  100 can recreate a false +1.
```

Original safe cleanup first keeps an ordering cutoff, then deletes only inactive records that the cutoff covers. A delayed callback below that cutoff still loses. Eligible active children must not be removed or hidden by advancing the cutoff.

| Question | Answer |
|---|---|
| Why the original design? | Bound old record growth without forgetting which delayed callbacks must be rejected. |
| Trade-off | The cleanup code must handle races, equal timestamps and crashes between keeping the cutoff and deleting records. |
| What would the proposed cut lose? | Safe cleanup during a long-lived session. The draft kept those records. If a later update would exceed the file-size limit, it refused that update and reported that some state was missing. |
| Direction | Keep the failure protection and a way to handle growth. Reconsider the implementation, not whether the failure matters. |

The original cleanup runs through explicit sweep. It uses age and count policies, but its count cap cannot force deletion of eligible active children.

[Source: child compaction in attention.py:2086,2171](../../libexec/attention.py)

## Q5 · Who does “manual polling”?

> **You asked:** “irregular manual polling - what counts as manual here?”

**Custom configuration code does it. You do not type a polling command.**

```text
Default setup
  WezTerm update-status callback
  -> plugin calls attention.poll()

Custom setup with auto_poll=false
  your existing update-status callback
  -> your configuration calls attention.poll()
```

Both can run automatically on a regular interval. The custom setup avoids installing a second handler when your configuration already owns the callback.

| Question | Answer |
|---|---|
| Why the extra expiry timer? | It asks for a new poll at the next expiry boundary, independently of the normal callback schedule. |
| Trade-off of removing it | Less timer logic; visible expiry waits for the next regular poll. |
| What would we lose? | An independent wakeup if custom configuration stops or delays polling. Regular custom polling is not inherently irregular. |
| Recommendation | Discuss timer removal separately from nanosecond precision. They are two different choices. |

[Source: expiry wakeup and default callback in init.lua:2256,2772](../../plugin/init.lua)

## Q6 · What is a settled pane title?

> **You asked:** “whats a settled-pane-title?”

**A pane title that was the same in two consecutive polls.** It is a last fallback when there is no server tab name or usable directory name.

Illustration: neither a server tab name nor a directory is available.

| Poll | Pane reports | Settled fallback |
|---|---|---|
| 1 | Working | None yet |
| 2 | Working | Working |
| 3 | Done | None yet |
| 4 | Done | Done |

Two equal samples do not mean two seconds. A title changing on every sample never becomes settled. A server tab name or usable directory takes precedence throughout.

| Question | Answer |
|---|---|
| Why the original design? | Use a helpful pane title without following every spinner change. |
| Trade-off | Keep a little sampling state and define when the title is usable. |
| What would we lose? | That last fallback. Server names, directory names and custom formatting would still work. |
| Review point | This is a small presentation choice. It is not required for agent identity, and removing it is unlikely to be the main code reduction. |

[Source: title sampling and precedence in init.lua:1793,1837](../../plugin/init.lua)

## Q7 · What changes when I upgrade?

> **You asked:** “explain this from a user who wants to upgrade”

**Writing both formats keeps old flat-file readers working, including bridge. The plugin can still need an upgrade for the new wire version.**

```text
With temporary dual writes
  upgrade plugin for the new wire version
  new writer writes: new records + old records
  old bridge still reads old records
  upgrade bridge later
```

```text
Without dual writes
  install new binary, keep it inactive
  prepare new plugin and bridge together
  activate the matching readers and writer

Switch only the writer -> old bridge misses new updates.
```

Here, the writer is the hook command. Readers include the plugin and bootstrap's bridge dashboard. The bridge directly reads old flat files; your GUI configuration already reads the plugin API.

| Question | Answer |
|---|---|
| Why the original design? | Permit gradual upgrades and keep old file readers working. |
| Trade-off of removing it | One stored output format, but a coordinated upgrade and rollback. |
| What would you lose? | The ability to activate the new writer while leaving an old bridge unchanged. |
| Recommendation | Keep temporary compatibility if the upgrade cannot move those readers together. Do not make you repair a broken dashboard after upgrading. |

For panes still using v1, the new plugin can keep reading old hook files. That does not require every new writer to produce them. Dual writes retain the old format's pane-ID limitations. They also do not make an old plugin understand a newer wire version; the existing reader refuses unknown future versions.

[Source: bridge's flat-file reader](../../../../_setup/bootstrap/scripts/bridge) · [plugin's existing public query](../../plugin/init.lua)

## Q8 · Why would launching an agent change?

> **You said:** “i dont understand.. wtf”

**The proposed Bash cut added a command you would have to remember. Rust does not require that change.**

With the implemented v2 Bash integration sourced, launch claiming is automatic. This is an illustration, not an instruction to change your setup:

```sh
# Implemented Bash behavior
codex

# Proposed manual Bash behavior
wezterm_attention_claim && codex
```

The claim says that a new agent is starting in this pane. Hooks inherit that launch identity. It prevents an older agent's delayed callback from changing the new agent's state.

| Question | Answer |
|---|---|
| Why the original design? | Give Bash users launch identity automatically when they type a supported agent command. |
| Trade-off of removing it | Less command parsing and DEBUG-trap handling, but a required extra launch step. |
| What would you lose? | Automatic Bash launch claiming. Without a fresh claim, hooks can inherit the previous launch ID or have none. The new agent then lacks a reliable new launch identity. |
| Recommendation | Preserve the ordinary launch experience. Do not approve manual Bash claiming as a code cleanup. |

The current v2 zsh implementation already uses explicit claiming because its automatic detection could not distinguish all executed compound commands. That does not mean your live setup uses it; existing v1 hooks do not require that v2 claim. The zsh limitation does not justify making Bash manual too. How the Rust version preserves convenient launch commands remains a design question; no replacement launcher was implemented here.

[Source: Bash integration](../../shell/wezterm-attention.bash) · [zsh integration](../../shell/wezterm-attention.zsh)

## Review state

Separate binding/child storage is retained by your direction. Growth and delayed-child protection remain requirements. The history preference is to keep what is cheap and defer expensive additions. The other changes are still under review; the earlier Rust draft is not ready to build.

This explainer uses the current checkout, the earlier v2 plan and your latest questions. It reports source inspection, not new runtime tests or measured savings. The HTML is generated from this Markdown source.
