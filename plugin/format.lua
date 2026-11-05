return function(context)
  local M = context.M
  local wezterm = context.wezterm
  local defaults = context.defaults
  local drawn_pane_key = context.runtime_state.drawn_pane_key
  local published_pane_key = context.runtime_state.published_pane_key
  local attention_cache = context.runtime_state.attention_cache
  local title_sources = context.titles.title_sources
  local display_text = context.titles.display_text

  local function mux_tab_panes(tab_id)
    return wezterm.mux.get_tab(tab_id):panes()
  end

  --- The keys of every pane of a GUI tab, as the bar draws them, or as
  --- `key_of` names them. A zoomed tab's TabInformation lists only the zoomed
  --- pane (WezTerm builds it from `iter_panes()`, which honors zoom);
  --- `MuxTab:panes()` also lists the panes the zoom hides, so a tab marks an
  --- agent in any of its panes. `wezterm.mux.get_tab` raises for a tab the mux
  --- no longer has, and then `tab.panes` stands in, which is empty too: its
  --- getter looks the tab up in the same mux (`add_field_method_get("panes")`
  --- in wezterm-gui/src/termwindow/mod.rs). (As of WezTerm 20260905-195314-b99b1ca2.)
  local function tab_pane_ids(tab, key_of)
    key_of = key_of or drawn_pane_key
    local ids = {}
    local function add(local_id)
      local key = key_of(tostring(local_id))
      if key then ids[#ids + 1] = key end
    end
    local ok, panes = pcall(mux_tab_panes, tab.tab_id)
    if ok and type(panes) == "table" then
      for _, pane in ipairs(panes) do add(pane:pane_id()) end
    else
      for _, p in ipairs(tab.panes) do add(p.pane_id) end
    end
    return ids
  end

  --- The text that says how many sub-agents are running: "+N" for a known
  --- count above zero, "+N?" when some pane's count could not be read, "+?"
  --- when nothing is counted and some count could not be read, and "" when
  --- there is nothing to say.
  local function subagent_count_text(count, uncertain)
    local counted = (count or 0) > 0 and tostring(count) or ""
    if uncertain == true then return "+" .. counted .. "?" end
    if counted ~= "" then return "+" .. counted end
    return ""
  end

  --- Project one tab's panes into exactly what a title formatter can see:
  --- { indicator = string, type = string|nil, color = string|nil }.
  --- Pane IDs are the input, not a GUI or mux tab object, so the renderer and
  --- the poller resolve the same value from the same tab. Panes with no cache
  --- entry contribute nothing.
  local function resolve_visible_attention(pane_ids, opts)
    local cfg_indicators = (opts and opts.indicators) or M._active_indicators or defaults.indicators
    local cfg_colors = (opts and opts.colors) or M._active_colors or defaults.colors
    local cfg_priority = M._active_priority_map or {}

    local best_type     = nil
    local best_priority = -1
    local best_frame    = nil
    local best_source   = nil
    local best_label    = nil
    local best_provider = nil
    local best_health   = nil
    local best_held     = false
    local best_quiet    = false

    local subagents = 0
    local subagents_uncertain = false
    -- Any pane's review, whoever owns it, and not only the winning pane's: a
    -- flag outranked by another pane's type is still on the tab.
    local review = false
    local muted = false

    for _, id in ipairs(pane_ids) do
      local cached = attention_cache[id]
      if cached then
        -- A count-only entry has no type. It contributes its subagents and never
        -- competes for the tab's marker glyph.
        local candidate_type = cached.type
        local held, quiet = cached.turn_end_held == true, cached.turn_end_quiet == true
        local label = cached.label
        muted = muted or cached.muted == true
        -- The user muted this pane: its agent shows nothing until it waits on
        -- the user, and the user's own flag stays, since it is not the agent's.
        if cached.muted == true and candidate_type ~= "notify" then
          candidate_type = cached.review == true and "review" or nil
          held, quiet, label = false, false, nil
        else
          subagents = subagents + (cached.subagents or 0)
          subagents_uncertain = subagents_uncertain or cached.subagents_uncertain == true
        end
        review = review or cached.review == true
        if candidate_type then
          local pri = quiet and 0 or (cfg_priority[candidate_type] or 0)
          if pri > best_priority then
            best_type     = candidate_type
            best_priority = pri
            best_frame    = candidate_type == "thinking" and cached.frame or nil
            best_source   = cached.source
            best_label    = label
            best_provider = cached.provider
            best_health   = cached.binding_health
            best_held     = held
            best_quiet    = quiet
          end
        end
      end
    end
    -- The mute's mark goes in front of whatever else the tab shows.
    local mark = muted and (cfg_indicators.muted or defaults.indicators.muted) or ""

    -- A pane whose count could not be read marks the tab's count with "?":
    -- the other panes' children are counted, and there may be more.
    local count_text = subagent_count_text(subagents, subagents_uncertain)

    if not best_type then
      -- No marker anywhere in the tab, but subagents of one of its panes are
      -- still running, or may be: show the count alone, in the tab's default
      -- colors. There is no marker type to name here, so `type` stays nil.
      if count_text ~= "" then
        return {
          indicator = mark .. count_text .. " ", type = nil, color = nil, glyph = "", priority = -1,
          subagents = subagents, subagents_uncertain = subagents_uncertain,
          source = nil, provider = nil, muted = muted,
        }
      end
      return {
        indicator = mark, type = nil, color = nil, glyph = "", priority = -1, subagents = 0,
        subagents_uncertain = false, source = nil, provider = nil, muted = muted,
      }
    end

    -- The count rides inside the indicator's own trailing space: a stop mark
    -- and its space with two subagents render as the mark, "+2" and the space.
    local function with_count(glyph)
      if count_text ~= "" then return glyph:gsub("%s+$", "") .. count_text .. " " end
      return glyph
    end

    local indicator = ""
    -- What the bar draws for this state whatever the frame. The published copy
    -- of the bar uses it, so a spinner does not rewrite that file every second.
    local still_indicator
    -- The state's own glyph, with no spinner frame, mute mark or count, for a
    -- caller that counts tabs by state (a folded tab group's label).
    local glyph = ""
    if best_type == "thinking" and best_held then
      indicator = cfg_indicators.held or defaults.indicators.held
      glyph = indicator
    elseif best_type == "thinking" then
      local frames = cfg_indicators.thinking_frames
      if frames and #frames > 0 then
        indicator = frames[((best_frame or 0) % #frames) + 1]
        still_indicator = with_count(frames[1])
        glyph = frames[1]
      end
    elseif best_type == "stop" and best_quiet then
      indicator = cfg_indicators.quiet or defaults.indicators.quiet
      glyph = indicator
    elseif cfg_indicators[best_type] then
      indicator = cfg_indicators[best_type]
      glyph = indicator
    end
    glyph = glyph:gsub("%s+$", "")
    indicator = mark .. with_count(indicator)
    if still_indicator then still_indicator = mark .. still_indicator end

    local show_provider = M._active_show_provider == true
    local provider_display = { claude = "Claude", codex = "Codex", pi = "Pi" }
    return {
      indicator = indicator,
      still_indicator = still_indicator or indicator,
      glyph = glyph,
      priority = best_priority,
      type = best_type,
      turn_end_held = best_held,
      turn_end_quiet = best_quiet,
      color = best_quiet and (cfg_colors.quiet or defaults.colors.quiet) or cfg_colors[best_type],
      subagents = subagents,
      subagents_uncertain = subagents_uncertain,
      source = best_source,
      label = best_label,
      provider = best_provider,
      review = review,
      muted = muted,
      binding_health = best_health,
      agent_suffix = show_provider and provider_display[best_provider] or nil,
    }
  end

  -- { highest percentage, glyph } pairs from `pct_to_glyph` in
  -- wezterm-gui/src/tabbar.rs (as of WezTerm 20260905-195314-b99b1ca2): Nerd
  -- Fonts' md_checkbox_blank_circle_outline, then md_circle_slice_1 to _8.
  local progress_glyphs = {
    { 5, "\u{f0130}" }, { 18, "\u{f0a9e}" }, { 31, "\u{f0a9f}" }, { 43, "\u{f0aa0}" },
    { 56, "\u{f0aa1}" }, { 68, "\u{f0aa2}" }, { 81, "\u{f0aa3}" }, { 94, "\u{f0aa4}" },
    { math.huge, "\u{f0aa5}" },
  }

  --- The progress gauge WezTerm's default title draws, which a format-tab-title
  --- callback replaces: `{ glyph = string, ansi_color = string }` from the
  --- tab's active pane for a percentage (green) or an error (red). Nil for no
  --- progress, for a build older than the progress field, and for the spinning
  --- form, which an agent's own mark already covers.
  local function progress_gauge(tab)
    local pane = tab.active_pane
    local progress = pane and pane.progress
    if type(progress) ~= "table" then return nil end
    local value, ansi_color = progress.Percentage, "Green"
    if value == nil then value, ansi_color = progress.Error, "Red" end
    if type(value) ~= "number" then return nil end
    for _, step in ipairs(progress_glyphs) do
      if value <= step[1] then return { glyph = step[2], ansi_color = ansi_color } end
    end
    return nil
  end

  --- Apply the shared attention indicator and color decoration to a base title.
  -- `2: <indicator> name`: the index first, as WezTerm's own default renders it, then the
  -- attention indicator, then the base. `show_index` false drops the index the
  -- way `show_tab_index_in_tab_bar = false` does for the default renderer.
  --- `indicator` replaces the visible one when given.
  --- `warning` goes between the indicator and the gauge.
  --- `gauge`, from progress_gauge, goes just before the base, in its own color.
  --- Returns a string, or a list of format items when the tab is tinted or has
  --- a gauge.
  local function decorate_tab_title(tab, visible, base, show_index, indicator, warning, gauge)
    local index = ""
    if show_index ~= false then index = (tab.tab_index + 1) .. ": " end
    local suffix = visible.agent_suffix and (" · " .. visible.agent_suffix) or ""
    local head = " " .. index .. (indicator or visible.indicator) .. (warning or "")
    local tail = base .. suffix .. " "
    if not gauge then
      if not visible.color then return head .. tail end
      return {
        { Background = { Color = visible.color } },
        { Text = head .. tail },
      }
    end
    local items = {}
    if visible.color then items[1] = { Background = { Color = visible.color } } end
    items[#items + 1] = { Text = head }
    items[#items + 1] = { Foreground = { AnsiColor = gauge.ansi_color } }
    items[#items + 1] = { Text = gauge.glyph .. " " }
    items[#items + 1] = { Foreground = "Default" }
    items[#items + 1] = { Text = tail }
    return items
  end

  --- What the focused tab draws while the attention command answers the
  --- plugin's calls with nothing, the way a command built before the plugin
  --- was updated fails; nil otherwise.
  local function writer_warning(tab)
    if M._writer_outdated ~= true or tab.is_active ~= true then return nil end
    return "⚠ rebuild attention · "
  end

  local function build_formatter_context(tab, visible, values)
    values = values or {}
    local titles = title_sources(tab)
    local attention = {
      visible.indicator, visible.type, visible.color,
      indicator = visible.indicator,
      type = visible.type,
      turn_end_held = visible.turn_end_held == true,
      turn_end_quiet = visible.turn_end_quiet == true,
      color = visible.color,
      subagents = visible.subagents or 0,
      subagents_uncertain = visible.subagents_uncertain == true,
      source = visible.source,
      label = visible.label,
      provider = visible.provider,
      review = visible.review == true,
      muted = visible.muted == true,
      binding_health = visible.binding_health,
    }
    return {
      tabs = values.tabs,
      panes = values.panes,
      config = values.config,
      hover = values.hover,
      max_width = values.max_width,
      default_title = titles.base_title,
      tab_title = titles.tab_title,
      directory = titles.directory,
      settled_title = titles.settled_title,
      -- One home per fact. `attention` carries the positional entries for
      -- formatters written against the first API, and named fields for the
      -- rest. Nothing is copied to this level: two spellings of one fact
      -- leave no rule for choosing.
      attention = attention,
    }
  end

  local last_base_title_by_tab = {}

  -- What the bar last drew for each tab, keyed by window id then tab id. The
  -- renderer is called once per tab, so a window's list is only complete once
  -- every tab in the callback's own array has been through it.
  local drawn_by_window = {}

  --- The text a decorate_tab_title return without a gauge carries: the string
  --- itself, or the one Text item of a tinted list. A return with a gauge holds
  --- three Text items, so its caller publishes a copy drawn without the gauge.
  local function drawn_text(rendered)
    if type(rendered) == "string" then return rendered end
    if type(rendered) ~= "table" then return nil end
    for _, item in ipairs(rendered) do
      if type(item) == "table" and type(item.Text) == "string" then return item.Text end
    end
    return nil
  end

  --- Record what this call drew for one tab, and return the whole window's tabs
  --- in the order the bar draws them once every one of them has been drawn.
  --- Returns nil while the window is still incomplete, so a caller publishes a
  --- whole bar or nothing. No file work happens here: this is the GUI thread.
  local function drawn_tab_order(tab, tabs, rendered)
    local window_id, tab_id = tab.window_id, tab.tab_id
    local text = drawn_text(rendered)
    if type(window_id) ~= "number" or type(tab_id) ~= "number" then return nil end
    if type(tabs) ~= "table" or text == nil then return nil end
    local marker_ids = tab_pane_ids(tab, published_pane_key)
    local drawn = drawn_by_window[window_id]
    if not drawn then
      drawn = {}
      drawn_by_window[window_id] = drawn
    end
    -- The published copy is read by `attention tabs`, which refuses a whole
    -- window's file over one tab text longer than a label or holding a control
    -- character. A user formatter can return either.
    drawn[tab_id] = {
      number = tab.tab_index + 1, text = display_text(text, 256), marker_ids = marker_ids,
    }
    local order, present = {}, {}
    for index, entry in ipairs(tabs) do
      -- WezTerm fills this array with TabInformation userdata, not tables.
      -- `type(entry) == "table"` is true in the luajit harness and false in
      -- the GUI, so a type test here publishes in tests and never in WezTerm.
      local kind = type(entry)
      local entry_id = (kind == "table" or kind == "userdata") and entry.tab_id or nil
      local recorded = type(entry_id) == "number" and drawn[entry_id]
      if not recorded then return nil end
      order[index] = recorded
      present[entry_id] = true
    end
    -- A closed tab's entry would otherwise be held for the life of the process.
    for id in pairs(drawn) do
      if not present[id] then drawn[id] = nil end
    end
    return order, window_id
  end


  return {
    tab_pane_ids = tab_pane_ids,
    subagent_count_text = subagent_count_text,
    resolve_visible_attention = resolve_visible_attention,
    decorate_tab_title = decorate_tab_title,
    progress_gauge = progress_gauge,
    writer_warning = writer_warning,
    drawn_tab_order = drawn_tab_order,
    build_formatter_context = build_formatter_context,
    last_base_title_by_tab = last_base_title_by_tab,
  }
end
