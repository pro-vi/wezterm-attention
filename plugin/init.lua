local _, module_loader_path = ...
local wezterm = require("wezterm")

local M = {}

-- ── Defaults ────────────────────────────────────────────────────────────────

local home = wezterm.home_dir or os.getenv("HOME") or os.getenv("USERPROFILE") or "/tmp"

local defaults = {
  -- Where marker files are written (one file per pane ID)
  dir = home .. "/.local/state/wezterm-attention",

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

  -- Stale marker cleanup by type, in milliseconds. Prevents zombie busy tabs
  -- after a process exits without clearing its marker. Set to false to disable.
  stale_after_ms = { thinking = 30 * 60 * 1000 },

  -- Keybind to toggle "review" marker on active pane (false to disable)
  review_key = { key = "b", mods = "ALT" },

  -- Ask WezTerm to rebuild the tab bar when a pane's attention changes.
  -- Set false when the host already repaints titles on its own tick.
  request_redraw = true,

  -- Optional provider suffix. Off preserves the shipped title shape.
  show_provider = false,

  -- Consumers that distinguish puppet activity may hide only that activity;
  -- review claims and subagent counts remain independent.
  show_puppet = true,

  -- Base-title sources: server name, then directory, then a two-poll settled
  -- process title. The raw process title is never read by the formatter.
  show_directory = true,
  settled_title_fallback = true,
}

-- Known attention types (reject unknown values from marker files)
local valid_types = { thinking = true, stop = true, notify = true, review = true }

-- ── V2 protocol authority ───────────────────────────────────────────────────

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
    plugin_root = (working_directory .. "/" .. plugin_root):gsub("/%./", "/")
  end
end
local loaded_module_errors = {}
local function load_plugin_module(name)
  local path = plugin_root and (plugin_root .. "/plugin/" .. name .. ".lua") or nil
  if not path then return nil, "plugin root is unavailable" end
  local chunk, load_error = loadfile(path)
  if not chunk then
    if not loaded_module_errors[name] then
      loaded_module_errors[name] = true
      wezterm.log_error("wezterm-attention: cannot load " .. name .. ": " .. tostring(load_error))
    end
    return nil, load_error
  end
  local ok, module = pcall(chunk)
  if not ok or type(module) ~= "function" then
    local message = ok and "module did not return a factory" or module
    if not loaded_module_errors[name] then
      loaded_module_errors[name] = true
      wezterm.log_error("wezterm-attention: cannot load " .. name .. ": " .. tostring(message))
    end
    return nil, message
  end
  return module
end
local protocol_path = plugin_root and (plugin_root .. "/protocol/v2.json") or nil

local protocol_factory, protocol_module_error = load_plugin_module("protocol")
local protocol_api
if protocol_factory then
  protocol_api = protocol_factory({
    wezterm = wezterm, protocol_path = protocol_path, M = M, defaults = defaults,
  })
else
  local function fallback_diagnostic(code, message, context)
    return { code = code, message = message, context = context or {} }
  end
  local function unavailable()
    return nil, fallback_diagnostic(
      "probe_unavailable", "protocol module is unavailable")
  end
  local function fallback_epoch_ms(value)
    local number = tonumber(value)
    if not number then return nil end
    return number < 100000000000 and number * 1000 or number
  end
  protocol_api = {
    protocol = nil,
    protocol_load_error = protocol_module_error,
    diagnostic = fallback_diagnostic,
    invalid = function(message, context)
      return fallback_diagnostic("record_invalid", message, context)
    end,
    parse_wire_value = unavailable,
    parse_wire_json = unavailable,
    parse_v2_record = unavailable,
    parse_v2_record_json = unavailable,
    normalize_epoch_ms = fallback_epoch_ms,
    now_ms = function() return os.time() * 1000 end,
    frame_for_now = function(poll_now_ms, frame_count)
      return math.floor(poll_now_ms / 1000) % frame_count
    end,
    stale_ttl_ms = function() return nil end,
    is_safe_text = function(value, maximum)
      return type(value) == "string" and value ~= "" and #value <= maximum
    end,
  }
