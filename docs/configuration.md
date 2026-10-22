# Configuration

What `apply_to_config` accepts, how the plugin builds a tab's title, and the functions it exports to your own Lua code.

## Options

All options are optional — defaults work out of the box. An unknown option, a value of the wrong type, and a `priority` or `auto_clear` that is not a list of attention types are each named once in the WezTerm log, and the default is used instead. An unknown name inside `colors` or `indicators` is named and ignored. A `title_formatter` given with `renderer = "manual"`, which draws no titles, is named and not used, and a second `apply_to_config` call is named and changes nothing:

```lua
attention.apply_to_config(config, {
  -- Render mode: "tab" | "manual". Any other value logs and means "tab".
  renderer = "tab",

  -- The state directory. The default is the first that applies:
  -- $WEZTERM_ATTENTION_DIR when set, non-empty and absolute; else
  -- $XDG_STATE_HOME/wezterm-attention when XDG_STATE_HOME is set, non-empty and
  -- absolute; else ~/.local/state/wezterm-attention. The attention command and
  -- the Pi extension resolve it the same way.
  dir = nil,

  -- Where the attention command was built, when it is not the checkout
  -- WezTerm loaded the plugin from. See Install.
  integration_root = nil,

  -- Custom base title (tab mode only; plugin adds indicators + colors around it)
  title_formatter = nil,  -- function(tab, ctx) -> string of plain text

  -- Base-title sources; see Custom tab titles.
  show_directory = true,
  settled_title_fallback = true,

  -- Append " · Claude", " · Codex" or " · Pi" when the tab's indicator comes
  -- from a pane with a provider binding.
  show_provider = false,

  -- Called after each poll with what changed in a window's pane views.
  -- See docs/consumer-guide.md, "GUI view callback".
  on_view_change = nil,  -- function(change)

  -- Tab background tints per attention type
  colors = {
    thinking = "#1c1730",  -- violet tint
    stop     = "#12271c",  -- mint tint
    quiet    = "#122033",  -- blue tint
    notify   = "#240f16",  -- rose tint
    review   = "#1a1a0c",  -- gold tint
  },

  -- Tab text indicators
  indicators = {
    thinking_frames = { "◌ ", "◔ ", "◑ ", "◕ " },
    stop   = "✓ ",
    held   = "⏾ ",
    quiet  = "↪ ",
    notify = "! ",
    review = "◆ ",
    muted  = "\u{f1f6} ",  -- a bell with a slash, from the Nerd Font symbols WezTerm bundles
  },

  -- Priority order (last = highest)
  priority = { "thinking", "review", "stop", "notify" },

  -- Visually acknowledge these types when focusing their pane
  auto_clear = { "stop", "notify" },

  -- Review toggle keybind (false to disable). Added to config.keys, so assign
  -- config.keys before calling apply_to_config.
  review_key = { key = "b", mods = "ALT" },

  -- Mute toggle keybind (false to disable), added the same way.
  mute_key = { key = "m", mods = "ALT" },

  -- Register the plugin's own update-status poller. See
  -- "Existing update-status handler?" below.
  auto_poll = true,

  -- Ask WezTerm to rebuild the tab bar when a pane's attention changes.
  -- Set false only if your own code already repaints titles every tick
  -- (renderer = "manual" with a title drawn from your update-status handler).
  -- The plugin then never performs ActivateTabRelative(0), saving that
  -- tab-activation pass; nothing else about polling or acknowledgement changes.
  request_redraw = true,

})
```

## Render modes

There are two render modes:

| Mode | Who owns `format-tab-title` | Per-tab colors | Use when |
|------|---------------------------|----------------|----------|
| `tab` (default) | Plugin | Yes | You want it to just work |
| `manual` | You | Yes | You have a custom tab formatter |

```lua
-- Default: plugin owns everything
attention.apply_to_config(config)

-- Manual: you own format-tab-title, plugin provides helpers
attention.apply_to_config(config, { renderer = "manual" })
wezterm.on("format-tab-title", attention.wrap_title_formatter(function(tab, ctx)
  return ctx.default_title  -- your custom logic here
end))
```

## Custom tab titles

The plugin draws each tab as ` <number>: <indicator><base title> `, with ` · <provider>` before the closing space when `show_provider = true`. The number is left out when `config.show_tab_index_in_tab_bar = false`.

The default base title is the first of these that is not empty:

