# Architecture diagrams

## Where the axes live

```mermaid
flowchart TB
  subgraph role["Role profile — chosen by the lead, per role"]
    RP["platform + model + extra argv"]
  end
  subgraph sub["Substrate axis — SessionRuntime (unchanged)"]
    H["HerdrSessionRuntime<br/>start / query / prompt / interrupt / wait"]
    Z["ZeroCostDummy (tests)"]
  end
  subgraph plat["Platform axis — SessionAdapter (new)"]
    C["ClaudeCode<br/>(today's logs.rs)"]
    X["Codex"]
  end
  RP -->|launch argv + herdr kind| H
  RP -->|selects| C
  RP -->|selects| X
  H -->|external session id| DB[(sessions row<br/>platform, model)]
  DB -->|platform| C
  DB -->|platform| X
  C -->|context, commits,<br/>prompt landing, mentions| D["daemon observation loop"]
  X -->|same| D
```

The launch flag flows left to right once; the persisted `platform` column is what lets the daemon re-enter the adapter on every later poll.

## Today vs after

```mermaid
flowchart LR
  subgraph now["Today"]
    S1["Store::open"] --> L1["logs_dir =<br/>~/.claude/projects/&lt;munged cwd&gt;"]
    L1 --> DB1[("chainsaw-supervisor.db<br/>inside the transcript dir")]
    L1 --> T1["session_log = logs_dir/&lt;id&gt;.jsonl"]
    T1 --> P1["logs.rs parses<br/>Claude Code JSONL"]
  end
  subgraph after["After"]
    S2["Store::open"] --> R2["chainsaw-owned state root"]
    R2 --> DB2[("chainsaw-supervisor.db")]
    S2 --> A2["adapter.transcript(session)"]
    A2 --> T2a["~/.claude/projects/…/&lt;id&gt;.jsonl"]
    A2 --> T2b["~/.codex/sessions/YYYY/MM/DD/<br/>rollout-*-&lt;uuid&gt;.jsonl"]
    T2a --> P2["adapter.context / commits /<br/>prompt_landed / mentions"]
    T2b --> P2
  end
```

The database moving out of the transcript directory is the change that unblocks everything else: there is no per-project Codex directory to put it in.

## A heterogeneous run

```mermaid
sequenceDiagram
  participant Human
  participant Lead as Lead (claude, fable)
  participant Sup as chainsaw
  participant Herdr
  participant Impl as Implementer (codex)
  participant Comm as Commentator (claude, opus)

  Human->>Lead: start session, invoke skill
  Lead->>Sup: daemon --lead … --platform claude
  Lead->>Sup: start-commentator --platform claude --model opus
  Lead->>Sup: launch worker-1 --platform codex --model gpt-5-codex
  Sup->>Herdr: agent start --kind codex -- -m gpt-5-codex …
  Herdr-->>Sup: external session id
  Sup->>Sup: persist platform=codex on the session row
  Lead->>Sup: dispatch task → prompt worker-1
  loop every poll
    Sup->>Impl: read rollout via Codex adapter
    Impl-->>Sup: commit sha, context, prompt landing
  end
  Sup->>Comm: commit <sha> landed for task N; review it from git
  Comm->>Sup: finding / observe / verdict
```

The commentator is Claude while the transcript it reviews is Codex — the case the adapter must make ordinary, by handing it resolved paths and naming the format rather than pointing at a directory.
