# slag

> Smelt ideas, skim the bugs, forge the product.

Task orchestrator for AI-powered development. Breaks requirements into S-expression ingots and forges them via configurable smith CLIs with automatic retry, re-smelt recovery, and proof-based verification.

## What's new in v1.3.16

- Stale molten recovery prompt: when forge finds interrupted `molten` ingots with no runnable `ore`, it now prompts for action: `requeue` (default), `crack`, or `abort`.
- Re-forge ETA hint: the stale-state prompt includes a best-effort time estimate based on recent forge logs.
- Safer unattended behavior: in non-interactive runs, stale `molten` ingots auto-requeue to `ore` so CI/headless runs do not hang.

## Install

```bash
# one-liner (auto-adds to PATH)
curl -sSf https://slag.dev/install.sh | sh

# bash version (no build required)
curl -fsSL https://slag.dev/slag.sh -o /usr/local/bin/slag && chmod +x /usr/local/bin/slag

# from source
cargo install --git https://github.com/sliday/slag
```

## Quick Start

```bash
# write your requirements
cat > PRD.md << 'EOF'
Build a REST API with auth and rate limiting
EOF

# forge
slag "Build the REST API from PRD.md"
```

> **Warning:** slag gives the configured smith autonomous shell access. Use in a dedicated directory or container.

## Pipeline

```
  PRD.md                                          PROGRESS.md
  (ore)                                             (ledger)
    |                                                 ^
    v                                                 |
 +-----------+    +--------------+    +--------+    +---------+    +-------+
 | SURVEYOR  |--->| FOUNDER      |--->| FORGE  |--->| OUTCOME |--->| ASSAY |
 | analyze   |    | cast ingots  |    | strike |    | validate|    | report|
 +-----------+    +--------------+    +--------+    +---------+    +-------+
    |                    |                    |             |
    v                    v                    v             v
 BLUEPRINT.md        PLAN.md             git commits   repair ingots
 (analysis)        (s-expr ingots)      (per ingot)   (on FAIL)
```

## Forge Loop

```
 PICK ORE                       PARALLEL ANVILS
    |                           +---------+---------+
    v                           |         |         |
 :solo t? ----yes-----> ANVIL 1   ANVIL 2   ANVIL 3
    |                      |         |         |
    no                     v         v         v
    |                   (each anvil is independent subshell)
    v
 SELECT SMITH by :skill + :grade
    |
    |  web/frontend --> +Playwright
    |  grade >= 3   --> plan mode
    |  default      --> base tools
    v
 STRIKE (smith invocation)
    |
    v
 CMD (extract & run shell command)
    |
    v
 PROOF (run :proof shell command)
    |
    +----- pass ----> :forged  + git commit
    |
    +----- fail ----> :heat++ retry with slag feedback
    |
    +----- max -----> RE-SMELT (analyze failure)
                         |
                         +----- rewrite --> new ore (retry)
                         +----- split ----> sub-ingots (2-4)
                         +----- impossible -> :cracked
```

## Five Phases

### 1. SURVEYOR
Deep analysis with plan mode. Reads PRD.md (ore), produces BLUEPRINT.md with architecture, dependency graph, risk assessment, and forging sequence. Self-iterates to resolve any ambiguity.

### 2. FOUNDER
Casts S-expression ingots from the blueprint. Each ingot has an ID, complexity grade, skill tag, proof command, and work description. Outputs PLAN.md as the crucible.
If the model returns wrappers/prose instead of ingot lines, slag automatically re-casts founder output with stricter constraints.
Multiline/wrapped `(ingot ...)` expressions are now parsed from mixed output to avoid false zero-ingot failures.

### 3. FORGE
Strikes each ingot via the configured smith. Solo ingots run on parallel anvils (up to 3). Selects smith by skill and grade. Retries with slag feedback on failure. Commits on success.

Default forge output is compact for readability. Use `--verbose` (or `--debug`) for detailed per-heat logs, longer Surveyor/Founder previews, and periodic stall heartbeats.
Set `SLAG_VERBOSE_HEARTBEAT_SECS` (default `15`) to control verbose heartbeat cadence for long-running anvils.
If a previous run crashed and left ingots in `molten` state, forge now prompts with options to `requeue` (default), `crack`, or `abort`; in non-interactive runs it defaults to `requeue`. The prompt includes a best-effort re-forge time estimate from recent logs.

### 4. OUTCOME
Independent tester/commenter pass validates user-visible behavior. If outcome fails, slag appends repair ingots and re-enters forge automatically. Disable with `--no-outcome`.
Set `SLAG_SMITH_OUTCOME` to run this validator on a specific model profile if desired (default is non-interactive plan mode). Other phases can be routed independently with `SLAG_SMITH_SURVEYOR`, `SLAG_SMITH_FOUNDER`, `SLAG_SMITH_REVIEW`, and `SLAG_SMITH_RECOVERY`.
Validator/recast calls are timeout-bounded via `SLAG_OUTCOME_TIMEOUT_SECS` (default 180). Proof/test commands are timeout-bounded via `SLAG_PROOF_TIMEOUT_SECS` (default 120).
If validator output is malformed (for example prose without `TEST:`), slag recasts validation and falls back to inferred/runtime proofs so the loop continues.
For web/simulation outcomes, outcome TEST commands must be headless and emit a screenshot to `$SLAG_OUTCOME_SCREENSHOT` (default `logs/outcome-smoke.png`).
For uncertain web outcomes, slag can force deterministic validation via `scripts/outcome_web_smoke.js` (page loads, runtime metric > 0, zero console errors, screenshot artifact).
Founder/outcome confidence is scored; thresholds come from `SLAG_CONFIDENCE_THRESHOLD` or phase overrides (`SLAG_FOUNDER_CONFIDENCE_THRESHOLD`, `SLAG_OUTCOME_CONFIDENCE_THRESHOLD`).
Low-confidence founder/outcome cases can escalate once via `SLAG_SMITH_SUBAGENT` (default auto-detect: `kimi`, `codex`, `gemini`, `opencode`, then `claude`; timeout `SLAG_SUBAGENT_TIMEOUT_SECS`).

