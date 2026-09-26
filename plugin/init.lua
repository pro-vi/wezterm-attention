local _, module_loader_path = ...
local wezterm = require("wezterm")

local M = {}

-- ── Defaults ────────────────────────────────────────────────────────────────

local home = wezterm.home_dir or os.getenv("HOME") or os.getenv("USERPROFILE") or "/tmp"

local function is_absolute_path(path)
  return path:sub(1, 1) == "/" or path:match("^%a:[\\/]") ~= nil or path:sub(1, 2) == "\\\\"
end

local defaults = {
  -- `dir`, the state directory the records are read from, is set from the
  -- state root below.

  -- Render mode: "tab" | "manual"
  --   tab:    plugin owns format-tab-title (default)
  --   manual: plugin registers no tab handler; use wrap_title_formatter() or API
  renderer = "tab",

  -- Tab background tint per attention type (subtle, dark)
  colors = {
    thinking = "#1c1730",
    stop     = "#12271c",
    notify   = "#240f16",
    review   = "#1a1a0c",
  },

  -- Tab text indicators
  indicators = {
    thinking_frames = { "◌ ", "◔ ", "◑ ", "◕ " },
    stop   = "✓ ",
    notify = "! ",
    review = "◆ ",
  },

  -- Higher index = higher priority when multiple panes have attention
  priority = { "thinking", "review", "stop", "notify" },

  -- These types are acknowledged when their pane becomes active
  acknowledge_types = { "stop", "notify" },

  -- Keybind to toggle the user's review flag on the active tab (false to disable)
  review_key = { key = "b", mods = "ALT" },

  -- Ask WezTerm to rebuild the tab bar when a pane's attention changes.
  -- Set false when the host already repaints titles on its own tick.
  request_redraw = true,

  -- Optional provider suffix. Off preserves the shipped title shape.
  show_provider = false,

  -- Base-title sources: server name, then directory, then a two-poll settled
  -- process title, and the title as it is right now only when none of those
  -- has anything to say.
  show_directory = true,
  settled_title_fallback = true,
}

-- ── Modules ─────────────────────────────────────────────────────────────────

local plugin_source = type(module_loader_path) == "string" and module_loader_path or nil
local debug_library = rawget(_G, "debug")
if not plugin_source and debug_library and type(debug_library.getinfo) == "function" then
  plugin_source = debug_library.getinfo(1, "S").source
  if plugin_source:sub(1, 1) == "@" then plugin_source = plugin_source:sub(2) end
end
local plugin_root = plugin_source and plugin_source:match("^(.*)/plugin/init%.lua$") or nil
if plugin_root and plugin_root:sub(1, 1) ~= "/" then
  local working_directory = os.getenv("PWD")
  if working_directory and working_directory:sub(1, 1) == "/" then
    plugin_root = (working_directory .. "/" .. plugin_root):gsub("/%./", "/"):gsub("/%.$", "")
  end
end
if not plugin_root then
  -- Every other module is loaded from beside this file, so without its path
  -- nothing below can work. Say how to give it one.
  error("wezterm-attention: cannot tell which directory the plugin was loaded from. Load it with "
    .. 'wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention"), or from a clone with '
    .. 'loadfile(clone .. "/plugin/init.lua")("wezterm-attention", clone .. "/plugin/init.lua"). '
    .. "dofile passes no module path, and WezTerm's Lua has no debug library to find one.", 0)
end

--- The factory plugin/<name>.lua returns. The modules are factories loaded
--- from this file's own checkout rather than tables loaded with `require`,
--- so each load of the plugin, as a config reload is, gets modules with state
--- of their own.
local function load_plugin_module(name)
  local chunk, load_error = loadfile(plugin_root .. "/plugin/" .. name .. ".lua")
  if not chunk then error("wezterm-attention: cannot load " .. name .. ": " .. tostring(load_error), 0) end
  return chunk()
