return function(context)
  local M = context.M
  local defaults = context.defaults
  local report_error_once = context.report_error_once
  local is_safe_text = context.is_safe_text
  local marker_id_by_local = context.marker_id_by_local
  local settled_title_state = {}

  --- Text from a source the plugin does not control -- a directory name a
  --- program chose through OSC 7, a title, a tab name any process in any pane
  --- can set -- made safe to draw and to publish: every control character
  --- removed, C0, DEL and C1 alike, then cut to `max_bytes` on a character
  --- boundary. WezTerm applies escape sequences in a formatter's text, so an
  --- ESC left in would restyle the bar.
  local function display_text(value, max_bytes)
    if type(value) ~= "string" then return nil end
    local text = value:gsub("[%z\1-\31\127]", "")
    -- Repeated because removing one pair can join its neighbours into another.
    local removed
    repeat text, removed = text:gsub("\194[\128-\159]", "") until removed == 0
    if #text > max_bytes then
      local cut = max_bytes
      -- A continuation byte just past the cut means the cut splits a
      -- character; back up to that character's first byte.
      while cut > 0 and text:byte(cut + 1) >= 0x80 and text:byte(cut + 1) < 0xC0 do
        cut = cut - 1
      end
      text = text:sub(1, cut)
    end
    return text
  end

  local function normalized_pane_title(value)
    if not is_safe_text(value, 256) then return nil end
    return value
  end

  local function sample_settled_title(cache_key, launch_id, raw_title, provider)
    if M._active_settled_title_fallback == false then return nil end
    local scope = tostring(launch_id or "v1")
    local title = normalized_pane_title(raw_title)
    local state = settled_title_state[cache_key]
    if not state or state.scope ~= scope then
      state = { scope = scope, candidate = title, repeats = title and 1 or 0, settled = nil, changes = 0 }
      settled_title_state[cache_key] = state
      return nil
    end
    if title == state.candidate then
      if title then
        state.repeats = state.repeats + 1
        if state.repeats >= 2 then
          state.settled = title
          state.changes = 0
        end
      end
      return state.settled
    end
    state.candidate = title
    state.repeats = title and 1 or 0
    state.settled = nil
    state.changes = state.changes + 1
    if state.changes >= 2 then
      local provider_hint = {
        claude = "Claude pane title is changing on every poll; prefer a server tab name or disable Claude title updates",
        codex = "Codex pane title is changing on every poll; prefer a server tab name or disable Codex title updates",
        pi = "Pi pane title is changing on every poll; prefer a server tab name or disable Pi title updates",
      }
      report_error_once("title-churn:" .. cache_key .. ":" .. scope,
        provider_hint[provider] or "pane title is changing on every poll; prefer a server tab name")
    end
    return nil
  end

  local function settled_title_for_tab(tab)
    if M._active_settled_title_fallback == false then return nil end
    local pane = tab.active_pane
    local local_id = pane and tostring(pane.pane_id) or nil
    local key = local_id and marker_id_by_local[local_id] or local_id
    local state = key and settled_title_state[key] or nil
    return state and state.settled or nil
  end

  local function nonempty(value)
    if value == "" then return nil end
    return value
  end

  local function title_sources(tab)
    local server_title = nonempty(display_text(tab.tab_title, 256))
    local pane = tab.active_pane
    local directory
    if M._active_show_directory ~= false then
      local cwd = pane and pane.current_working_dir
      if cwd then
        local path = cwd.file_path or cwd.path or tostring(cwd)
        directory = nonempty(display_text(string.match(path, "([^/]+)/?$"), 256))
      end
    end
    local settled_title = settled_title_for_tab(tab)
    local base_title = server_title or directory or settled_title
    -- Nothing else to go by: the title as it is right now, however often it
    -- changes, rather than a tab with no name at all.
    if not base_title then base_title = display_text(pane and pane.title, 256) or "" end
    return {
      server_title = server_title,
      directory = directory,
      settled_title = settled_title,
      base_title = base_title,
    }
  end

  local function default_title(tab)
    return title_sources(tab).base_title
  end

  --- Marker IDs of one tab, translated from the local pane ids the GUI hands to
  --- format-tab-title.

  return {
    display_text = display_text,
    normalized_pane_title = normalized_pane_title,
    sample_settled_title = sample_settled_title,
    settled_title_state = settled_title_state,
    settled_title_for_tab = settled_title_for_tab,
    title_sources = title_sources,
    default_title = default_title,
  }
end
