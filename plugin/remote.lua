-- ── Another machine's records ───────────────────────────────────────────────
-- A domain whose mux runs on another machine, as one reached through an ssh
-- proxy_command does, has its records in that machine's state directory, not
-- in this one's. For a domain named in remote_domains, the plugin runs one
-- shell command line on that machine, the way ssh runs one, which prints the
-- records of the panes this GUI shows from that mux, keeps them in memory and
-- reads them with the same reader as local records. No record is written on
-- either machine.

return function(context)
  local wezterm = context.wezterm
  local protocol_api = context.protocol_api
  local report_error_once = context.report_error_once
  local report_warning_once = context.report_warning_once
  local now_ms = protocol_api.now_ms

  --- How long after one fetch starts the next may start. A fetch over ssh
  --- takes about as long, mostly in the key exchange.
  local fetch_interval_ms = 3000
  --- A fetch still running after this long is given up on, so one ssh that
  --- hangs cannot stop the reads for good.
  local fetch_abandon_ms = 60000
  --- A pane wanted by no poll for this long is no longer fetched.
  local wanted_for_ms = 10000
  --- Records read longer ago than this are no longer shown: nothing at all
  --- is better than a state that may have ended.
  local stale_after_ms = 30000
  --- The most a fetch may print. A record is at most 1 MiB, and a fetch reads
  --- a few small records for each pane it names.
  local max_output_bytes = 16 * 1024 * 1024

  local entries = {}
  local states = {}

  local function quote(value)
    return "'" .. value:gsub("'", "'\\''") .. "'"
  end

  --- A path on the other machine, as a word for its shell: "~/" stands for
  --- that machine's home directory. An absent value is the empty word.
  local function remote_path(value)
    if value == nil then return "''" end
    if value:sub(1, 2) == "~/" then return '"$HOME"/' .. quote(value:sub(3)) end
    return quote(value)
  end

  local function is_remote_path(value)
    return type(value) == "string" and not value:find("%z")
      and (value:sub(1, 1) == "/" or value:sub(1, 2) == "~/")
  end

  --- Prints, for the realm and incarnation it is given and each pane and
  --- launch after them, the records the reader reads for that launch, framed
  --- as "@<kind> <path>\n<bytes>" and a record separator (octal 036), which no
  --- JSON text holds; paths are relative to the state directory. A record it
  --- does not print is not there, or awk could not open it. First comes what
  --- `attention tab-source` says of the socket, which names the mux's realm
  --- and incarnation; with "-" for the realm that is all it prints. The
  --- binding is the one the launch's current-binding record names, and the
  --- launch's own activity and acknowledgement are read only when it names
  --- none, as the reader does. One awk reads every record, since a process
  --- per record cost most of a second for thirty panes. Run by /bin/sh, so
  --- the login shell on the other side (zsh, bash) only passes it on.
  --- Positional parameters: socket, state directory (empty: that machine's
  --- own default), attention command (empty: `attention` on PATH), realm,
  --- incarnation, then pane and launch pairs.
  local fetch_script = [==[
socket=$1 dir=$2 attention=${3:-attention} realm=$4 incarnation=$5
shift 5
if [ -z "$dir" ]; then
  if [ -n "${WEZTERM_ATTENTION_DIR:-}" ]; then
    dir=$WEZTERM_ATTENTION_DIR
  else
    case ${XDG_STATE_HOME:-} in
      /*) dir=${XDG_STATE_HOME%/}/wezterm-attention ;;
      *) dir=$HOME/.local/state/wezterm-attention ;;
    esac
  fi
fi
printf '@source\n'
WEZTERM_ATTENTION_DIR=$dir "$attention" tab-source --socket "$socket" 2>/dev/null
printf '\036'
[ "$realm" = - ] && exit 0
root=v2/realms/$realm/incarnations/$incarnation
reviews=
pane=
for word in "$@"; do
  if [ -z "$pane" ]; then pane=$word; continue; fi
  names=
  for review in "$dir/$root/panes/$pane/reviews/"*.json; do
    [ -e "$review" ] || continue
    names="$names${review##*/}
"
    reviews="$reviews$root/panes/$pane/reviews/${review##*/}
"
  done
  printf '@list %s\n%s\036' "$root/panes/$pane/reviews" "$names"
  pane=
done
FETCH_DIR=$dir FETCH_ROOT=$root FETCH_REALM=$realm FETCH_REVIEWS=$reviews exec awk '
function emit(path,    line, body, count) {
  body = ""
  count = 0
  while ((getline line < (dir "/" path)) > 0) body = body (count++ ? "\n" : "") line
  close(dir "/" path)
  if (count == 0) return ""
  printf "@file %s\n%s\036", path, body
  return body
}
BEGIN {
  dir = ENVIRON["FETCH_DIR"]
  root = ENVIRON["FETCH_ROOT"]
  emit("v2/realms/" ENVIRON["FETCH_REALM"] "/realm.json")
  emit(root "/incarnation.json")
  count = split(ENVIRON["FETCH_REVIEWS"], reviews, "\n")
  for (i = 1; i <= count; i++) if (reviews[i] != "") emit(reviews[i])
  split("binding end activity-clear activity ack children", names, " ")
  for (i = 1; i + 1 < ARGC; i += 2) {
    pane = root "/panes/" ARGV[i]
    launch = pane "/launches/" ARGV[i + 1]
    printf "@launch %s\036", launch
    emit(pane "/claim.json")
    emit(pane "/mute.json")
    pointer = emit(launch "/current-binding.json")
    if (match(pointer, /"binding_id"[ \t\r\n]*:[ \t\r\n]*"[0-9a-f]+"/)) {
      binding = substr(pointer, RSTART, RLENGTH)
      sub(/^"binding_id"[ \t\r\n]*:[ \t\r\n]*"/, "", binding)
      sub(/"$/, "", binding)
      if (length(binding) == 64) {
        for (n = 1; n <= 6; n++) emit(launch "/bindings/" binding "/" names[n] ".json")
      }
    } else {
      emit(launch "/activity.json")
      emit(launch "/ack.json")
    }
  }
}' "$@"
]==]

  --- Republishes every pane of the mux behind the socket, as that machine's
  --- own GUI does for panes it cannot name. Positional parameters: socket,
  --- state directory, attention command, as for the fetch.
  local publish_script = [[
socket=$1 dir=$2 attention=${3:-attention}
if [ -n "$dir" ]; then WEZTERM_ATTENTION_DIR=$dir; export WEZTERM_ATTENTION_DIR; fi
exec "$attention" hooks publish --socket "$socket" --quiet
]]

  --- The argv that runs `script` on the entry's machine with `arguments`,
  --- each already a word for that machine's shell.
  local function remote_argv(entry, script, arguments)
    local words = { "exec", "/bin/sh", "-c", quote(script), "wezterm-attention",
      remote_path(entry.socket), remote_path(entry.dir), remote_path(entry.attention) }
    for _, argument in ipairs(arguments or {}) do words[#words + 1] = argument end
    local argv = protocol_api.deep_copy(entry.command)
    argv[#argv + 1] = table.concat(words, " ")
    return argv
  end

  --- Where the domain's records are read from: a root no state directory
  --- can be, since those are absolute paths.
  local function mount_root(domain)
    return "remote:" .. domain
  end

  local function state_of(domain)
    local state = states[domain]
    if not state then
      state = { wanted = {}, token = 0 }
      states[domain] = state
    end
    return state
  end

  local function tree_for(domain)
    return {
      read = function(relative)
        local snapshot = state_of(domain).snapshot
        if not snapshot then return nil, "not read from " .. domain .. " yet" end
        local content = snapshot.files[relative]
        if content then return content end
        -- The fetch prints every record it finds of the realm, of the
        -- incarnation, and of each pane it read; one it did not print is not
        -- there. A record it never prints, such as a lifecycle snapshot, reads
        -- as absent the same way.
        local pane = relative:match("^(.-/panes/[^/]+)/")
        if not pane or snapshot.panes[pane] then return nil, "No such file or directory" end
        return nil, "not read from " .. domain
      end,
      list = function(directory)
        local snapshot = state_of(domain).snapshot
        return snapshot and snapshot.lists[directory] or nil
      end,
    }
  end

  local entry_options = { attention = true, command = true, dir = true, socket = true }

  --- Keep the usable entries of the remote_domains option, naming each
  --- problem once in the log, and read records through them from now on.
  local function configure(option)
    entries = {}
    states = {}
    for domain, entry in pairs(option or {}) do
      local problem
      if type(domain) ~= "string" or domain == "" then
        problem = "a domain name must be a non-empty string"
      elseif type(entry) ~= "table" then
        problem = "must be a table"
      elseif type(entry.command) ~= "table" or #entry.command == 0 then
        problem = "command must be a non-empty list of strings, the argv that runs one shell command line there"
      elseif not is_remote_path(entry.socket) then
        problem = "socket must be the mux socket's path on that machine, absolute or starting with ~/"
      elseif entry.dir ~= nil and not is_remote_path(entry.dir) then
        problem = "dir must be absolute or start with ~/"
      elseif entry.attention ~= nil and (type(entry.attention) ~= "string" or entry.attention == ""
          or entry.attention:find("%z")) then
        problem = "attention must be a non-empty string"
      else
        for index, word in pairs(entry.command) do
          if type(index) ~= "number" or type(word) ~= "string" or word == "" or word:find("%z") then
            problem = "command must be a non-empty list of strings"
          end
        end
      end
      if problem then
        report_warning_once("option:remote_domains:" .. tostring(domain), "option remote_domains["
          .. tostring(domain) .. "] " .. problem .. ", so that domain's panes show no attention")
      else
        for key in pairs(entry) do
          if not entry_options[key] then
            report_warning_once("option:remote_domains:" .. domain .. "." .. tostring(key),
              "unknown option remote_domains[" .. domain .. "]." .. tostring(key)
              .. " is ignored; the options in remote_domains[" .. domain
              .. "] are attention, command, dir and socket")
          end
        end
        entries[domain] = entry
        protocol_api.mount(mount_root(domain), tree_for(domain))
      end
    end
  end

  --- The remote_domains entry of the domain, or nil.
  local function entry(domain)
    return entries[domain]
  end

  --- Does `address` name a pane of the mux behind the domain? nil when the
  --- domain is not named in remote_domains; "unknown" until the first fetch
  --- says which mux that is; else "own" or "other".
  local function verdict(domain, address)
    if not entries[domain] then return nil end
    local identity = state_of(domain).identity
    if not identity then return "unknown" end
    if address.realm_id == identity.realm_id and address.incarnation_id == identity.incarnation_id then
      return "own"
    end
    return "other"
  end

  --- Ask the next fetch for the launch `read` names, and say whether its
  --- records can be shown: "covered" when the latest fetch read it and is
  --- recent, "pending" when no fetch has read it yet, "stale" when the
  --- latest records are too old to show.
  local function coverage(domain, read)
    local state = state_of(domain)
    local at = now_ms()
    state.wanted[read.address.pane_id] = { launch_id = read.launch_id, at = at }
    local snapshot = state.snapshot
    if not snapshot then return "pending" end
    if at - snapshot.taken_ms > stale_after_ms then return "stale" end
    local launch = table.concat({ "v2/realms", read.address.realm_id, "incarnations",
      read.address.incarnation_id, "panes", read.address.pane_id, "launches", read.launch_id }, "/")
    if snapshot.launches[launch] then return "covered" end
    return "pending"
  end

  --- Split what a fetch printed into its chunks at each record separator;
  --- nil when it is not the framing the script writes.
  local function chunks(text)
    local list, position = {}, 1
    while position <= #text do
      local finish = text:find("\30", position, true)
      if not finish then return nil end
      list[#list + 1] = text:sub(position, finish - 1)
      position = finish + 1
    end
    return list
  end

  --- What a fetch printed: the mux identity it names and, when it read the
  --- records of `asked` (the identity the fetch named) and that is still the
  --- mux there, the snapshot of them; nil and why when it cannot be used.
  local function parse(text, asked, parse_tab_source)
    if type(text) ~= "string" or #text > max_output_bytes then return nil, "output is missing or too large" end
    local list = chunks(text)
    if not list or #list == 0 then return nil, "output is not what the fetch prints" end
    local source_text = list[1]:match("^@source\n(.*)$")
    local identity = source_text and parse_tab_source(source_text)
    if not identity then return nil, "attention tab-source gave no usable answer there" end
    -- Records read for a mux that was replaced since the panes were named
    -- are not theirs.
    if not asked or identity.realm_id ~= asked.realm_id
        or identity.incarnation_id ~= asked.incarnation_id then
      return identity
    end
    local prefix = "v2/realms/" .. asked.realm_id .. "/"
    local snapshot = { files = {}, lists = {}, launches = {}, panes = {} }
    for index = 2, #list do
      local header, body = list[index]:match("^([^\n]*)\n(.*)$")
      header = header or list[index]
      local kind, path = header:match("^@(%a+) (.+)$")
      if not kind or path:sub(1, #prefix) ~= prefix or path:find("..", 1, true) then
        return nil, "output names a record it was not asked for"
      end
      if kind == "file" then
        snapshot.files[path] = body or ""
      elseif kind == "list" then
        local names = {}
        for name in (body or ""):gmatch("[^\n]+") do names[#names + 1] = name end
        snapshot.lists[path] = names
      elseif kind == "launch" then
        snapshot.launches[path] = true
        snapshot.panes[path:match("^(.*)/launches/[^/]+$") or path] = true
      else
        return nil, "output has a chunk of unknown kind " .. kind
      end
    end
    return identity, nil, snapshot
  end

  --- The fetch's arguments: the realm and incarnation, then each wanted
  --- pane and launch, checked to be the digits and UUID the wire format
  --- allows before they go on a command line.
  local function fetch_arguments(state, at)
    local identity = state.identity
    if not identity then return { "-", "-" } end
    local arguments = { identity.realm_id, identity.incarnation_id }
    local panes = {}
    for pane_id, wanted in pairs(state.wanted) do
      if at - wanted.at > wanted_for_ms then
        state.wanted[pane_id] = nil
      elseif pane_id:match("^%d+$") and protocol_api.is_uuid(wanted.launch_id) then
        panes[#panes + 1] = pane_id
      end
    end
    table.sort(panes, function(a, b) return tonumber(a) < tonumber(b) end)
    for _, pane_id in ipairs(panes) do
      arguments[#arguments + 1] = pane_id
      arguments[#arguments + 1] = state.wanted[pane_id].launch_id
    end
    return arguments
  end

  --- Run one fetch for the domain and keep what it read.
  local function fetch(domain, state, token, argv, asked, parse_tab_source)
    local ok, success, stdout = pcall(wezterm.run_child_process, argv)
    if state ~= states[domain] or state.token ~= token then return end
    state.running_since = nil
    local identity, problem, snapshot
    if ok and success then
      identity, problem, snapshot = parse(stdout, asked, parse_tab_source)
    else
      problem = ok and "the command failed" or ("the command could not start: " .. tostring(success))
    end
    if not identity then
      report_error_once("remote-fetch:" .. domain .. ":" .. problem, "cannot read the records of domain "
        .. domain .. " on its machine: " .. problem .. "; its panes keep what was last read for "
        .. math.floor(stale_after_ms / 1000) .. " seconds")
      return
    end
    state.identity = identity
    if snapshot then
      snapshot.taken_ms = now_ms()
      state.snapshot = snapshot
    end
  end

  --- Start a fetch for each domain a poll showed, when one is due, through
  --- `call_after`, so the ssh runs in a callback of its own and no poll or
  --- tab bar draw waits on it.
  local function request(domains, call_after, parse_tab_source)
    if type(call_after) ~= "function" or type(wezterm.run_child_process) ~= "function" then
      report_error_once("remote-fetch-unavailable", "cannot read the records of a domain on another "
        .. "machine: wezterm.time.call_after or wezterm.run_child_process is unavailable")
      return
    end
    for domain in pairs(domains) do
      local state = state_of(domain)
      local at = now_ms()
      local running = state.running_since and at - state.running_since < fetch_abandon_ms
      if not running and (not state.started_ms or at - state.started_ms >= fetch_interval_ms) then
        state.token = state.token + 1
        state.started_ms, state.running_since = at, at
        local token, asked = state.token, state.identity
        local argv = remote_argv(entries[domain], fetch_script, fetch_arguments(state, at))
        call_after(0, function() fetch(domain, state, token, argv, asked, parse_tab_source) end)
      end
    end
  end

  --- The argv that republishes the identity of every pane of the domain's
  --- mux on its own machine.
  local function publish_argv(domain)
    return remote_argv(entries[domain], publish_script)
  end

  return {
    configure = configure,
    entry = entry,
    mount_root = mount_root,
    verdict = verdict,
    coverage = coverage,
    request = request,
    publish_argv = publish_argv,
  }
end