end
local protocol_path = plugin_root .. "/protocol/v2.json"

local protocol_api = load_plugin_module("protocol")({ wezterm = wezterm, protocol_path = protocol_path })
local overlays = load_plugin_module("overlays")({ wezterm = wezterm, now_ms = protocol_api.now_ms })
local runtime_state = load_plugin_module("runtime")()
-- The runtime reads panes through the reader, and the reader asks the runtime
-- which mux this GUI is, so that one question is looked up when it is asked.
local runtime
local reader = load_plugin_module("reader")({
  M = M,
  wezterm = wezterm,
  defaults = defaults,
  home_dir = home,
  protocol_api = protocol_api,
  own_mux_identity = function() return runtime.own_mux_identity() end,
})
local titles = load_plugin_module("titles")({
  M = M, protocol_api = protocol_api, overlays = overlays, runtime_state = runtime_state,
})
local format = load_plugin_module("format")({
  M = M, defaults = defaults, runtime_state = runtime_state, titles = titles,
})
runtime = runtime_state.bind({
  M = M,
  wezterm = wezterm,
  defaults = defaults,
  protocol_api = protocol_api,
  overlays = overlays,
  reader = reader,
  titles = titles,
})
local report_error_once = overlays.report_error_once
local report_warning_once = overlays.report_warning_once

-- ── State root ──────────────────────────────────────────────────────────────

--- The writer's bound on a root path, `path_max_bytes` in protocol/v2.json.
--- A root is resolved even when that manifest cannot be read, so the bound is
--- spelled here as well.
local path_max_bytes = 4096

--- A root the writer would take as well: at most its path bound in bytes,
--- UTF-8, since the writer reads its environment as text, and no control
--- character, C0, DEL or C1.
local function safe_root_text(path)
  return #path <= path_max_bytes and titles.well_formed_utf8(path) == path
    and not path:find("[%z\1-\31\127]") and not path:find("\194[\128-\159]")
end

--- Why a root the writer would refuse is refused, for the log, which must
--- not repeat the bytes that made it so.
local unsafe_root_problem = "longer than " .. path_max_bytes
  .. " bytes, not UTF-8, or holds a control character"

--- The state root, resolved in the order the attention CLI and the Pi
--- extension use, so a producer, the writer and this reader agree on one
--- directory: WEZTERM_ATTENTION_DIR, then $XDG_STATE_HOME/wezterm-attention,
--- then ~/.local/state/wezterm-attention. An empty value counts as unset, and a
--- relative XDG_STATE_HOME is ignored as the XDG spec says, as is one the
--- writer would refuse. A WEZTERM_ATTENTION_DIR that is relative or that the
--- writer would refuse, and an absolute XDG_STATE_HOME that is not UTF-8, are
--- errors to the CLI; here they are ignored, and the second return says so for the
--- log.
local function resolve_state_root()
  local note
  local explicit = os.getenv("WEZTERM_ATTENTION_DIR")
  if explicit and explicit ~= "" then
    -- Checked first so that the log never repeats a control character.
    if not safe_root_text(explicit) then
      note = "WEZTERM_ATTENTION_DIR is " .. unsafe_root_problem .. ", so it is ignored"
    elseif not is_absolute_path(explicit) then
      note = "WEZTERM_ATTENTION_DIR is not an absolute path, so it is ignored: " .. explicit
    else
      return explicit
    end
  end
  local state_home = os.getenv("XDG_STATE_HOME")
  if state_home and state_home ~= "" then
    if titles.well_formed_utf8(state_home) ~= state_home then
      note = (note and note .. "; " or "") .. "XDG_STATE_HOME is not UTF-8, so it is ignored"
    elseif is_absolute_path(state_home) and safe_root_text(state_home) then
      return (state_home:gsub("(.)/+$", "%1")) .. "/wezterm-attention", note
    end
  end
  return home .. "/.local/state/wezterm-attention", note
end