end
local read_all = protocol_api.read_all
local decode_json = protocol_api.decode_json
local protocol = protocol_api.protocol
local protocol_load_error = protocol_api.protocol_load_error
local diagnostic = protocol_api.diagnostic
local invalid = protocol_api.invalid
local sha256 = protocol_api.sha256
local parse_wire_value = protocol_api.parse_wire_value
local parse_wire_json = protocol_api.parse_wire_json
local parse_v2_record = protocol_api.parse_v2_record
local parse_v2_record_json = protocol_api.parse_v2_record_json
local compare_ns20 = protocol_api.compare_ns20
local unix_ns_parts = protocol_api.unix_ns_parts
local format_unix_ns20 = protocol_api.format_unix_ns20
local wezterm_now_unix_ns20 = protocol_api.wezterm_now_unix_ns20
local add_ms_to_unix_ns = protocol_api.add_ms_to_unix_ns
local seconds_until_after = protocol_api.seconds_until_after
local age_exceeds_ms = protocol_api.age_exceeds_ms
local address_cache_key = protocol_api.address_cache_key
local v2_pane_root = protocol_api.v2_pane_root
local binding_root = protocol_api.binding_root
local read_record_file = protocol_api.read_record_file
local record_matches = protocol_api.record_matches
local identity_diagnostic = protocol_api.identity_diagnostic
local read_expected_record = protocol_api.read_expected_record
local read_expected_record_cached = protocol_api.read_expected_record_cached
local path_stem = protocol_api.path_stem
local glob_paths = protocol_api.glob_paths
local read_record_collection = protocol_api.read_record_collection
local collect_diagnostic = protocol_api.collect_diagnostic
local health_from_diagnostics = protocol_api.health_from_diagnostics
local diagnostics_have_unavailable_io = protocol_api.diagnostics_have_unavailable_io
local eligible_subagent = protocol_api.eligible_subagent
local deep_copy = protocol_api.deep_copy
local fixture_set_path = protocol_api.fixture_set_path
local fixture_remove_path = protocol_api.fixture_remove_path
local fixture_case_value = protocol_api.fixture_case_value
local parse_fixture_cases = protocol_api.parse_fixture_cases
local fixture_eligibility_cases = protocol_api.fixture_eligibility_cases
local now_ms = protocol_api.now_ms
local frame_for_now = protocol_api.frame_for_now
local normalize_epoch_ms = protocol_api.normalize_epoch_ms
local stale_ttl_ms = protocol_api.stale_ttl_ms
local is_integer = protocol_api.is_integer
local is_hex64 = protocol_api.is_hex64
local is_uuid = protocol_api.is_uuid
local is_ns20 = protocol_api.is_ns20
local is_canonical_decimal = protocol_api.is_canonical_decimal
local is_safe_text = protocol_api.is_safe_text
local same_address = protocol_api.same_address
local legacy_factory = assert(load_plugin_module("legacy"))
local legacy_api = legacy_factory({
  wezterm = wezterm,
  valid_types = valid_types,
  normalize_epoch_ms = normalize_epoch_ms,
  protocol = protocol,
  diagnostic = diagnostic,
})
local read_marker = legacy_api.read_marker
local subagent_live_ms = legacy_api.subagent_live_ms
local subagents_path = legacy_api.subagents_path
local count_live_subagents = legacy_api.count_live_subagents
local overlays_factory = assert(load_plugin_module("overlays"))
local overlays_api = overlays_factory({
  wezterm = wezterm,
  now_ms = now_ms,
  sha256 = sha256,
  parse_v2_record = parse_v2_record,
  record_matches = record_matches,
  identity_diagnostic = identity_diagnostic,
  read_expected_record = read_expected_record,
  read_marker = read_marker,
  subagents_path = subagents_path,
})
local reported_errors = overlays_api.reported_errors
local publication_session = overlays_api.publication_session
local report_error_once = overlays_api.report_error_once
local next_publication_id = overlays_api.next_publication_id
local json_string = overlays_api.json_string
local json_value = overlays_api.json_value
local next_v2_event_id = overlays_api.next_v2_event_id
local write_v2_record = overlays_api.write_v2_record
local acknowledgement_path = overlays_api.acknowledgement_path
local acknowledgement_tmp_path = overlays_api.acknowledgement_tmp_path
local marker_identity = overlays_api.marker_identity
local read_acknowledgement = overlays_api.read_acknowledgement
local clear_acknowledgement = overlays_api.clear_acknowledgement
local write_acknowledgement = overlays_api.write_acknowledgement
local acknowledgement_matches = overlays_api.acknowledgement_matches
local read_effective_marker = overlays_api.read_effective_marker
local review_path = overlays_api.review_path
local review_tmp_path = overlays_api.review_tmp_path
local review_flagged = overlays_api.review_flagged
local write_review_flag = overlays_api.write_review_flag
local clear_review_flag = overlays_api.clear_review_flag
local remove_expired_marker = overlays_api.remove_expired_marker
local remove_marker = overlays_api.remove_marker
local reader_factory = assert(load_plugin_module("reader"))
local reader_api = reader_factory({
  M = M,
  deep_copy = deep_copy,
  classify_lifecycle_tool = protocol_api.classify_lifecycle_tool,
  protocol = protocol,
  protocol_load_error = protocol_load_error,
  parse_wire_json = parse_wire_json,
  address_cache_key = address_cache_key,
  same_address = same_address,
  diagnostic = diagnostic,
  invalid = invalid,
  diagnostics_have_unavailable_io = diagnostics_have_unavailable_io,
  health_from_diagnostics = health_from_diagnostics,
  collect_diagnostic = collect_diagnostic,
  v2_pane_root = v2_pane_root,
  binding_root = binding_root,
  read_expected_record_cached = read_expected_record_cached,
  read_record_collection = read_record_collection,
  identity_diagnostic = identity_diagnostic,
  age_exceeds_ms = age_exceeds_ms,
  eligible_subagent = eligible_subagent,
})
local canonical_pane_id = reader_api.canonical_pane_id
local pane_call = reader_api.pane_call
local pane_method = reader_api.pane_method
local resolve_pane_read = reader_api.resolve_pane_read
local same_target = reader_api.same_target
local effective_attention_type = reader_api.effective_attention_type
local empty_v2_view = reader_api.empty_v2_view
local read_attention_view = reader_api.read_attention_view
local runtime_factory = assert(load_plugin_module("runtime"))
local runtime_state = runtime_factory()
local attention_cache = runtime_state.attention_cache
local legacy_cache_key_by_marker_id = runtime_state.legacy_cache_key_by_marker_id
local marker_id_by_local = runtime_state.marker_id_by_local
local seen_marker_ids_by_window = runtime_state.seen_marker_ids_by_window
local v2_overlays = overlays_api.bind_v2({
  M = M,
  defaults = defaults,
  protocol = protocol,
  binding_root = binding_root,
  v2_pane_root = v2_pane_root,
  glob_paths = glob_paths,
  read_expected_record = read_expected_record,
  path_stem = path_stem,
  identity_diagnostic = identity_diagnostic,
  read_attention_view = read_attention_view,
  attention_cache = attention_cache,
  legacy_cache_key_by_marker_id = legacy_cache_key_by_marker_id,
  wezterm_now_unix_ns20 = wezterm_now_unix_ns20,
})
local selected_v2_records_root = v2_overlays.selected_v2_records_root
local v2_review_paths = v2_overlays.v2_review_paths
local write_v2_user_review = v2_overlays.write_v2_user_review
local clear_v2_reviews = v2_overlays.clear_v2_reviews
local refresh_cached_v2 = v2_overlays.refresh_cached_v2
local acknowledge_focused_v2_pane = v2_overlays.acknowledge_focused_v2_pane
-- ── Internal helpers ────────────────────────────────────────────────────────

