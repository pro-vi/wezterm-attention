local source = debug.getinfo(1, "S").source:sub(2)
local repo_root = source:match("^(.*)/tests/auto_clear_spec.lua$") or "."

local function shell_quote(value)
  return "'" .. value:gsub("'", "'\\''") .. "'"
end

local test_dir = os.tmpname()
os.remove(test_dir)
assert(os.execute("mkdir -p " .. shell_quote(test_dir)) == 0)

local handlers = {}
local wezterm = {
  home_dir = test_dir,
  action_callback = function(callback) return callback end,
  json_parse = function(content)
    return {
      type = content:match('"type"%s*:%s*"([^"]+)"'),
      frame = tonumber(content:match('"frame"%s*:%s*(%d+)')),
      updated_at = tonumber(content:match('"updated_at"%s*:%s*(%d+)')),
    }
  end,
  log_error = function(message)
    error(message)
  end,
  on = function(event, callback)
    handlers[event] = handlers[event] or {}
    table.insert(handlers[event], callback)
  end,
}

package.preload.wezterm = function() return wezterm end

local attention = dofile(repo_root .. "/plugin/init.lua")
local config = {}
attention.apply_to_config(config, {
  auto_poll = false,
  dir = test_dir,
  review_key = false,
})

local format_tab_title = assert(
  handlers["format-tab-title"] and handlers["format-tab-title"][1],
  "format-tab-title handler was not registered"
)

local function write_marker(pane_id, marker_type)
  local file = assert(io.open(test_dir .. "/" .. pane_id, "w"))
  file:write(string.format('{"type":"%s"}', marker_type))
  file:close()
end

local function marker_exists(pane_id)
  local file = io.open(test_dir .. "/" .. pane_id, "r")
  if not file then return false end
  file:close()
  return true
end

local function mux_pane(pane_id)
  return {
    pane_id = function() return pane_id end,
  }
end

local function gui_pane(pane_id)
  return {
    pane_id = pane_id,
    title = "pane-" .. pane_id,
  }
end

local function poll(pane_ids)
  local panes = {}
  for _, pane_id in ipairs(pane_ids) do
    table.insert(panes, mux_pane(pane_id))
  end

  local mux_tab = {
    panes = function() return panes end,
  }
  local mux_window = {
    tabs = function() return { mux_tab } end,
  }
  local window = {
    mux_window = function() return mux_window end,
  }

  attention.poll(window)
end

local function tab(active_pane_id, sibling_pane_id, is_active)
  local active_pane = gui_pane(active_pane_id)
  local sibling_pane = gui_pane(sibling_pane_id)
  return {
    active_pane = active_pane,
    is_active = is_active,
    panes = { active_pane, sibling_pane },
    tab_index = 0,
  }
end

local passed = 0
local failed = 0

local function test(name, callback)
  local ok, err = pcall(callback)
  if ok then
    passed = passed + 1
    io.write("ok - " .. name .. "\n")
  else
    failed = failed + 1
    io.write("not ok - " .. name .. "\n  " .. tostring(err) .. "\n")
  end
end

test("default renderer clears only the focused pane", function()
  write_marker(101, "stop")
  write_marker(102, "notify")
  poll({ 101, 102 })

  local rendered = format_tab_title(tab(101, 102, true))

  assert(not marker_exists(101), "focused pane marker should be cleared")
  assert(marker_exists(102), "unfocused sibling marker should remain")
  assert(attention.get_attention(102) == "notify", "sibling cache should remain")
  assert(type(rendered) == "table", "active tab should render sibling attention styling")
  assert(rendered[2].Text:find("! ", 1, true), "active tab should show sibling indicator")
end)

test("manual title wrapper clears only the focused pane", function()
  write_marker(201, "notify")
  write_marker(202, "stop")
  poll({ 201, 202 })

  local formatter_attention
  local wrapped = attention.wrap_title_formatter(function(_, ctx)
    formatter_attention = ctx.attention[2]
    return "custom title"
  end)
  local rendered = wrapped(tab(201, 202, true))

  assert(not marker_exists(201), "focused pane marker should be cleared")
  assert(marker_exists(202), "unfocused sibling marker should remain")
  assert(type(rendered) == "table", "manual wrapper should render sibling attention styling")
  assert(rendered[2].Text:find("✓ ", 1, true), "manual wrapper should show sibling indicator")
  assert(formatter_attention == "stop", "formatter context should reflect retained sibling attention")
end)

test("inactive tabs do not clear either pane", function()
  write_marker(301, "stop")
  write_marker(302, "notify")
  poll({ 301, 302 })

  format_tab_title(tab(301, 302, false))

  assert(marker_exists(301), "inactive tab active-pane marker should remain")
  assert(marker_exists(302), "inactive tab sibling marker should remain")
end)

test("visiting the sibling pane clears its retained marker", function()
  write_marker(401, "stop")
  write_marker(402, "notify")
  poll({ 401, 402 })

  format_tab_title(tab(401, 402, true))
  assert(marker_exists(402), "unfocused sibling marker should initially remain")

  format_tab_title(tab(402, 401, true))
  assert(not marker_exists(402), "marker should clear once its pane is focused")
end)

test("auto-clear never deletes a newer non-clearable marker", function()
  write_marker(501, "stop")
  write_marker(502, "notify")
  poll({ 501, 502 })

  -- A new turn starts after poll() cached stop but before title rendering.
  write_marker(501, "thinking")
  format_tab_title(tab(501, 502, true))

  assert(marker_exists(501), "newer thinking marker should remain on disk")
  assert(attention.get_attention(501) == "thinking", "cache should adopt the newer marker")
end)

os.execute("rm -rf " .. shell_quote(test_dir))

io.write(string.format("%d passed, %d failed\n", passed, failed))
if failed > 0 then os.exit(1) end