1. The tab's own title, as set by `tab:set_title()` or `wezterm cli set-tab-title`.
2. The last component of the pane's current directory. `show_directory = false` skips it.
3. The pane's title, once it has stayed the same for two polls. `settled_title_fallback = false` skips it, and the poll then samples no titles at all.
4. The pane's title as it is now.

Text from the first two sources, and the title from the last, has escape sequences and control characters removed and is cut to 256 bytes on a character boundary; a title with a control character in it is never used as the settled title.

In `tab` mode, pass a `title_formatter` to replace the base title without losing indicators:

```lua
attention.apply_to_config(config, {
  title_formatter = function(tab, ctx)
    -- ctx.default_title: the base title from the rule above
    -- ctx.tab_title, ctx.directory, ctx.settled_title: its sources, nil when empty
    -- ctx.attention: { indicator, type, color, subagents, subagents_uncertain,
    --   source, label, provider, review, muted, binding_health }
    local pane = tab.active_pane
    return pane.title  -- just the pane title, no directory
  end,
})
```

`ctx.attention[1]`, `[2]` and `[3]` are the indicator, type and color, for formatters written against the positional form.

The tab's `type`, `color`, `source`, `label`, `provider` and `binding_health` are those of the pane whose type ranks highest in `priority`, where a muted pane ranks only by its `notify` or your review flag. `subagents` is the sum of every pane's count, less those of muted panes that show no `notify`, and `subagents_uncertain`, `review` and `muted` are true when they are true for any pane of the tab.

## Existing update-status handler?

By default, the plugin registers its own `update-status` handler to poll pane records. If you already have one (e.g., for a git status bar), use manual polling instead:

```lua
attention.apply_to_config(config, { auto_poll = false })

-- Then in your existing update-status handler:
wezterm.on('update-status', function(window, pane)
  attention.poll(window, { active_pane = pane })  -- redraw transport when no current pane is available
  -- ... your git status bar, battery, etc.
end)
```

## Public API

The plugin exposes functions for use in your own WezTerm Lua code:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")

-- The id a pane's records are filed under: in one of the GUI's own domains
-- (local, exec, serial, WSL) its pane id; elsewhere the server pane id it
-- published (WEZTERM_ATTENTION, else WEZTERM_PANE); nil when it published neither.
local marker_id = attention.pane_marker_id(pane)

-- Read cached attention state: returns (type, frame) or nil.
-- state is the effective type: "review" when the pane's review flag outranks
-- its activity, else the activity's own type; nil when neither is in effect, as
-- when the activity was acknowledged or only the pane's subagents are running.
-- frame is the activity's frame; the `frame` row of docs/consumer-guide.md says
-- where it comes from. The call returns a single nil when the id is seen at more
-- than one full pane address; get_attention_view tells them apart.
local state, frame = attention.get_attention(marker_id)

-- Read the cached base fields plus independent lifecycle evidence, without
-- I/O: type and frame as above, the activity's source, the subagent count and
-- whether it could be read, the review flag, the provider binding and more; see
-- docs/consumer-guide.md. Nested returned values do not share mutable state with
-- the plugin cache.
local view = attention.get_attention_view(pane)

-- What the tab renderer resolves for a tab with these GUI pane ids (the
-- numbers or strings WezTerm's TabInformation carries), for a config that
-- draws one thing for several tabs, such as a folded group. Besides
-- indicator, type, color and the rest, it carries glyph, the state's own glyph
-- with no spinner frame, mute mark or count ("" for none), and priority, the
-- winning type's priority (-1 for none), so tabs can be counted by state. A
-- pane no poll has walked yet adds nothing. Read-only.
local visible = attention.visible_attention({ 3, 4 })

-- Poll manually (for auto_poll = false)
attention.poll(window, { active_pane = pane })

-- Wrap a title function with attention decoration (for renderer = "manual")
wezterm.on("format-tab-title", attention.wrap_title_formatter(function(tab, ctx)
  -- ctx is the same table title_formatter receives; see Custom tab titles
  return ctx.default_title
end))
```

Which removals need a new major version, and what is private: [Compatibility](consumer-guide.md#compatibility).

## Type annotations

LuaCATS type annotations are available via [wezterm-types](https://github.com/DrKJeff16/wezterm-types) for IDE autocomplete and type checking. See [DrKJeff16/wezterm-types#145](https://github.com/DrKJeff16/wezterm-types/pull/145).
