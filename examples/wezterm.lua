-- Shareable WezTerm config with wezterm-attention plugin
-- Includes: attention system, keybindings, git status bar, session management, QoL
-- Does NOT include: color theme, fonts, transparency, gradients — add your own

local wezterm = require("wezterm")
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")
local resurrect = wezterm.plugin.require("https://github.com/MLFlexer/resurrect.wezterm")

local act = wezterm.action
local config = wezterm.config_builder()

-- ── Attention plugin ────────────────────────────────────────────────────────
-- Tab indicators for CLI tools (Claude Code, Codex, builds, scripts).
-- See: https://github.com/pro-vi/wezterm-attention

attention.apply_to_config(config, { auto_poll = false })

-- ── Keybindings ─────────────────────────────────────────────────────────────

config.keys = {
  -- Tab navigation
  { key = '{', mods = 'SHIFT|ALT', action = act.MoveTabRelative(-1) },
  { key = '}', mods = 'SHIFT|ALT', action = act.MoveTabRelative(1) },

  -- Pane splitting
  { key = 'd', mods = 'CMD', action = act.SplitHorizontal { domain = 'CurrentPaneDomain' } },
  { key = 'd', mods = 'CMD|SHIFT', action = act.SplitVertical { domain = 'CurrentPaneDomain' } },

  -- Pane focus (vim-style)
  { key = 'h', mods = 'ALT', action = act.ActivatePaneDirection 'Left' },
  { key = 'j', mods = 'ALT', action = act.ActivatePaneDirection 'Down' },
  { key = 'k', mods = 'ALT', action = act.ActivatePaneDirection 'Up' },
  { key = 'l', mods = 'ALT', action = act.ActivatePaneDirection 'Right' },

  -- Pane resize
  { key = 'H', mods = 'SHIFT|ALT', action = act.AdjustPaneSize { 'Left', 5 } },
  { key = 'J', mods = 'SHIFT|ALT', action = act.AdjustPaneSize { 'Down', 5 } },
  { key = 'K', mods = 'SHIFT|ALT', action = act.AdjustPaneSize { 'Up', 5 } },
  { key = 'L', mods = 'SHIFT|ALT', action = act.AdjustPaneSize { 'Right', 5 } },

  -- Pane zoom / close
  { key = 'z', mods = 'ALT', action = act.TogglePaneZoomState },
  { key = 'w', mods = 'ALT', action = act.CloseCurrentPane { confirm = true } },

  -- Terminal navigation (macOS — Cmd+Arrow for line, Opt+Arrow for word)
  { key = 'LeftArrow', mods = 'CMD', action = act.SendString("\x01") },
  { key = 'RightArrow', mods = 'CMD', action = act.SendString("\x05") },
  { key = 'LeftArrow', mods = 'OPT', action = act.SendString("\x1bb") },
  { key = 'RightArrow', mods = 'OPT', action = act.SendString("\x1bf") },

  -- Quick Select (file paths, git SHAs, UUIDs)
  { key = 'q', mods = 'ALT', action = act.QuickSelect },

  -- Copy Mode (vim-style selection)
  { key = 'c', mods = 'ALT', action = act.ActivateCopyMode },

  -- Copy Mode + search in one step
  { key = 'p', mods = 'ALT', action = act.Multiple {
    act.ActivateCopyMode,
    act.CopyMode 'EditPattern',
  } },

  -- Command palette + debug overlay
  { key = 'p', mods = 'CMD|SHIFT', action = act.ActivateCommandPalette },
  { key = 'i', mods = 'ALT|SHIFT', action = act.ShowDebugOverlay },

  -- Tab rename
  { key = 'n', mods = 'ALT', action = wezterm.action_callback(function(win, pane)
    win:perform_action(
      act.PromptInputLine {
        description = "Tab name:",
        action = wezterm.action_callback(function(win, pane, name)
          if name and #name > 0 then
            pane:tab():set_title(name)
          end
        end),
      },
      pane
    )
  end) },

  -- Scrollback
  { key = 'DownArrow', mods = 'CMD', action = act.ScrollToBottom },

  -- Fullscreen (non-native — preserves transparency)
  { key = "Enter", mods = "ALT", action = act.ToggleFullScreen },

  -- Session management (resurrect plugin)
  { key = "s", mods = "ALT", action = wezterm.action_callback(function(win, pane)
      win:perform_action(
        act.PromptInputLine {
          description = "Save session as:",
          action = wezterm.action_callback(function(win, pane, name)
            if name and #name > 0 then
              resurrect.state_manager.save_state(resurrect.workspace_state.get_workspace_state(), name)
              win:toast_notification("WezTerm", "Session saved: " .. name, nil, 4000)
            end
          end),
        },
        pane
      )
    end)
  },
  { key = "d", mods = "ALT|SHIFT", action = wezterm.action_callback(function(win, pane)
      resurrect.fuzzy_loader.fuzzy_load(win, pane, function(id)
        resurrect.state_manager.delete_state(id)
      end, { title = "Delete session:", is_fuzzy = true })
    end)
  },
  { key = "r", mods = "ALT", action = wezterm.action_callback(function(win, pane)
      resurrect.fuzzy_loader.fuzzy_load(win, pane, function(id, label)
        local type = string.match(id, "^([^/]+)")
        id = string.match(id, "([^/]+)$")
        id = string.match(id, "(.+)%..+$")
        local opts = {
          relative = true,
          restore_text = true,
          on_pane_restore = resurrect.tab_state.default_on_pane_restore,
        }
        if type == "workspace" then
          local state = resurrect.state_manager.load_state(id, "workspace")
          resurrect.workspace_state.restore_workspace(state, opts)
        elseif type == "window" then
          local state = resurrect.state_manager.load_state(id, "window")
          resurrect.window_state.restore_window(pane:window(), state, opts)
        elseif type == "tab" then
          local state = resurrect.state_manager.load_state(id, "tab")
          resurrect.tab_state.restore_tab(pane:tab(), state, opts)
        end
      end)
    end)
  },
}