local default_dir_note
defaults.dir, default_dir_note = resolve_state_root()

local function formatter_tab_key(tab)
  return tostring(tab.tab_id or tab.tab_index or tab)
end

--- The user's base title, repaired the way every text the bar draws is: a
--- formatter often returns a pane's title as the program set it, escape
--- sequences and all, or cuts one by bytes inside a character, which WezTerm
--- then refuses to draw at all. Not cut to a length: that is the bar's to do.
local function call_title_formatter(base_fn, tab, ctx)
  local key = formatter_tab_key(tab)
  local ok, base = pcall(base_fn, tab, ctx)
  if ok and type(base) == "string" then
    base = titles.display_text(base, math.huge)
    format.last_base_title_by_tab[key] = base
    return base
  end
  report_error_once("title-formatter:" .. key,
    "title formatter failed: " .. tostring(ok and "non-string result" or base))
  return format.last_base_title_by_tab[key] or ctx.default_title
end

--- Wrap a user's title function with attention decoration.
--- For renderer = "manual" mode. Returns a function suitable for wezterm.on("format-tab-title", ...).
---
--- Usage:
---   wezterm.on("format-tab-title", attention.wrap_title_formatter(function(tab, ctx)
---     return string.format("%d %s", tab.tab_index + 1, ctx.default_title)
---   end))
function M.wrap_title_formatter(base_fn)
  return function(tab, tabs, panes, config, hover, max_width)
    -- Read-only. WezTerm may call this at any moment, including for a window
    -- the user is not looking at, so acknowledgement belongs in poll() where
    -- focus is known.
    local visible = format.resolve_visible_attention(format.gui_tab_pane_ids(tab))

    local ctx = format.build_formatter_context(tab, visible, {
      tabs = tabs, panes = panes, config = config, hover = hover, max_width = max_width,
    })
    local show_index = not (config and config.show_tab_index_in_tab_bar == false)
    return format.decorate_tab_title(tab, visible, call_title_formatter(base_fn, tab, ctx), show_index)
  end
end

-- ── apply_to_config ─────────────────────────────────────────────────────────

--- Tab orders drawn while this GUI is still asking who it is, by window id. A
--- window keeps the name of its first file for as long as it is open, so a
--- file written now would stay at the unsourced name every GUI shares.
local held_tab_orders = {}

local function publish_drawn_tab_order(dir, window_id, order)
  local status, source = runtime.tab_source_status()
  if status == "pending" then
    held_tab_orders[window_id] = { dir = dir, order = order, drawn_at = protocol_api.now_ms() }
    return
  end
  held_tab_orders[window_id] = nil
  overlays.publish_tab_order(dir, window_id, order, source)
end

--- Ask who this GUI is, and publish what the bar drew meanwhile once there
--- is an answer, or once it is known that none will come.
local function settle_tab_source(socket)
  runtime.acquire_tab_source(socket)
  if next(held_tab_orders) == nil then return end
  local status, source = runtime.tab_source_status()
  if status == "pending" then return end
  for window_id, held in pairs(held_tab_orders) do
    held_tab_orders[window_id] = nil
    overlays.publish_tab_order(held.dir, window_id, held.order, source, held.drawn_at)
  end
end

local applied = false

-- What each option may be. `false` stands for the literal false, which some
-- options take to mean "off".
local option_kinds = {
  dir = { "string" }, renderer = { "string" },
  title_formatter = { "function" }, on_view_change = { "function" },
  colors = { "table" }, indicators = { "table" }, priority = { "table" }, auto_clear = { "table" },
  review_key = { "table", false },
  request_redraw = { "boolean" }, auto_poll = { "boolean" }, show_provider = { "boolean" },
  show_directory = { "boolean" }, settled_title_fallback = { "boolean" },
  integration_root = { "string" },
}
-- What to say about a name that is not an option: the option it is spelled
-- as, or why it went.
local option_notes = {
  acknowledge_types = "the option is auto_clear",
  format_tab_title = 'the option is renderer = "manual"',
  stale_after_ms = "it applied only to flat marker files, which are not read",
}

