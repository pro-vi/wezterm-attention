return function(context)
  local M = context.M
  local defaults = context.defaults
  local report_error_once = context.report_error_once
  local is_safe_text = context.is_safe_text
  local drawn_pane_key = context.drawn_pane_key
  local settled_title_state = {}

  --- The second byte each UTF-8 lead byte allows, where it is narrower than
  --- any continuation byte: these ranges refuse overlong forms, surrogates and
  --- code points past U+10FFFF.
  local second_byte_range = {
    [0xE0] = { 0xA0, 0xBF }, [0xED] = { 0x80, 0x9F },
    [0xF0] = { 0x90, 0xBF }, [0xF4] = { 0x80, 0x8F },
  }

  --- `text` with every ill-formed UTF-8 sequence replaced by U+FFFD, one per
  --- maximal ill-formed part, as Rust's `String::from_utf8_lossy` does. A
  --- formatter can return bytes cut inside a character, and a reader that
  --- decodes the published file as JSON refuses the whole file over them.
  local function well_formed_utf8(text)
    local start = text:find("[\128-\255]")
    if not start then return text end
    local parts, from, length = {}, 1, #text
    while start do
      local lead = text:byte(start)
      local size = lead >= 0xC2 and lead <= 0xDF and 2
        or lead >= 0xE0 and lead <= 0xEF and 3
        or lead >= 0xF0 and lead <= 0xF4 and 4
        or 1
      local range = second_byte_range[lead]
      local good = 0
      if size > 1 then
        good = 1
        for offset = 1, size - 1 do
          local byte = text:byte(start + offset)
          local low, high = 0x80, 0xBF
          if offset == 1 and range then low, high = range[1], range[2] end
          if not byte or byte < low or byte > high then break end
          good = good + 1
        end
      end
      if good == size then
        start = text:find("[\128-\255]", start + size)
      else
        parts[#parts + 1] = text:sub(from, start - 1)
        parts[#parts + 1] = "\239\191\189"
        from = start + math.max(good, 1)
        start = text:find("[\128-\255]", from)
      end
    end
    if from == 1 then return text end
    parts[#parts + 1] = text:sub(from, length)
    return table.concat(parts)
  end

  -- What follows an ESC byte, or the C1 character that stands for ESC and
  -- that byte, to open a sequence with a body. That C1 character is 0xC2 and
  -- then the byte plus 0x40. Anything else after an ESC is a short sequence:
  -- intermediate bytes, then one final byte.
  local after_esc = { [0x5B] = "csi", [0x5D] = "osc", [0x50] = "string", [0x58] = "string",
    [0x5E] = "string", [0x5F] = "string" }

  --- Where the sequence of `kind` whose body starts at `body` ends: the index
  --- just past it, or past the text when it is never closed. A string runs to
  --- ST (ESC \ or U+009C), an OSC to BEL as well; an ESC that is not ST ends
  --- it and opens the next sequence.
  local function sequence_end(text, kind, body)
    local index = body
    if kind == "csi" or kind == "short" then
      local low = kind == "csi" and 0x3F or 0x2F
      local byte = text:byte(index)
      while byte and byte >= 0x20 and byte <= low do
        index = index + 1
        byte = text:byte(index)
      end
      local first_final = kind == "csi" and 0x40 or 0x30
      if byte and byte >= first_final and byte <= 0x7E then index = index + 1 end
      return index
    end
    while true do
      local stop = text:find("[\7\27\194]", index)
      if not stop then return #text + 1 end
      local byte, next_byte = text:byte(stop), text:byte(stop + 1)
      if byte == 7 and kind == "osc" then return stop + 1 end
      if byte == 27 then return next_byte == 0x5C and stop + 2 or stop end
      if byte == 194 and next_byte == 0x9C then return stop + 2 end
      index = stop + 1
    end
  end

  --- Remove every escape sequence whole, in one pass over well-formed text.
  --- Removing only the ESC would leave the rest of the sequence to be drawn.
  local function without_escape_sequences(text)
    local start = text:find("[\27\194]")
    if not start then return text end
    local parts, from = {}, 1
    while start do
      local byte, next_byte = text:byte(start), text:byte(start + 1)
      local kind, body
      if byte == 27 then
        kind = next_byte and after_esc[next_byte]
        if kind then body = start + 2
        elseif next_byte and next_byte >= 0x20 and next_byte <= 0x7E then kind, body = "short", start + 1 end
      else
        kind = next_byte and after_esc[next_byte - 0x40]
        body = start + 2
      end
      if kind then
        parts[#parts + 1] = text:sub(from, start - 1)
        from = sequence_end(text, kind, body)
        start = text:find("[\27\194]", from)
      else
        start = text:find("[\27\194]", start + 1)
      end
    end
    parts[#parts + 1] = text:sub(from)
    return table.concat(parts)
  end

  --- Text from a source the plugin does not control -- a directory name a
  --- program chose through OSC 7, a title, a tab name any process in any pane
  --- can set, a formatter's return -- made safe to draw and to publish:
  --- ill-formed UTF-8 replaced, escape sequences removed whole, every other
  --- control character removed, C0, DEL and C1 alike, then cut to `max_bytes`
  --- on a character boundary. WezTerm applies escape sequences in a
  --- formatter's text, so one left in would restyle the bar; style comes
  --- from the plugin's colors instead.
  local function display_text(value, max_bytes)
    if type(value) ~= "string" then return nil end
    -- Well-formed first, so that removing a control character cannot join
    -- two stray bytes into a C1 control. In well-formed text every removal
    -- takes whole characters, so one pass leaves none behind.
    local text = without_escape_sequences(well_formed_utf8(value)):gsub("[%z\1-\31\127]", "")
    text = text:gsub("\194[\128-\159]", "")
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
    local key = local_id and drawn_pane_key(local_id)
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

  return {
    well_formed_utf8 = well_formed_utf8,
    display_text = display_text,
    normalized_pane_title = normalized_pane_title,
    sample_settled_title = sample_settled_title,
    settled_title_state = settled_title_state,
    settled_title_for_tab = settled_title_for_tab,
    title_sources = title_sources,
    default_title = default_title,
  }
end
