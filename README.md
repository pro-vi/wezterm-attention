# wezterm-attention

Your WezTerm tab bar shows what the AI agent in each tab is doing: working, finished, or waiting for you. It works with Claude Code, Codex and Pi, and any script can set its tab the same way: a build, a test run, a long job.

## What it looks like

| Tab shows | Tint | Meaning |
|-----------|------|---------|
| ◌ ◔ ◑ ◕ (animated) | Violet | The agent is working |
| ✓ | Mint | The agent finished; check the result |
| ! | Rose | The agent needs you, for example at a permission prompt |
| ◆ | Gold | You flagged the tab for review with `Alt+B` |
| A bell with a slash | None | You muted the tab with `Alt+M` |
| ✓+2 | Mint | The agent finished, and two of its sub-agents are still running |

A tab lights up while you are on another tab. Focusing the pane clears a ✓ and, in most cases, a !. When a tab's panes differ, by default ! wins over ✓, ✓ over ◆, and ◆ over the spinner. [The tab bar](docs/tab-bar.md) has every rule.

## Install

There are two parts: the plugin draws the tabs, and the `attention` command records what each agent is doing. Your agents' hooks run the command. You need WezTerm `20230320-124340-559cb7b0` or newer, macOS or Linux (glibc), and Rust 1.94 or newer to build the command.

Upgrading from 0.6? Follow [the upgrade steps in the changelog](CHANGELOG.md#upgrading-from-06).

### 1. Load the plugin

Add it to your WezTerm config, after you assign `config.keys` and before any other `format-tab-title` handler:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")
attention.apply_to_config(config)
```

Then start WezTerm once, so that it downloads the plugin.

### 2. Build the command

Build it in the copy WezTerm downloaded, and link it onto your PATH:

```sh
case "$(uname)" in
  Darwin) plugins="$HOME/Library/Application Support/wezterm/plugins" ;;
  *) plugins="${XDG_DATA_HOME:-$HOME/.local/share}/wezterm/plugins" ;;
esac
checkout="$plugins/httpssCssZssZsgithubsDscomsZspro-visZswezterm-attention"
sh "$checkout/scripts/install-cli.sh"
mkdir -p ~/.local/bin
ln -s "$checkout/bin/attention" ~/.local/bin/attention
```

Reload the WezTerm config afterwards. **Run `install-cli.sh` again after every `wezterm.plugin.update_all()`**: an update replaces the plugin's Lua files and leaves the old command in place.

### 3. Register your agents' hooks

- **Claude Code:** merge [these hooks](docs/setup.md#claude-code-hooks) into `~/.claude/settings.json`.
- **Codex:** add [these hooks](docs/setup.md#codex-hooks) to `~/.codex/hooks.json`, approve them with `/hooks` in Codex, and start Codex as `codex --no-daemon`.
- **Pi:** run `pi install git:github.com/pro-vi/wezterm-attention`.

On macOS that is all: an agent claims its own pane when its session starts. On Linux the shell claims the pane; set that up as [Mux setup](docs/mux-setup.md) describes.

Start an agent in a new pane, and its tab lights up. If it does not, run `attention doctor`, then read [Troubleshooting](docs/troubleshooting.md). [Setup](docs/setup.md) has each step in full, including loading the plugin from your own clone and staying on a release.

## Keys

| Key | What it does |
|-----|--------------|
| `Alt+B` | Flags the tab for your own review (◆). Press it again to clear the flag. |
| `Alt+M` | Mutes the tab: it shows the bell and hides its agent's activity until the agent needs you. Press it again to unmute. |

Both work on a pane where an agent, or a shell that claims panes, has started a command. Move either key, or turn it off, with `review_key` and `mute_key` in [Configuration](docs/configuration.md#options). The plugin adds its keys after yours, so on WezTerm `20260905-195314-b99b1ca2` they replace your own bindings of the same keys.

## Build on it

Attention keeps a record of the agent session in each pane: its state, which agent it is, its session file, and its prompts, turn ends and questions. Your own code can read it, and can set a tab itself.

- **Inside WezTerm:** `attention.get_attention_view(pane)` returns a pane's view, and the `on_view_change` option calls your function when a view changes.
- **From any program:** `attention bindings`, `attention tabs`, `attention inspect` and `attention observe` answer in JSON.
- **On each hook:** a program registered with `--consumer` receives the event, and with `--include-reply` the agent's final message.
- **To set a tab:** `attention mark thinking|stop|notify|review|clear --source NAME`, run from a shell that has claimed the pane.

Built on it so far, outside this repository: a prompt-cache countdown, a pane picker that names each pane's agent, a highlight for questions nobody has answered, and a relay that passes one agent's reply to another pane. [Building on Attention](docs/building-on-attention.md) says what each one reads. The supported interface is the `attention` command's JSON, specified in the [consumer guide](docs/consumer-guide.md), and the plugin's [Lua API](docs/configuration.md#public-api); the Rust crate is not.

## Documentation

| Page | What it covers |
|------|----------------|
| [Setup](docs/setup.md) | Each install step in full, and the hooks for Claude Code, Codex and Pi |
| [The tab bar](docs/tab-bar.md) | Every state, the sub-agent count, the review flag, muting, how polling works |
| [Configuration](docs/configuration.md) | Every option, render modes, custom tab titles, the Lua API |
| [Building on Attention](docs/building-on-attention.md) | Setting a tab from your own tools, the drawn tab order, what has been built |
| [Consumer guide](docs/consumer-guide.md) | The JSON contract for programs that read Attention |
| [Record contract](docs/record-contract.md) | The record files and who writes each |
| [Mux setup](docs/mux-setup.md) | Mux servers, shell claims, Linux |
| [Troubleshooting](docs/troubleshooting.md) | A tab that shows nothing, or the wrong thing |
| [Accepted limitations](docs/accepted-limitations.md) | Known compromises, and why each ships |
| [Changelog](CHANGELOG.md) | What each release changed |

## License

MIT