local titles_factory = assert(load_plugin_module("titles"))
local titles_api = titles_factory({
  M = M,
  defaults = defaults,
  report_error_once = report_error_once,
  is_safe_text = is_safe_text,
  marker_id_by_local = marker_id_by_local,
})
local normalized_pane_title = titles_api.normalized_pane_title
local sample_settled_title = titles_api.sample_settled_title
local settled_title_state = titles_api.settled_title_state
local settled_title_for_tab = titles_api.settled_title_for_tab
local title_sources = titles_api.title_sources
local default_title = titles_api.default_title
local format_factory = assert(load_plugin_module("format"))
local format_api = format_factory({
  M = M,
  defaults = defaults,
  marker_id_by_local = marker_id_by_local,
  attention_cache = attention_cache,
  title_sources = title_sources,
})
local gui_tab_pane_ids = format_api.gui_tab_pane_ids
local resolve_visible_attention = format_api.resolve_visible_attention
local decorate_tab_title = format_api.decorate_tab_title
local build_formatter_context = format_api.build_formatter_context
local last_base_title_by_tab = format_api.last_base_title_by_tab
local runtime_api = runtime_state.bind({
  M = M,
  deep_copy = deep_copy,
  defaults = defaults,
  wezterm = wezterm,
  protocol = protocol,
  plugin_root = plugin_root,
  diagnostic = diagnostic,
  report_error_once = report_error_once,
  resolve_pane_read = resolve_pane_read,
  read_attention_view = read_attention_view,
  pane_method = pane_method,
  selected_v2_records_root = selected_v2_records_root,
  v2_review_paths = v2_review_paths,
  write_v2_user_review = write_v2_user_review,
  clear_v2_reviews = clear_v2_reviews,
  refresh_cached_v2 = refresh_cached_v2,
  acknowledge_focused_v2_pane = acknowledge_focused_v2_pane,
  read_effective_marker = read_effective_marker,
  read_marker = read_marker,
  marker_identity = marker_identity,
  clear_acknowledgement = clear_acknowledgement,
  write_acknowledgement = write_acknowledgement,
  count_live_subagents = count_live_subagents,
  review_flagged = review_flagged,
  acknowledgement_matches = acknowledgement_matches,
  remove_expired_marker = remove_expired_marker,
  remove_marker = remove_marker,
  clear_review_flag = clear_review_flag,
  write_review_flag = write_review_flag,
  now_ms = now_ms,
  frame_for_now = frame_for_now,
  stale_ttl_ms = stale_ttl_ms,
  format_unix_ns20 = format_unix_ns20,
  wezterm_now_unix_ns20 = wezterm_now_unix_ns20,
  seconds_until_after = seconds_until_after,
  sample_settled_title = sample_settled_title,
  settled_title_state = settled_title_state,
  gui_tab_pane_ids = gui_tab_pane_ids,
  resolve_visible_attention = resolve_visible_attention,
})
local same_cached_attention = runtime_api.same_cached_attention
local tab_panes_containing = runtime_api.tab_panes_containing
local tab_panes_containing_read = runtime_api.tab_panes_containing_read
local review_outranks = runtime_api.review_outranks
local cache_marker_values = runtime_api.cache_marker_values
local refresh_cached_pane = runtime_api.refresh_cached_pane
local acknowledge_focused_pane = runtime_api.acknowledge_focused_pane
local redraw_window_key = runtime_api.redraw_window_key
local request_tab_bar_redraw = runtime_api.request_tab_bar_redraw
local spawn_republish = runtime_api.spawn_republish
local publish_call_after = runtime_api.publish_call_after
local schedule_publish_retry = runtime_api.schedule_publish_retry
local update_publish_schedule = runtime_api.update_publish_schedule
local schedule_ttl_wakeup = runtime_api.schedule_ttl_wakeup
local function formatter_tab_key(tab)
  return tostring(tab.tab_id or tab.tab_index or tab)
