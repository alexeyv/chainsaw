# Platform matrix

What varies between agent platforms, and the evidence behind each row. Gathered from Claude Code as chainsaw uses it today and from Codex CLI 0.150.1 installed locally, reading a real rollout file — not from documentation.

## The four axes

| Axis | Members | Owner in code |
|---|---|---|
| Substrate | herdr, tmux, orca, subagents | `SessionRuntime` (unchanged) |
| Platform | claude, codex | new session adapter |
| Model | fable, opus, grok-4.6, … | field on the role profile, passed to the adapter |
| Role | lead, commentator, implementer | picks the platform+model pair |

Substrate constrains platform rather than being independent of it: a multiplexer substrate lets every pane run a different platform, while a subagent substrate pins all sessions to the host's own platform. That is the whole reason the adapter axis is worth building, and the reason herdr is the substrate that matters.

## Claude Code vs Codex

| | Claude Code | Codex 0.150.1 |
|---|---|---|
| herdr kind | `claude` | `codex` (already supported) |
| Skills | `SKILL.md` | `SKILL.md`, loaded natively |
| Transcript path | `~/.claude/projects/<cwd with / and . → ->/<session-id>.jsonl` | `~/.codex/sessions/YYYY/MM/DD/rollout-<ISO>-<uuid>.jsonl` |
| Partitioned by | project | **date, across all projects** |
| Per-run directory | yes — chainsaw uses it as `logs_dir` | **none** |
| Turn entry | `type: "assistant"`, `message.content` | `type: "response_item"`, `payload.type` ∈ {message, agent_message, reasoning, custom_tool_call}, `content[].type: "input_text"` |
| Token accounting | `message.usage.{input,cache_read,cache_creation}_tokens`, summed | `event_msg`/`token_count` → `info.total_token_usage.total_tokens` |
| Context window | not stated; thresholds hardcoded | `info.model_context_window` stated per turn |
| Compaction | supervisor pushes `/compact` | automatic; no command to send |
| Tool narrowing | `--disallowedTools` | **none**; `--sandbox`, `-a never`, `-c` overrides only |
| Web search | on by default, disabled by flag | off by default, `--search` opts in |
| Model flag | `--model` | `-m/--model` |
| Session index carries cwd | n/a (path encodes it) | no — `~/.codex/session_index.jsonl` is `{id, thread_name, updated_at}`; cwd lives in the rollout's `session_meta` |

Codex's token telemetry is **better** than Claude Code's: cumulative totals and the context window are both stated outright, so thresholds against Codex can be a fraction of the window instead of the hardcoded constants used today.

Two Codex details that cut the other way: an `agent_message` payload can carry `encrypted_content`, so text scraping is not guaranteed complete; and because rollouts are date-partitioned, resolving a session id to a file means globbing `rollout-*-<uuid>.jsonl` across date directories rather than joining a path.

## Why subagents are not a substrate

Ruled out, not deferred. Three independent reasons:

1. **Platform is pinned.** Claude subagents are Claude; Codex subagents are Codex. Model can still vary — Claude's Agent tool takes a per-agent model — but the platform cannot, which is exactly the freedom this spec exists to buy.
2. **No per-session transcript.** Subagent turns land as `isSidechain: true` entries inside the parent's file, which `logs.rs` deliberately filters out so they do not corrupt the lead's context math. The commentator's method — read the implementer's transcript after the session is discarded — has no file to read.
3. **No pane.** Chainsaw's premise is that every role is a visible interactive session a human can watch and take over. Subagents are invisible by construction.

Supporting them would mean replacing transcript scraping with in-process events, which is a different product.

## Grok, for the record

Investigated and dropped from scope at the operator's direction. Worth recording because it constrains the trait's shape: Grok persists sessions at `~/.grok/sessions/<percent-encoded-cwd>/<uuid>/chat_history.jsonl` in a clean `{type: user|assistant|reasoning|tool_result}` format, and its `--deny` flag is documented as a compat alias for `--disallowedTools` — but it records **no token accounting anywhere**. Across an 11,000-event session the only token-shaped name is `first_token`, an event rather than a count.

That is why the adapter's context call must be allowed to return "unavailable" rather than a number. Under Grok, calibration would go dark while commit detection and prompt landing kept working — a degradation the design should permit even though no Grok adapter is being built.