### 5. ASSAY
Final quality report. Shows forged/cracked counts, temperature bar, and identifies any cracked ingots. Exits 0 on full forge, 1 if any ingot cracked.

## Design Decisions

### Why S-Expressions?
S-expressions are single-line, grep/sed parseable, require zero dependencies, and survive bash string handling. Every ingot is one line in PLAN.md. The entire orchestrator can manipulate state with sed_i without any JSON/YAML parser. Fields are keyword-prefixed (`:id`, `:status`) making them unambiguous to extract with pattern matching.

### Why Parallel Anvils?
Independent ingots (`:solo t`) run concurrently in background subshells, up to MAX_ANVILS=3. This gives 3x throughput for foundation tasks that have no dependencies. Each anvil gets its own smith process. Sequential ingots (`:solo nil`) run one at a time after parallel work completes.

### Why Proof-Based Verification?
Every ingot carries a `:proof` field containing a shell command. Exit code 0 means pass, anything else means fail. No human review needed. Proofs are concrete: `test -f file`, `npm test`, `grep -q pattern file`. This enables fully autonomous forging with machine-verifiable quality gates.

### Why Self-Iteration?
When a surveyor or founder output contains questions, slag detects them and feeds the output back with instructions to resolve autonomously. Up to 3 rounds. This prevents the forge from stalling on ambiguity. The AI is instructed to make expert decisions rather than ask for clarification.

### Why Re-Smelt + Reconsider?
When an ingot cracks after exhausting all heats, re-smelting analyzes failure logs, blueprint, and git history to diagnose the root cause. Recovery outputs now pass a strict contract: change approach, keep concrete proofs, and avoid previously failed proof signatures. If primary recovery output is rejected, slag can escalate to `SLAG_SMITH_INDEPENDENT` for an independent retry. If a re-smelted ingot cracks again, a reconsider pass rethinks the approach (not just tweaks), so each ingot gets two recovery stages before permanently cracking.

### Why Metallurgical Metaphor?
Unambiguous vocabulary that maps naturally to the pipeline. Ore (raw input) is surveyed, cast into ingots, heated in a forge, and either becomes forged steel or cracked waste. Every term has exactly one meaning. The temperature gradient (cold → hot → pure) maps to progress from unstarted to complete.

## FAQ

### "command not found: slag" after install
The installer adds `~/.slag/bin` to your shell profile automatically. Open a new terminal or run:
```bash
source ~/.zshrc  # or ~/.bashrc
```
If that doesn't work, manually add to your profile: `export PATH="$HOME/.slag/bin:$PATH"`

### How do I update slag?
Run `slag update` to self-update to the latest release. This downloads the new binary from GitHub and replaces the current one.

### What's the difference between binary and bash versions?
The Rust binary is faster, has better error handling, and includes self-update. The bash script is a single file with no build step — useful if you can't install Rust or want to inspect/modify the orchestrator directly.

### Do I need Claude CLI installed?
Not for the Rust binary. It auto-detects the first available supported smith CLI (`kimi`, `codex`, `gemini`, `opencode`, then `claude`) unless you set `SLAG_SMITH` explicitly. The legacy bash script still expects Claude CLI.

## Ingot S-Expression Format

```lisp
(ingot :id "i1" :status ore :solo t :grade 2 :skill web :heat 0 :max 5
       :proof "test -f index.html && npm test"
       :work "Create project structure with index.html and test suite")
```

## Field Reference

| Field | Values | Meaning |
|-------|--------|---------|
| `:id` | "i1", "i2", ... | Unique ingot identifier |
| `:status` | ore \| molten \| forged \| cracked | Lifecycle state |
| `:solo` | t \| nil | Can run in parallel (t) or must be sequential (nil) |
| `:grade` | 1-5 | Complexity level; grade >= 3 uses plan mode |
| `:skill` | web \| api \| cli \| default | Selects smith tools/plugins |
| `:heat` | 0-N | Current retry attempt |
| `:max` | 5-8+ | Max retries before cracking |
| `:smelt` | 0-2+ | Re-smelt/reconsider count (0 = never, 1 = re-smelted, 2 = reconsidered) |
| `:proof` | shell command | Acceptance test (exit 0 = pass) |
| `:work` | string | Task description for the smith |

## Links

- [GitHub](https://github.com/sliday/slag)
- [Releases](https://github.com/sliday/slag/releases)
- [Download bash version](https://slag.dev/slag.sh)

MIT License