end

local function call_title_formatter(base_fn, tab, ctx)
  local key = formatter_tab_key(tab)
  local ok, base = pcall(base_fn, tab, ctx)
  if ok and type(base) == "string" then
    last_base_title_by_tab[key] = base
    return base
  end
  report_error_once("title-formatter:" .. key,
    "title formatter failed: " .. tostring(ok and "non-string result" or base))
  return last_base_title_by_tab[key] or ctx.default_title
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
    local visible = resolve_visible_attention(gui_tab_pane_ids(tab))

    local ctx = build_formatter_context(tab, visible, {
      tabs = tabs, panes = panes, config = config, hover = hover, max_width = max_width,
    })
    local show_index = not (config and config.show_tab_index_in_tab_bar == false)
    return decorate_tab_title(tab, visible, call_title_formatter(base_fn, tab, ctx), show_index)
  end
end

-- ── apply_to_config ─────────────────────────────────────────────────────────

local applied = false

function M.apply_to_config(config, opts)
  if applied then return end
  applied = true

  opts = opts or {}

  -- Merge options with defaults
  local dir = opts.dir or defaults.dir
  local auto_poll = opts.auto_poll ~= false
  M._active_dir = dir
  local integration_root = opts.integration_root or plugin_root
  if type(integration_root) == "string" and integration_root:sub(1, 1) == "/" then
    M._active_integration_root = integration_root
    config.set_environment_variables = config.set_environment_variables or {}
    config.set_environment_variables.WEZTERM_ATTENTION_ROOT = integration_root
    config.set_environment_variables.WEZTERM_ATTENTION_DIR = dir
  else
    M._active_integration_root = nil
    report_error_once("integration-root", "v2 integration root is unavailable")
  end

  local unix_domains = {}
  for _, domain in ipairs(config.unix_domains or {}) do
    if type(domain) == "table" and type(domain.name) == "string"
        and type(domain.socket_path) == "string" and domain.socket_path:sub(1, 1) == "/" then
      unix_domains[domain.name] = domain.socket_path
    end
  end
  M._active_unix_domains = unix_domains
  local request_redraw = opts.request_redraw
  if request_redraw == nil then request_redraw = defaults.request_redraw end
  M._active_request_redraw = request_redraw ~= false
  M._active_show_provider = opts.show_provider == true
  local show_puppet = opts.show_puppet
  if show_puppet == nil then show_puppet = defaults.show_puppet end
  M._active_show_puppet = show_puppet ~= false
  local show_directory = opts.show_directory
  if show_directory == nil then show_directory = defaults.show_directory end
  M._active_show_directory = show_directory ~= false
  local settled_fallback = opts.settled_title_fallback
  if settled_fallback == nil then settled_fallback = defaults.settled_title_fallback end
  M._active_settled_title_fallback = settled_fallback ~= false

  -- Once, at config load. The Alt+B handler used to do this on the GUI thread
  -- on every press; the directory does not change between presses.
  local quoted_dir = dir:gsub("'", [['\'']])
  os.execute("mkdir -p '" .. quoted_dir .. "'")

  -- Resolve renderer: support both new "renderer" and legacy "format_tab_title"
  local renderer = opts.renderer or defaults.renderer
  if opts.format_tab_title == false then renderer = "manual" end

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
  local stale_after_ms = opts.stale_after_ms
  if stale_after_ms == nil then stale_after_ms = defaults.stale_after_ms end
  M._active_stale_after_ms = stale_after_ms

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

  -- Nothing registers on "pane-destroyed": WezTerm emits no such event, so the
  -- handler that used to be here never once fired and closed panes' markers
  -- were never removed. poll() detects a closed pane by absence instead.

  -- ── Renderer: format-tab-title ────────────────────────────────────────

  if renderer == "tab" then
    wezterm.on("format-tab-title", function(tab, tabs, panes, cfg, hover, max_width)
      -- Read-only. WezTerm may call this at any moment, including for a window
      -- the user is not looking at, so acknowledgement belongs in poll() where
      -- focus is known.
      --
      -- Resolve what this tab shows, including an unfocused sibling marker
      -- on the active tab.
      local visible = resolve_visible_attention(gui_tab_pane_ids(tab))
      local show_index = not (cfg and cfg.show_tab_index_in_tab_bar == false)

      -- Build base title (user callback or default)
      local base
      local ctx = build_formatter_context(tab, visible, {
        tabs = tabs, panes = panes, config = cfg, hover = hover, max_width = max_width,
      })
      if title_formatter then
        base = call_title_formatter(title_formatter, tab, ctx)
      else
        base = ctx.default_title
      end

      return decorate_tab_title(tab, visible, base, show_index)
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
        local mux_win = win:mux_window()
        local target_read = resolve_pane_read(pane)
        local target_id = target_read.marker_id
        if target_read.kind ~= "v1" and target_read.kind ~= "v2" then
          -- A mux-client pane that has not published its $WEZTERM_PANE. Its
          -- local id names some other pane's marker file, so there is nothing
          -- here that can be safely written or removed.
          report_error_once("review-unknown-pane",
            "cannot toggle review: this pane has not published its WEZTERM_PANE user var")
          return
        end
        local panes
        if mux_win then
          panes = tab_panes_containing_read(mux_win:tabs(), target_read)
        end
        panes = panes or { pane }

        -- Decide and act on disk truth, never the cache. poll() rebuilds the
        -- cache from files every tick, so the cache can lag a flag another
        -- window's handler just wrote, and a clear that skipped a pane would
        -- leave the tab lit and unclearable on the next poll.
        --
        -- A pane is flagged if its own sidecar is there, or if its marker file
        -- itself says "review" — that is how an older version of this plugin
        -- wrote the flag, and it is still the same user flag. Effective truth
        -- is what counts for the legacy case: a review marker that was somehow
        -- acknowledged shows nothing, and a press must then flag the pane
        -- rather than silently clear what the user cannot see.
        local function is_flagged(candidate)
          local read = resolve_pane_read(candidate)
          if read.kind == "v2" then return #v2_review_paths(read, dir) > 0 end
          if read.kind ~= "v1" then return false end
          if review_flagged(dir, read.marker_id) then return true end
          return read_effective_marker(dir, read.marker_id) == "review"
        end

        local has_flag = false
        for _, p in ipairs(panes) do
          if is_flagged(p) then
            has_flag = true
            break
          end
        end

        -- Tab already flagged → clear the flag from all its panes. Only the
        -- flag is removed: a sibling's thinking/stop/notify marker belongs to
        -- its writer, and dropping one would drop a completion or failure the
        -- user never saw.
        if has_flag then
          for _, p in ipairs(panes) do
            local read = resolve_pane_read(p)
            if read.kind == "v2" then
              if clear_v2_reviews(read, dir) then refresh_cached_v2(read, dir) end
            elseif read.kind == "v1" then
              local id = read.marker_id
              local cleared = review_flagged(dir, id) and clear_review_flag(dir, id)
              if read_effective_marker(dir, id) == "review" then
                -- The legacy shape. Removing the marker file is the only way to
                -- clear a flag that was written into it, and a marker whose
                -- type is "review" can only ever have been this keybind's.
                remove_expired_marker(dir, id)
                cleared = true
              end
              if cleared then refresh_cached_pane(dir, id, now_ms()) end
            end
          end
          request_tab_bar_redraw(win, pane)
          return
        end

        -- Tab not flagged → flag the focused pane (the active pane of its tab,
        -- always a member of `panes`, so flag and clear stay symmetric).
        --
        -- The flag is written to its own "<id>.review" file, so it never
        -- touches writer-owned truth and needs no permission from it. It used
        -- to be written into the marker file, guarded against overwriting a
        -- process marker — which meant it could only be set on a pane no agent
        -- had ever run in, and pressing Alt+B in an agent pane did nothing.
        if target_read.kind == "v2" and write_v2_user_review(target_read, dir) then
          refresh_cached_v2(target_read, dir)
          request_tab_bar_redraw(win, pane)
        elseif target_read.kind == "v1" and write_review_flag(dir, target_id) then
          refresh_cached_pane(dir, target_id, now_ms())
          request_tab_bar_redraw(win, pane)
        end
      end),
    })
  end
