# ADR 0008: The lead's latest prompt and Codex question answers are kept beside the lifecycle pools

- **Status:** Accepted
- **Date:** 2026-10-11

## Context

A lead's `prompt_submitted` observations share the `general` pool of
`lifecycle.json` with every generic tool preflight and result, and that pool
keeps the 64 newest. A busy Codex lead filled it in well under an hour; one
confirmed prompt was gone from `inspect` 26 minutes after it was written. Two
consumers decide whether a nonblocking question still waits by whether a
prompt came after it, and both lost that boundary.

Codex 0.162.0 also submits a person's answer to a `request_user_input_async`
question as an ordinary prompt: a `<send_user_message_question_reply>` wrapper,
after IDE context when there is any, whose `questionItemId` names the question
call's id. Attention recorded
that prompt like any other and related nothing to the question, and the prompt
itself was lost to the same eviction.

## Decision

A binding keeps a third file, `prompts.json` (kind `lead_prompts`), beside
`lifecycle.json` and `children-lifecycle.json`. It holds the lead's latest
prompt (observation id and both times) and up to 64 answers, each a question's
call id and index with the observation id of the prompt that answered it. The
lifecycle writer updates it under the same locks for every lead prompt, moves
the latest prompt only forward, and takes answers only from a lead Codex
prompt that is the wrapper; the prompt's text never enters it. It does so
whether or not `lifecycle.json` stored that prompt, because Codex gives every
prompt of a turn the turn's id and the general pool keeps one prompt per id,
so a prompt typed while the turn runs is never stored there. A question keeps
its first answer, so a hook delivered twice adds nothing.

Both readers add `latest_prompt` to the lifecycle facet, the later of the
record's and the newest lead prompt the pools still hold, and give every
question group `answer_observation_ids`, the record's answers whose call id
equals the group's. Matching ignores the turn: an answer can arrive in its
question's turn or a later one. A record that cannot be used adds one
diagnostic and nothing else.

`lifecycle.json`, its observation union and every schema number stay as they
were. The manifest gains the record and two shapes.

## Alternatives

- A new observation kind for answers, or a new field on `prompt_submitted`, in
  `lifecycle.json`: it changes the `lifecycle_snapshot` shape. A plugin still
  loaded in a running GUI would show the lifecycle facet `invalid`, and an
  older command writing to that binding moves the file aside and starts an
  empty one. [ADR 0003](0003-actors-are-separate-eviction-domains.md) rejected
  four pools in `lifecycle.json` for the same reason.
- Keeping the newest prompt in the `general` pool past its floor: every reader
  rejects a member at or below its pool's floor, so the floor would change
  meaning.
- Moving prompts to the `requests` pool: prompts are frequent, and they would
  evict the question evidence they bound.
- The same protection for the newest turn end: `activity.json` already keeps
  the latest turn end, and the newest lead observation is never the one
  evicted, so the last time the lead was recorded is the later of that
  observation and `latest_prompt`.

## Consequences

A poll reads one more file per bound pane, at most about 11 KB with 64 Codex
answers, beside a `lifecycle.json` of up to 262 KB; on an M5 Max, reading and
comparing it took about 9 µs per pane, measured outside WezTerm. A lead prompt
hook writes one more file, about 0.2 ms. The latest prompt's or an answer's
observation id can name a prompt no pool shows, when it came while its turn
ran. An answer says a
person answered the call, not that nothing on it waits: a call with several questions
can be partly answered, and Attention does not record how many a call asked. A
typed prompt that is a copy of the wrapper is read as an answer. An older
command's `sweep` and `doctor` keep a binding that holds `prompts.json`, as
they keep any binding with a file they do not know, and report it as
`record_invalid` ("unknown binding state is preserved"), until a newer command
runs.

## Revisit

When Codex changes the wrapper or the id inside it, the answers stop; the
consumer guide names the version the format was read from. When a consumer
needs an older answer than the newest 64, or the latest turn end beside the
latest prompt.