--- The options as given, less any the plugin cannot use: an unknown name, or
--- a value of the wrong kind, is named once in the log and left out, so the
--- default applies instead of a typo silently changing nothing or a wrong
--- kind failing the whole config.
local function usable_options(opts)
  local usable = {}
  for key, value in pairs(opts) do
    local kinds = option_kinds[key]
    if not kinds then
      local note = option_notes[key]
      report_warning_once("option:" .. tostring(key), "unknown option " .. tostring(key)
        .. " is ignored" .. (note and ("; " .. note) or ""))
    else
      local accepted = false
      for _, kind in ipairs(kinds) do
        if (kind == false and value == false) or type(value) == kind then accepted = true end
      end
      if accepted then
        usable[key] = value
      else
        local names = {}
        for index, kind in ipairs(kinds) do names[index] = kind == false and "false" or ("a " .. kind) end
        report_warning_once("option:" .. key, "option " .. key .. " must be " .. table.concat(names, " or ")
          .. ", not " .. type(value) .. "; the default is used")
      end
    end
  end
  if usable.renderer ~= nil and usable.renderer ~= "tab" and usable.renderer ~= "manual" then
    report_warning_once("option:renderer", 'option renderer must be "tab" or "manual", not "'
      .. usable.renderer .. '"; the default "tab" is used')
    usable.renderer = nil
  end
  -- Exported to every pane, where the attention command refuses a root it
  -- would not write to; the same rule as for WEZTERM_ATTENTION_DIR.
  if usable.dir ~= nil then
    local problem
    -- Checked first so that the log never repeats a control character.
    if not safe_root_text(usable.dir) then
      problem = unsafe_root_problem
    elseif not is_absolute_path(usable.dir) then
      problem = "not an absolute path: " .. usable.dir
    end
    if problem then
      report_warning_once("option:dir", "option dir is " .. problem
        .. ", so the attention command would refuse it; the default is used")
      usable.dir = nil
    end
  end
  -- Values inside the option tables, each checked where it is used: a wrong
  -- one is named and left out, so the default for that one entry applies.
  local function usable_entries(name, value, rules)
    if value == nil then return nil end
    local kept = {}
    for key, entry in pairs(value) do
      local rule = rules[key]
      if rule and not rule.check(entry) then
        report_warning_once("option:" .. name .. "." .. tostring(key), "option " .. name .. "."
          .. tostring(key) .. " must be " .. rule.kind .. ", not " .. type(entry) .. "; the default is used")
      else
        kept[key] = entry
      end
    end
    return kept
  end
  local function is_string(entry) return type(entry) == "string" end
  local text = { kind = "a string", check = is_string }
  usable.indicators = usable_entries("indicators", usable.indicators, {
    thinking_frames = { kind = "a non-empty list of strings", check = function(entry)
      if type(entry) ~= "table" or #entry == 0 then return false end
      local count = 0
      for _, frame in pairs(entry) do
        count = count + 1
        if type(frame) ~= "string" then return false end
      end
      return count == #entry
    end },
    stop = text, notify = text, review = text,
  })
  usable.colors = usable_entries("colors", usable.colors,
    { thinking = text, stop = text, notify = text, review = text })
  local review_key = usable.review_key
  if review_key and (type(review_key.key) ~= "string"
      or (review_key.mods ~= nil and type(review_key.mods) ~= "string")) then
    report_warning_once("option:review_key", 'option review_key must be a table like '
      .. '{ key = "b", mods = "ALT" }, with key a string and mods a string or absent; the default is used')
    usable.review_key = nil
  end
  return usable
end