end

-- Internal seams, exposed for the LuaJIT specs only. Not public API.
M._internal = {
  acknowledge_focused_pane = acknowledge_focused_pane,
  resolve_visible_attention = resolve_visible_attention,
  build_formatter_context = build_formatter_context,
  same_cached_attention = same_cached_attention,
  sample_settled_title = sample_settled_title,
  settled_title_state = settled_title_state,
  acknowledgement_tmp_path = acknowledgement_tmp_path,
  review_tmp_path = review_tmp_path,
  protocol = protocol,
  protocol_path = protocol_path,
  parse_wire_value = parse_wire_value,
  parse_wire_json = parse_wire_json,
  parse_v2_record = parse_v2_record,
  parse_v2_record_json = parse_v2_record_json,
  parse_fixture_cases = parse_fixture_cases,
  fixture_eligibility_cases = fixture_eligibility_cases,
  compare_ns20 = compare_ns20,
  sha256 = sha256,
  format_unix_ns20 = format_unix_ns20,
  unix_ns_parts = unix_ns_parts,
  age_exceeds_ms = age_exceeds_ms,
  address_cache_key = address_cache_key,
  resolve_pane_read = resolve_pane_read,
  read_attention_view = read_attention_view,
  attention_cache = attention_cache,
}

return M