-- Tab jump: Ctrl+Alt+1-8
for i = 1, 8 do
  table.insert(config.keys, {
    key = tostring(i),
    mods = 'CTRL|ALT',
    action = wezterm.action_callback(function(win, pane)
      local mux_win = win:mux_window()
      if not mux_win then return end
      local tabs = mux_win:tabs()
      local current_pos = pane:tab():get_index()
      local target_idx = i - 1
      if current_pos == target_idx then
        pane:inject_output("\x07") -- visual bell: already here
      elseif i <= #tabs then
        win:perform_action(act.ActivateTab(target_idx), pane)
      else
        pane:inject_output("\x07") -- visual bell: tab doesn't exist
      end
    end),
  })
end

-- ── Git status bar ──────────────────────────────────────────────────────────
-- Right status: branch +N/-N ?N ↑N | battery | time

config.status_update_interval = 5000

local git_cache = { cwd = "", diff = "", branch = "", untracked = 0, ahead = 0, is_repo = false, last = 0 }

wezterm.on('update-status', function(window, pane)
  -- Poll attention markers (manual mode — plugin doesn't own this event)
  attention.poll(window)

  local git_cells_prefix = {}
  local cwd_uri = pane:get_current_working_dir()
  if cwd_uri then
    local cwd = cwd_uri.file_path or ""
    local now = os.time()

    if cwd ~= git_cache.cwd or (now - git_cache.last) >= 5 then
      git_cache.cwd = cwd
      git_cache.last = now
      git_cache.is_repo = false
      git_cache.diff = ""
      git_cache.branch = ""
      git_cache.untracked = 0
      git_cache.ahead = 0

      local ok, success = pcall(wezterm.run_child_process, {
        "git", "-C", cwd, "rev-parse", "--is-inside-work-tree"
      })
      if ok and success then
        git_cache.is_repo = true
        local ok_b, success_b, branch_out = pcall(wezterm.run_child_process, {
          "git", "-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"
        })
        if ok_b and success_b and branch_out then
          git_cache.branch = branch_out:gsub("%s+", "")
        end
        local ok2, success2, stdout2 = pcall(wezterm.run_child_process, {
          "git", "-C", cwd, "diff", "HEAD", "--shortstat"
        })
        if ok2 and success2 then
          git_cache.diff = stdout2 or ""
        end
        local ok3, success3, stdout3 = pcall(wezterm.run_child_process, {
          "git", "-C", cwd, "status", "--porcelain"
        })
        if ok3 and success3 and stdout3 then
          local count = 0
          for line in stdout3:gmatch("[^\n]+") do
            if line:match("^%?%?") then count = count + 1 end
          end
          git_cache.untracked = count
        end
        local ok4, success4, stdout4 = pcall(wezterm.run_child_process, {
          "git", "-C", cwd, "rev-list", "--count", "@{upstream}..HEAD"
        })
        if ok4 and success4 and stdout4 then
          git_cache.ahead = tonumber(stdout4:match("(%d+)")) or 0
        end
      end
    end

    if git_cache.is_repo then
      local branch = git_cache.branch
      if #branch > 20 then branch = branch:sub(1, 18) .. ".." end
      git_cells_prefix = {
        { Foreground = { Color = "#8BA4B8" } },
        { Text = " " .. branch .. " " },
      }
      local has_changes = false
      if git_cache.diff:match("%d") then
        has_changes = true
        local ins = git_cache.diff:match("(%d+) insertion") or "0"
        local del = git_cache.diff:match("(%d+) deletion") or "0"
        table.insert(git_cells_prefix, { Foreground = { Color = "#3fb950" } })
        table.insert(git_cells_prefix, { Text = "+" .. ins })
        table.insert(git_cells_prefix, { Foreground = { Color = "#f85149" } })
        table.insert(git_cells_prefix, { Text = " -" .. del })
      end
      if git_cache.untracked > 0 then
        has_changes = true
        table.insert(git_cells_prefix, { Foreground = { Color = "#D2A04A" } })
        table.insert(git_cells_prefix, { Text = " ?" .. git_cache.untracked })
      end
      if git_cache.ahead > 0 then
        has_changes = true
        table.insert(git_cells_prefix, { Foreground = { Color = "#58A6FF" } })
        table.insert(git_cells_prefix, { Text = " ↑" .. git_cache.ahead })
      end
      if has_changes then
        table.insert(git_cells_prefix, { Text = "  " })
      else
        table.insert(git_cells_prefix, { Foreground = { Color = "#3fb950" } })
        table.insert(git_cells_prefix, { Text = "✓  " })
      end
    end
  end

  local function git_cells(rest)
    local result = {}
    for _, v in ipairs(git_cells_prefix) do table.insert(result, v) end
    for _, v in ipairs(rest) do table.insert(result, v) end
    return result
  end

  -- Battery
  local bat = ""
  for _, b in ipairs(wezterm.battery_info()) do
    local charge = b.state_of_charge * 100
    local icon = ""
    if charge >= 90 then icon = ""
    elseif charge >= 70 then icon = ""
    elseif charge >= 50 then icon = ""
    elseif charge >= 20 then icon = ""
    else icon = ""
    end
    bat = icon .. " " .. string.format("%.0f%%", charge)
  end

  -- Zoom indicator
  local zoom = ""
  local tab = pane:tab()
  if tab then
    local panes_with_info = tab:panes_with_info()
    if #panes_with_info > 1 then
      for _, p in ipairs(panes_with_info) do
        if p.is_zoomed then
          zoom = "[Z] "
          break
        end
      end
    end
  end

  window:set_left_status("")
  window:set_right_status(wezterm.format(git_cells({
    { Foreground = { Color = "#999999" } },
    { Text = zoom .. bat .. "  " .. os.date("%H:%M") .. " " },
  })))
end)

-- ── Quality of Life ─────────────────────────────────────────────────────────

config.front_end = "WebGpu"
config.scrollback_lines = 10000
config.tab_max_width = 28
config.switch_to_last_active_tab_when_closing_tab = true
config.unzoom_on_switch_pane = true
config.audible_bell = "Disabled"
config.visual_bell = {
  fade_in_function = "EaseIn",
  fade_in_duration_ms = 30,
  fade_out_function = "EaseOut",
  fade_out_duration_ms = 120,
}
config.quick_select_patterns = {
  "[\\w./+-]+\\.[a-z]{1,4}(?::\\d+)?",  -- file paths with line number
  "[0-9a-f]{7,40}",                      -- git SHAs
  "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", -- UUIDs
}

-- Copy mode: vim-style / and ? search
local key_tables = wezterm.gui.default_key_tables()
table.insert(key_tables.copy_mode, { key = '/', mods = 'NONE', action = act.CopyMode 'EditPattern' })
table.insert(key_tables.copy_mode, { key = '?', mods = 'NONE', action = act.CopyMode 'EditPattern' })
table.insert(key_tables.copy_mode, { key = 'n', mods = 'NONE', action = act.CopyMode 'NextMatch' })
table.insert(key_tables.copy_mode, { key = 'N', mods = 'NONE', action = act.CopyMode 'PriorMatch' })
table.insert(key_tables.copy_mode, { key = 'N', mods = 'SHIFT', action = act.CopyMode 'PriorMatch' })
config.key_tables = key_tables

-- Auto-save session every 15 minutes
resurrect.state_manager.periodic_save()

-- ── YOUR THEME HERE ─────────────────────────────────────────────────────────
-- Uncomment and customize:
--
-- config.font = wezterm.font("Your Font")
-- config.font_size = 14
-- config.colors = { ... }
-- config.window_background_opacity = 0.9
-- config.window_padding = { left = 10, right = 10, top = 0, bottom = 8 }

return config