function M.apply_to_config(config, opts)
  if applied then return end
  applied = true

  opts = usable_options(opts or {})
  M._on_view_change = opts.on_view_change

  -- Merge options with defaults
  local dir = opts.dir or defaults.dir
  if not opts.dir and default_dir_note then report_warning_once("state-root", default_dir_note) end
  local auto_poll = opts.auto_poll ~= false
  M._active_dir = dir
  local integration_root = opts.integration_root or plugin_root
  M._active_writer_installed = false
  if type(integration_root) == "string" and integration_root:sub(1, 1) == "/" then
    M._active_integration_root = integration_root
    config.set_environment_variables = config.set_environment_variables or {}
    -- Every writer resolves the state directory from this first, so the
    -- plugin and they agree on one.
    config.set_environment_variables.WEZTERM_ATTENTION_DIR = dir
    -- A producer that sees the root runs its bin/attention. Exported only once
    -- the writer is built: before that every callback would fail at the
    -- shim's missing-binary guard, and a producer without the root says
    -- instead that the command is not installed.
    local writer_path = integration_root .. "/libexec/attention-rs"
    local writer = io.open(writer_path, "r")
    if writer then
      writer:close()
      M._active_writer_installed = true
      config.set_environment_variables.WEZTERM_ATTENTION_ROOT = integration_root
    else
      -- Once per config load. The usual cause is an install-cli.sh run in a
      -- clone of the user's own while wezterm.plugin.require loads another
      -- copy, which leaves every agent writing nothing with nothing to say why.
      report_warning_once("integration-writer", writer_path .. " is missing, so panes get no "
        .. "WEZTERM_ATTENTION_ROOT and agents record nothing. Run scripts/install-cli.sh in "
        .. integration_root .. ", or set integration_root to the clone where it was run.")
    end
  else
    M._active_integration_root = nil
    report_error_once("integration-root", "v2 integration root is unavailable")
  end

  -- Its domain lists are read when a poll first needs them, not now: a config
  -- may set them after this call.
  M._active_config = config
  reader.refresh_domain_facts()
  local request_redraw = opts.request_redraw
  if request_redraw == nil then request_redraw = defaults.request_redraw end
  M._active_request_redraw = request_redraw ~= false
  M._active_show_provider = opts.show_provider == true
  local show_directory = opts.show_directory
  if show_directory == nil then show_directory = defaults.show_directory end
  M._active_show_directory = show_directory ~= false
  local settled_fallback = opts.settled_title_fallback
  if settled_fallback == nil then settled_fallback = defaults.settled_title_fallback end
  M._active_settled_title_fallback = settled_fallback ~= false
  if not M._active_settled_title_fallback then
    for key in pairs(titles.settled_title_state) do titles.settled_title_state[key] = nil end
  end

  -- Once, at config load. The Alt+B handler used to do this on the GUI thread
  -- on every press; the directory does not change between presses.
  -- `tabs/` holds one file per GUI window, so it is made with the state
  -- directory rather than on the tab formatter's own thread.
  if package.config:sub(1, 1) == "\\" then
    -- cmd.exe has no -p: its mkdir makes the missing parents itself, and fails
    -- on a directory that is already there.
    local tabs_dir = (dir .. "/tabs"):gsub("/", "\\")
    os.execute('if not exist "' .. tabs_dir .. '" mkdir "' .. tabs_dir .. '"')
  else
    -- Private from the first moment, not from the writer's first chmod: the
    -- GUI's umask would otherwise leave the root and every file the plugin
    -- writes readable by other users until an agent first claims a pane.
    local quoted_dir = dir:gsub("'", [['\'']])
    os.execute("umask 077; mkdir -p '" .. quoted_dir .. "/tabs' && chmod 700 '"
      .. quoted_dir .. "' '" .. quoted_dir .. "/tabs'")
  end

  local renderer = opts.renderer or defaults.renderer
  runtime.reset_tab_source()

  local title_formatter = opts.title_formatter -- optional user callback

  local colors = {}
  for k, v in pairs(defaults.colors) do colors[k] = v end
  if opts.colors then
    for k, v in pairs(opts.colors) do colors[k] = v end
  end
  M._active_colors = colors

  local indicators = {}
  for k, v in pairs(defaults.indicators) do indicators[k] = v end
  if opts.indicators then
    for k, v in pairs(opts.indicators) do indicators[k] = v end
  end
  M._active_indicators = indicators

  local acknowledge_types = opts.auto_clear or defaults.acknowledge_types
  local priority   = opts.priority   or defaults.priority

  -- Build lookup tables
  local acknowledge_set = {}
  for _, t in ipairs(acknowledge_types) do acknowledge_set[t] = true end
  M._active_acknowledge_set = acknowledge_set

  local priority_map = {}
  for i, t in ipairs(priority) do priority_map[t] = i end
  M._active_priority_map = priority_map

  -- ── Poller: update-status ─────────────────────────────────────────────

  if auto_poll then
    wezterm.on("update-status", function(window, pane)
      M.poll(window, { active_pane = pane })
    end)
  end

  -- Nothing registers on "pane-destroyed": WezTerm emits no such event.
  -- poll() detects a closed pane by absence instead.

  -- ── Renderer: format-tab-title ────────────────────────────────────────

  -- Whatever the renderer: a local pane's published identity is checked
  -- against this answer as well as the bar publishing under it.
  -- This callback may yield; pane-state polling has already finished in its own callback.
  wezterm.on("update-status", function()
    settle_tab_source(os.getenv("WEZTERM_UNIX_SOCKET"))
  end)

  if renderer == "tab" then
    wezterm.on("format-tab-title", function(tab, tabs, panes, cfg, hover, max_width)
      -- Read-only. WezTerm may call this at any moment, including for a window
      -- the user is not looking at, so acknowledgement belongs in poll() where
      -- focus is known.
      --
      -- Resolve what this tab shows, including an unfocused sibling marker
      -- on the active tab.
      local marker_ids = format.gui_tab_pane_ids(tab)
      local visible = format.resolve_visible_attention(marker_ids)
      local show_index = not (cfg and cfg.show_tab_index_in_tab_bar == false)

      -- Build base title (user callback or default)
      local base
      local ctx = format.build_formatter_context(tab, visible, {
        tabs = tabs, panes = panes, config = cfg, hover = hover, max_width = max_width,
      })
      if title_formatter then
        base = call_title_formatter(title_formatter, tab, ctx)
      else
        base = ctx.default_title
      end

      local rendered = format.decorate_tab_title(tab, visible, base, show_index)

      -- Nothing outside this process can see the order the bar draws, so the
      -- bar publishes it. Only a window whose every tab has been drawn, and
      -- only when the drawn list or its source identity changes: an ordinary
      -- redraw with the same source touches no file.
      local published = rendered
      if visible.still_indicator ~= visible.indicator then
        published = format.decorate_tab_title(tab, visible, base, show_index, visible.still_indicator)
      end
      local order, window_id = format.drawn_tab_order(tab, tabs, marker_ids, published)
      if order then publish_drawn_tab_order(dir, window_id, order) end

      return rendered
    end)
  end
  -- renderer == "manual": no format-tab-title registered

  -- ── Review toggle keybind ─────────────────────────────────────────────

  local review_key = opts.review_key
  if review_key == nil then review_key = defaults.review_key end

  if review_key then
    config.keys = config.keys or {}
    table.insert(config.keys, {
      key  = review_key.key,
      mods = review_key.mods,
      action = wezterm.action_callback(function(win, pane)
        -- The review indicator is tab-level: resolve_visible_attention lights the tab
        -- if ANY of its panes is flagged. So toggle across every pane in the
        -- focused pane's tab — toggling only the focused pane leaves a split
        -- tab stuck showing ◆ (the other pane is still flagged) and unclearable.
        -- Panes not in any tab (GUI overlays) fall back to per-pane behavior.
        -- The flag toggled is the user's own: another owner's review is that
        -- owner's to withdraw, and a press leaves it.
        local mux_win = win:mux_window()
        local target_read = reader.resolve_pane_read(pane)
        if target_read.kind ~= "v2" then
          -- Nothing has claimed the pane, or it has not published who it is,
          -- so no reader would show a flag written for it.
          report_error_once("review-unclaimed-pane",
            "cannot toggle review: this pane has published no agent launch")
          return
        end
        local panes
        if mux_win then
          -- The same race the poll walks carry: a tab can go between the listing
          -- and the read. Falling back to this pane alone is better than a key
          -- press that does nothing.
          local tabs_ok, mux_tabs = pcall(mux_win.tabs, mux_win)
          if tabs_ok and type(mux_tabs) == "table" then
            panes = runtime.tab_panes_containing_read(mux_tabs, target_read)
          end
        end
        panes = panes or { pane }
        local reads = {}
        for _, observed_pane in ipairs(panes) do
          local read = reader.resolve_pane_read(observed_pane)
          runtime.observe_pane(win, observed_pane, read)
          if read.kind == "v2" then reads[#reads + 1] = read end
        end

        -- Decide on disk truth, never the cache. poll() rebuilds the cache
        -- from files every tick, so the cache can lag a flag another window's
        -- handler just wrote, and a clear that skipped a pane would leave the
        -- tab lit and unclearable on the next poll.
        local flagged = {}
        for _, read in ipairs(reads) do
          if runtime.user_review_present(read, dir) then flagged[#flagged + 1] = read end
        end

        -- Tab already flagged → clear the user's flag from all its panes;
        -- otherwise flag the focused pane, the active pane of its tab and
        -- always a member of `panes`, so flag and clear stay symmetric.
        local written = {}
        if #flagged > 0 then
          for _, read in ipairs(flagged) do
            if runtime.run_plugin_write("clear-review", read, dir) then
              written[#written + 1] = read
            end
          end
        elseif runtime.run_plugin_write("set-review", target_read, dir) then
          written[1] = target_read
        end
        if #written == 0 then return end
        for _, read in ipairs(written) do runtime.refresh_cached_v2(read, dir) end
        runtime.request_tab_bar_redraw(win, pane)
      end),
    })
  end
end

-- Internal seams, exposed for the LuaJIT specs only. Not public API.
M._internal = {
  tab_source = runtime.tab_source,
  reset_tab_source = runtime.reset_tab_source,
  acquire_tab_source = settle_tab_source,
  parse_tab_source_response = runtime.parse_tab_source_response,
  lifecycle_facet = reader.lifecycle_facet,
  resolve_visible_attention = format.resolve_visible_attention,
  same_cached_attention = runtime.same_cached_attention,
  sample_settled_title = titles.sample_settled_title,
  settled_title_state = titles.settled_title_state,
  protocol_path = protocol_path,
  parse_wire_value = protocol_api.parse_wire_value,
  parse_wire_json = protocol_api.parse_wire_json,
  parse_v2_record = protocol_api.parse_v2_record,
  parse_v2_record_json = protocol_api.parse_v2_record_json,
  -- Read by the fixture interpreter in tests/lua/support, which drives these
  -- production functions from outside rather than living beside them.
  deep_copy = protocol_api.deep_copy,
  eligible_subagent = protocol_api.eligible_subagent,
  compare_ns20 = protocol_api.compare_ns20,
  sha256 = protocol_api.sha256,
  format_unix_ns20 = protocol_api.format_unix_ns20,
  unix_ns_parts = protocol_api.unix_ns_parts,
  age_exceeds_ms = protocol_api.age_exceeds_ms,
  address_cache_key = protocol_api.address_cache_key,
  resolve_pane_read = reader.resolve_pane_read,
  read_attention_view = reader.read_attention_view,
  attention_cache = runtime_state.attention_cache,
}

return M
