return function(context)
  local wezterm = context.wezterm
  local now_ms = context.now_ms
  local sha256 = context.sha256
  local parse_v2_record = context.parse_v2_record
  local record_matches = context.record_matches
  local identity_diagnostic = context.identity_diagnostic
  local read_expected_record = context.read_expected_record
  local read_marker = context.read_marker
  local subagents_path = context.subagents_path

  local reported_errors = {}
  local publication_counter = 0
  local publication_session = tostring({}):gsub("[^%w]", "")
  local report_error_once

  local function next_publication_id()
    publication_counter = publication_counter + 1
    return table.concat({
      tostring(math.floor(now_ms())), publication_session, publication_counter,
    }, "-")
  end

  local function json_string(value)
    return '"' .. value:gsub('[%z\1-\31\\"]', function(char)
      local escapes = {
        ['"'] = '\\"', ["\\"] = "\\\\", ["\b"] = "\\b", ["\f"] = "\\f",
        ["\n"] = "\\n", ["\r"] = "\\r", ["\t"] = "\\t",
      }
      return escapes[char] or string.format("\\u%04x", char:byte())
    end) .. '"'
  end

  local function json_value(value)
    local value_type = type(value)
    if value_type == "string" then return json_string(value) end
    if value_type == "number" or value_type == "boolean" then return tostring(value) end
    if value_type ~= "table" then return nil end
    local keys = {}
    for key in pairs(value) do
      if type(key) ~= "string" then return nil end
      keys[#keys + 1] = key
    end
    table.sort(keys)
    local fields = {}
    for _, key in ipairs(keys) do
      local encoded = json_value(value[key])
      if not encoded then return nil end
      fields[#fields + 1] = json_string(key) .. ":" .. encoded
    end
    return "{" .. table.concat(fields, ",") .. "}"
  end

  local function next_v2_event_id()
    local digest = sha256(next_publication_id())
    return table.concat({
      digest:sub(1, 8), digest:sub(9, 12), "4" .. digest:sub(14, 16),
      "8" .. digest:sub(18, 20), digest:sub(21, 32),
    }, "-")
  end

  local function write_v2_record(path, record, kind, expected)
    local parsed, parse_diagnostic = parse_v2_record(record, kind)
    if not parsed or (expected and not record_matches(parsed, expected)) then
      local item = parse_diagnostic or identity_diagnostic(kind, path)
      report_error_once("write-v2:" .. path, item.code .. ": " .. item.message)
      return false
    end
    local existing, existing_diagnostic, status = read_expected_record(
      path, kind, expected, false)
    if status ~= "missing" and not existing then
      report_error_once("write-v2-existing:" .. path,
        existing_diagnostic.code .. ": refusing to replace " .. path)
      return false
    end
    local body = json_value(record)
    if not body then return false end
    local tmp = path .. "." .. publication_session .. ".tmp"
    os.remove(tmp)
    local file, open_err = io.open(tmp, "w")
    if not file then
      report_error_once("write-v2-open:" .. path,
        "failed to write " .. tmp .. ": " .. tostring(open_err))
      return false
    end
    local wrote, write_err = file:write(body .. "\n")
    local closed, close_err = file:close()
    if not wrote or not closed then
      os.remove(tmp)
      report_error_once("write-v2-finish:" .. path,
        "failed to finish " .. tmp .. ": " .. tostring(write_err or close_err))
      return false
    end
    local renamed, rename_err = os.rename(tmp, path)
    if not renamed then
      os.remove(tmp)
      report_error_once("write-v2-rename:" .. path,
        "failed to place " .. path .. ": " .. tostring(rename_err))
      return false
    end
    return true
  end

  local function acknowledgement_path(dir, pane_id)
    return dir .. "/" .. pane_id .. ".ack"
  end

  --- The in-flight sidecar this process writes before renaming it into place.
  --- The name carries this process's session token, so two WezTerm processes
  --- acknowledging the same pane never delete each other's in-flight file while
  --- it is being renamed. A bare "<id>.ack.tmp" was shared state.
  local function acknowledgement_tmp_path(dir, pane_id)
    return acknowledgement_path(dir, pane_id) .. "." .. publication_session .. ".tmp"
  end

  local function marker_identity(raw, publication_id)
    if publication_id then return "publication\n" .. publication_id end
    return "raw\n" .. (raw or "")
  end

  local function read_acknowledgement(dir, pane_id)
    local file = io.open(acknowledgement_path(dir, pane_id), "r")
    if not file then return nil end
    local identity = file:read("*a")
    file:close()
    return identity
  end

  --- Log a message once per key, so a persistent failure does not fill the log
  --- on every poll tick.
  report_error_once = function(key, message)
    if reported_errors[key] then return end
    reported_errors[key] = true
    wezterm.log_error("wezterm-attention: " .. message)
  end

  local function clear_acknowledgement(dir, pane_id)
    local path = acknowledgement_path(dir, pane_id)
    local existing = io.open(path, "r")
    if not existing then return true end
    existing:close()

    local ok, err = os.remove(path)
    if ok then return true end
    report_error_once(
      "clear:" .. pane_id,
      "failed to remove acknowledgement " .. path .. ": " .. tostring(err))
    return false
  end

  local function write_acknowledgement(dir, pane_id, identity)
    local path = acknowledgement_path(dir, pane_id)
    local tmp = acknowledgement_tmp_path(dir, pane_id)
    os.remove(tmp)
    local file, open_err = io.open(tmp, "w")
    if not file then
      report_error_once(
        "write:" .. pane_id,
        "failed to write acknowledgement " .. tmp .. ": " .. tostring(open_err))
      return false
    end

    local wrote, write_err = file:write(identity)
    local closed, close_err = file:close()
    if not wrote or not closed then
      os.remove(tmp)
      report_error_once(
        "write:" .. pane_id,
        "failed to finish acknowledgement " .. tmp .. ": " .. tostring(write_err or close_err))
      return false
    end

    -- No unlink of `path` here. os.rename replaces an existing file atomically on
    -- POSIX, so removing the old sidecar first would only open a window in which
    -- a concurrent reader sees no acknowledgement and re-displays a marker the
    -- user already looked at.
    local renamed, rename_err = os.rename(tmp, path)
    if not renamed then
      os.remove(tmp)
      report_error_once(
        "write:" .. pane_id,
        "failed to place acknowledgement " .. path .. ": " .. tostring(rename_err))
      return false
    end
    return true
  end

  local function acknowledgement_matches(dir, pane_id, raw, publication_id)
    local acknowledged = read_acknowledgement(dir, pane_id)
    if raw and acknowledged == marker_identity(raw, publication_id) then return true end

    -- Cleanup is best-effort. A stale sidecar never suppresses absent or
    -- mismatched canonical truth even when it cannot be removed.
    if acknowledged then clear_acknowledgement(dir, pane_id) end
    return false
  end

  local function read_effective_marker(dir, pane_id)
    -- A crash can strand only this process's own temporary sidecar. It was never
    -- authoritative. Another process's in-flight temp is not ours to remove: it
    -- may be one instant away from being renamed into place.
    os.remove(acknowledgement_tmp_path(dir, pane_id))
    local atype, frame, updated_at, marker_ttl_ms, raw, publication_id, source, puppet =
      read_marker(dir, pane_id)
    if acknowledgement_matches(dir, pane_id, raw, publication_id) then return nil end
    return atype, frame, updated_at, marker_ttl_ms, raw, publication_id, source, puppet
  end

  -- ── Review flag sidecar ─────────────────────────────────────────────────────
  -- The Alt+B review flag is the user's own, and it lives in its own file:
  --   <marker id>.review   {"publication_id":"<id>"}
  -- It used to be written into the marker file as {"type":"review"}, which meant
  -- it could only ever be set on a pane no process had written to. Every pane an
  -- agent has run in carries a thinking/stop/notify marker, so on exactly the
  -- panes the user wants to flag, Alt+B did nothing at all. As a sidecar the flag
  -- coexists with writer-owned truth instead of competing for the same file.

  local function review_path(dir, pane_id)
    return dir .. "/" .. pane_id .. ".review"
  end

  --- The in-flight flag this process writes before renaming it into place. Named
  --- with this process's session token for the same reason the acknowledgement
  --- temp is: two WezTerm processes flagging the same pane must not delete each
  --- other's file mid-rename.
  local function review_tmp_path(dir, pane_id)
    return review_path(dir, pane_id) .. "." .. publication_session .. ".tmp"
  end

  --- Is this pane flagged for review? The file's presence is the flag. Its body
  --- is read by nothing, so content that no parser would accept still counts as
  --- flagged: a truncated write must never silently drop a flag the user set by
  --- hand.
  local function review_flagged(dir, pane_id)
    local file = io.open(review_path(dir, pane_id), "r")
    if not file then return false end
    file:close()
    return true
  end

  local function write_review_flag(dir, pane_id)
    local path = review_path(dir, pane_id)
    local tmp = review_tmp_path(dir, pane_id)
    os.remove(tmp)

    local file, open_err = io.open(tmp, "w")
    if not file then
      wezterm.log_error(
        "wezterm-attention: failed to write review flag " .. tmp .. ": " .. tostring(open_err))
      return false
    end

    local wrote, write_err = file:write(
      '{"publication_id":"' .. next_publication_id() .. '"}')
    local closed, close_err = file:close()
    if not wrote or not closed then
      os.remove(tmp)
      wezterm.log_error(
        "wezterm-attention: failed to finish review flag " .. tmp .. ": "
          .. tostring(write_err or close_err))
      return false
    end

    local renamed, rename_err = os.rename(tmp, path)
    if not renamed then
      os.remove(tmp)
      wezterm.log_error(
        "wezterm-attention: failed to place review flag " .. path .. ": " .. tostring(rename_err))
      return false
    end
    return true
  end

  local function clear_review_flag(dir, pane_id)
    os.remove(review_tmp_path(dir, pane_id))
    local path = review_path(dir, pane_id)
    local existing = io.open(path, "r")
    if not existing then return true end
    existing:close()

    local ok, err = os.remove(path)
    if ok then return true end
    report_error_once(
      "clear-review:" .. pane_id,
      "failed to remove review flag " .. path .. ": " .. tostring(err))
    return false
  end

  --- Remove the writer's marker and the acknowledgement that referred to it, and
  --- nothing else. This is what an expired marker costs: the subagent sidecar and
  --- the user's review flag have their own lifetimes and neither of them aged out
  --- because a spinner did.
  local function remove_expired_marker(dir, pane_id)
    os.remove(dir .. "/" .. pane_id)
    clear_acknowledgement(dir, pane_id)
    os.remove(acknowledgement_tmp_path(dir, pane_id))
  end

  --- Remove every file this plugin knows a pane by. The subagent sidecar and the
  --- review flag go with the marker: the pane they described is gone (the absence
  --- sweep) or the caller asked for that pane's state to be cleared, and a
  --- surviving sidecar would keep a "+N" or a ◆ on a tab whose pane is no longer
  --- there to retract it.
  local function remove_marker(dir, pane_id)
    remove_expired_marker(dir, pane_id)
    os.remove(subagents_path(dir, pane_id))
    clear_review_flag(dir, pane_id)
  end


  local function bind_v2(context)
    local M = context.M
    local defaults = context.defaults
    local protocol = context.protocol
    local binding_root = context.binding_root
    local v2_pane_root = context.v2_pane_root
    local glob_paths = context.glob_paths
    local read_expected_record = context.read_expected_record
    local path_stem = context.path_stem
    local identity_diagnostic = context.identity_diagnostic
    local read_attention_view = context.read_attention_view
    local attention_cache = context.attention_cache
    local legacy_cache_key_by_marker_id = context.legacy_cache_key_by_marker_id
    local wezterm_now_unix_ns20 = context.wezterm_now_unix_ns20

    local function selected_v2_records_root(dir, read, binding_id)
      if binding_id then
        return binding_root(dir, read.address, read.launch_id, binding_id), {
          kind = "binding", binding_id = binding_id,
        }
      end
      return v2_pane_root(dir, read.address) .. "/launches/" .. read.launch_id,
        { kind = "launch" }
    end

    local function v2_review_paths(read, dir)
      local pattern = v2_pane_root(dir, read.address) .. "/reviews/*.json"
      local paths, glob_diagnostic = glob_paths(pattern)
      if not paths then
        report_error_once("review-enumerate:" .. read.cache_key,
          glob_diagnostic.code .. ": " .. glob_diagnostic.message)
        return {}
      end
      local valid = {}
      for _, path in ipairs(paths) do
        local record, record_diagnostic = read_expected_record(
          path, "review", { address = read.address }, true)
        if record and path_stem(path) == record.owner_key then
          valid[#valid + 1] = path
        else
          local item = record_diagnostic or identity_diagnostic("review", path)
          report_error_once("review-record:" .. path, item.code .. ": " .. item.message)
        end
      end
      return valid
    end

    local function write_v2_user_review(read, dir)
      local owner_id = "user"
      local owner_key = sha256(owner_id)
      local path = v2_pane_root(dir, read.address) .. "/reviews/" .. owner_key .. ".json"
      return write_v2_record(path, {
        kind = "review",
        schema = protocol.record_schema,
        address = read.address,
        owner_id = owner_id,
        owner_key = owner_key,
        event_id = next_v2_event_id(),
      }, "review", { address = read.address })
    end

    local function clear_v2_reviews(read, dir)
      local cleared = false
      for _, path in ipairs(v2_review_paths(read, dir)) do
        local removed, remove_err = os.remove(path)
        if removed then
          cleared = true
        else
          report_error_once("clear-v2-review:" .. path,
            "failed to remove review claim " .. path .. ": " .. tostring(remove_err))
        end
      end
      return cleared
    end

    local function refresh_cached_v2(read, dir, now_unix_ns)
      local current_now = now_unix_ns
      if not current_now then current_now = wezterm_now_unix_ns20() end
      local view = read_attention_view(read, current_now, {
        dir = dir, previous_view = attention_cache[read.cache_key],
      })
      attention_cache[read.cache_key] = view
      legacy_cache_key_by_marker_id[read.marker_id] = read.cache_key
      return view
    end

    local function acknowledge_focused_v2_pane(read, opts)
      local dir = (opts and opts.dir) or M._active_dir or defaults.dir
      local acknowledge_set = M._active_acknowledge_set or { stop = true, notify = true }
      local view = read_attention_view(read, opts and opts.now_unix_ns, { dir = dir })
      if not (view.activity_type and acknowledge_set[view.activity_type] and view.event_id) then
        attention_cache[read.cache_key] = view
        return "absent"
      end
      local records_root, target = selected_v2_records_root(dir, read, view.binding_id)
      local path = records_root .. "/ack.json"
      local record = {
        kind = "acknowledgement",
        schema = protocol.record_schema,
        address = read.address,
        launch_id = read.launch_id,
        target = target,
        activity_event_id = view.event_id,
        event_id = next_v2_event_id(),
      }
      if not write_v2_record(path, record, "acknowledgement", {
        address = read.address, launch_id = read.launch_id,
      }) then
        attention_cache[read.cache_key] = view
        return "failed"
      end
      refresh_cached_v2(read, dir, opts and opts.now_unix_ns)
      return "acknowledged"
    end


    return {
      selected_v2_records_root = selected_v2_records_root,
      v2_review_paths = v2_review_paths,
      write_v2_user_review = write_v2_user_review,
      clear_v2_reviews = clear_v2_reviews,
      refresh_cached_v2 = refresh_cached_v2,
      acknowledge_focused_v2_pane = acknowledge_focused_v2_pane,
    }
  end

  return {
    bind_v2 = bind_v2,
    reported_errors = reported_errors,
    publication_session = publication_session,
    report_error_once = report_error_once,
    next_publication_id = next_publication_id,
    json_string = json_string,
    json_value = json_value,
    next_v2_event_id = next_v2_event_id,
    write_v2_record = write_v2_record,
    acknowledgement_path = acknowledgement_path,
    acknowledgement_tmp_path = acknowledgement_tmp_path,
    marker_identity = marker_identity,
    read_acknowledgement = read_acknowledgement,
    clear_acknowledgement = clear_acknowledgement,
    write_acknowledgement = write_acknowledgement,
    acknowledgement_matches = acknowledgement_matches,
    read_effective_marker = read_effective_marker,
    review_path = review_path,
    review_tmp_path = review_tmp_path,
    review_flagged = review_flagged,
    write_review_flag = write_review_flag,
    clear_review_flag = clear_review_flag,
    remove_expired_marker = remove_expired_marker,
    remove_marker = remove_marker,
  }
end
