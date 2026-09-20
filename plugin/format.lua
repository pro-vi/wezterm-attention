return function(context)
  local M = context.M
  local defaults = context.defaults
  local marker_id_by_local = context.marker_id_by_local
  local attention_cache = context.attention_cache
  local title_sources = context.title_sources

  local function gui_tab_pane_ids(tab)
    local ids = {}
    for _, p in ipairs(tab.panes) do
      local local_id = tostring(p.pane_id)
      local mapped = marker_id_by_local[local_id]
      if mapped then
        ids[#ids + 1] = mapped
      elseif mapped == nil then
        -- No poll has walked this pane yet. The cache is empty for it either
        -- way, and on a local pane the local id is the marker id, so the
        -- untranslated id keeps single-machine setups rendering immediately.
        ids[#ids + 1] = local_id
      end
      -- mapped == false: a mux-client pane that has not published its
      -- $WEZTERM_PANE. Its local id names some other pane's markers, so it
      -- contributes nothing rather than something wrong.
    end
    return ids
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
    local best_provider = nil
    local best_review   = false
    local best_health   = nil

    local subagents = 0

    for _, id in ipairs(pane_ids) do
      local cached = attention_cache[id]
      if cached then
        subagents = subagents + (cached.subagents or 0)
        -- A count-only entry has no type. It contributes its subagents and never
        -- competes for the tab's marker glyph.
        local candidate_type = cached.type
        if candidate_type then
          local pri = cfg_priority[candidate_type] or 0
          if pri > best_priority then
            best_type     = candidate_type
            best_priority = pri
            best_frame    = candidate_type == "thinking" and cached.frame or nil
            if candidate_type == "review" then
              best_source, best_provider = nil, nil
            else
              best_source, best_provider =
                cached.source, cached.provider
            end
            best_review   = cached.review == true
            best_health   = cached.binding_health
          end
        end
      end
    end

    if not best_type then
      -- No marker anywhere in the tab, but subagents of one of its panes are
      -- still running: show the count alone, tinted as stop. There is no marker
      -- type to name here, so `type` stays nil.
      if subagents > 0 then
        return {
          indicator = "+" .. subagents .. " ", type = nil, color = nil,
          subagents = subagents, source = nil, provider = nil,
        }
      end
      return {
        indicator = "", type = nil, color = nil, subagents = 0,
        source = nil, provider = nil,
      }
    end

    local indicator = ""
    if best_type == "thinking" then
      local frames = cfg_indicators.thinking_frames
      if frames and #frames > 0 then
        indicator = frames[((best_frame or 0) % #frames) + 1]
      end
    elseif cfg_indicators[best_type] then
      indicator = cfg_indicators[best_type]
    end

    -- The count rides inside the indicator's own trailing space, so "✓ " with two
    -- subagents renders "✓+2 " and the tab gains one column, not four.
    if subagents > 0 then
      indicator = indicator:gsub("%s+$", "") .. "+" .. subagents .. " "
    end

    local show_provider = M._active_show_provider == true
    local provider_display = { claude = "Claude", codex = "Codex", pi = "Pi" }
    return {
      indicator = indicator,
      type = best_type,
      color = cfg_colors[best_type],
      subagents = subagents,
      source = best_source,
      provider = best_provider,
      review = best_review,
      binding_health = best_health,
      agent_suffix = show_provider and provider_display[best_provider] or nil,
    }
  end


  local function decorate_tab_title(tab, visible, base, show_index)
    local index = ""
    if show_index ~= false then index = (tab.tab_index + 1) .. ": " end
    local suffix = visible.agent_suffix and (" · " .. visible.agent_suffix) or ""
    local text = " " .. index .. visible.indicator .. base .. suffix .. " "
    if visible.color then
      return {
        { Background = { Color = visible.color } },
        { Text = text },
      }
    end
    return text
  end

  local function build_formatter_context(tab, visible, values)
    values = values or {}
    local titles = title_sources(tab)
    local attention = {
      visible.indicator, visible.type, visible.color,
      indicator = visible.indicator,
      type = visible.type,
      color = visible.color,
      subagents = visible.subagents or 0,
      source = visible.source,
      provider = visible.provider,
      review = visible.review == true,
      binding_health = visible.binding_health,
    }
    return {
      tabs = values.tabs,
      panes = values.panes,
      config = values.config,
      hover = values.hover,
      max_width = values.max_width,
      default_title = titles.base_title,
      server_title = titles.server_title,
      directory = titles.directory,
      settled_title = titles.settled_title,
      -- One home per fact. `attention` carries the v1 positional entries for
      -- formatters written against the shipped API, and named fields for the
      -- rest. There used to be flattened copies of all of them at this level
      -- too, which gave every fact two spellings and no rule for choosing.
      attention = attention,
    }
  end

  local last_base_title_by_tab = {}

  -- What the bar last drew for each tab, keyed by window id then tab id. The
  -- renderer is called once per tab, so a window's list is only complete once
  -- every tab in the callback's own array has been through it.
  local drawn_by_window = {}

  --- The text a formatter return carries, whether it was tinted or not. A tinted
  --- return is a list of format items and the text is the one that has it; an
  --- untinted return is the string itself.
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
  local function drawn_tab_order(tab, tabs, marker_ids, rendered)
    local window_id, tab_id = tab.window_id, tab.tab_id
    local text = drawn_text(rendered)
    if type(window_id) ~= "number" or type(tab_id) ~= "number" then return nil end
    if type(tabs) ~= "table" or text == nil then return nil end
    local drawn = drawn_by_window[window_id]
    if not drawn then
      drawn = {}
      drawn_by_window[window_id] = drawn
    end
    drawn[tab_id] = {
      number = tab.tab_index + 1, text = text, marker_ids = marker_ids,
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
    gui_tab_pane_ids = gui_tab_pane_ids,
    resolve_visible_attention = resolve_visible_attention,
    decorate_tab_title = decorate_tab_title,
    drawn_tab_order = drawn_tab_order,
    build_formatter_context = build_formatter_context,
    last_base_title_by_tab = last_base_title_by_tab,
  }
end
