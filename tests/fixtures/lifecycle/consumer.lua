-- Example consumer policy. It selects a symbol, not a WezTerm color API.
-- Nothing here writes Attention records or modifies the supplied view.
local M = {}

local function scope(view)
  if not view or type(view.address) ~= "table" then return nil end
  local values = { view.address.realm_id, view.address.incarnation_id, view.address.pane_id, view.launch_id, view.binding_id }
  for index = 1, 5 do if type(values[index]) ~= "string" then return nil end end
  return table.concat(values, "\0")
end

function M.new()
  local selected_scope, dismissed, previous, displayed = nil, {}, {}, {}
  local evidence_lost = false
  local consumer = {}

  function consumer.appearance(view)
    local next_scope = scope(view)
    if next_scope and next_scope ~= selected_scope then
      selected_scope, dismissed, previous, displayed, evidence_lost = next_scope, {}, {}, {}, false
    end
    displayed = {}
    local lifecycle = view and view.lifecycle
    if not next_scope or view.binding_phase ~= "active" or view.reader_confidence ~= "confirmed"
      or view.binding_health ~= "valid" or view.pane_presence ~= "present"
      or not lifecycle or lifecycle.availability ~= "available" then return "unknown" end

    local present = {}
    for _, request in ipairs(lifecycle.requests) do
      if request.kind == "question" and request.question_mode == "nonblocking" then
        for _, id in ipairs(request.publication_observation_ids) do present[id] = true end
      end
    end
    for id in pairs(previous) do if not present[id] then evidence_lost = true end end
    for id in pairs(dismissed) do if not present[id] then dismissed[id] = nil end end
    previous = present
    local attention_needed = false
    for id in pairs(present) do
      if not dismissed[id] then displayed[id], attention_needed = true, true end
    end
    if attention_needed then return "follow_up" end
    if evidence_lost or lifecycle.retention_floors.requests then return "unknown" end
    -- Base appearance is a display choice, not proof that nothing is unanswered.
    return "base"
  end

  function consumer.dismiss()
    -- Only IDs from the last displayed view: a newly arrived Q2 is not dismissed
    -- by a click that the consumer rendered for Q1.
    for id in pairs(displayed) do dismissed[id] = true end
    displayed = {}
  end

  return consumer
end

return M
