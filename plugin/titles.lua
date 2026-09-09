return function(context)
  local M = context.M
  local defaults = context.defaults
  local report_error_once = context.report_error_once
  local is_safe_text = context.is_safe_text
  local marker_id_by_local = context.marker_id_by_local
  local settled_title_state = {}

  local function normalized_pane_title(value)
    if not is_safe_text(value, 256) then return nil end
    return value
  end

  local function sample_settled_title(cache_key, launch_id, raw_title, provider)
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

  local function title_sources(tab)
    local server_title = type(tab.tab_title) == "string" and tab.tab_title ~= ""
      and tab.tab_title or nil
    local pane = tab.active_pane
    local directory
    if M._active_show_directory ~= false then
      local cwd = pane and pane.current_working_dir
      if cwd then
        local path = cwd.file_path or cwd.path or tostring(cwd)
        local dir_name = string.match(path, "([^/]+)/?$") or ""
        if dir_name ~= "" then directory = dir_name end
      end
    end
    local settled_title = settled_title_for_tab(tab)
    return {
      server_title = server_title,
      directory = directory,
      settled_title = settled_title,
      base_title = server_title or directory or settled_title or "",
    }
  end

  local function default_title(tab)
    return title_sources(tab).base_title
  end

  --- Marker IDs of one tab, translated from the local pane ids the GUI hands to
  --- format-tab-title.

  return {
    normalized_pane_title = normalized_pane_title,
    sample_settled_title = sample_settled_title,
    settled_title_state = settled_title_state,
    settled_title_for_tab = settled_title_for_tab,
    title_sources = title_sources,
    default_title = default_title,
  }
end
