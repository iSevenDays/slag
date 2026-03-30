# slag

[slag.dev](https://slag.dev)

**Smelt ideas, skim the bugs, forge the product.**

A task orchestrator for AI-powered development. Give it a product requirement, and it breaks it into verifiable tasks, executes them via a configured smith CLI, and proves each one passed before moving on.

![slag-promo](https://github.com/user-attachments/assets/d12def06-6eab-4236-9634-bbbd09be6683)

## What's new in v1.5.0

- **Default parallel anvils raised from 3 to 6.** Better utilization for Claude Max and high-rate-limit API tiers. Override with `--anvils N`.
- **Stall detection with proactive cancellation.** If an anvil exceeds 2x the average sibling completion time (floor 10 min), it is aborted and the ingot requeued as ore. Configurable via `SLAG_STALL_MULTIPLIER` (default 2.0) and `SLAG_STALL_FLOOR_SECS` (default 600). Works in all modes, not just verbose.
- **Parallel proof re-evaluation.** Post-forge proof checks for cracked ingots now run concurrently via JoinSet instead of sequentially. With 6 cracked ingots this reduces re-eval from ~12 min worst case to ~2 min.

## What's new in v1.4.1

- **Fix: `truncate_output` byte cap for short-but-huge output.** When output had few lines but exceeded 4KB, the early return path skipped the byte cap and duplicated lines. Now applies byte cap directly.
- **Fix: `git_revert_last` now creates proper revert commits.** Removed `--no-commit` flag that was leaving dirty tree state, bleeding staged changes into the next heat's experiment commit.
- **Fix: `parse_test_output` accumulates across crates.** Multi-crate `cargo test --all` output now sums pass/fail counts instead of only keeping the last crate's numbers.

## What's new in v1.4.0

- **`slag self-improve` — slag can improve its own codebase and submit PRs.** Clones from GitHub, forges improvements in `/tmp/` sandbox, measures before/after metrics (tests, clippy), and creates a PR via `gh` if improved. Anyone can contribute improvements without touching the source repo directly.
- **Freeform targets:** `slag self-improve quality` (predefined) or `slag self-improve "Add streaming support to smith output"` (any freeform text becomes a commission).
- **Existing PR detection:** before starting, queries GitHub for open self-improve PRs. Shows a TUI picker to continue an existing PR or start fresh.
- **Predefined targets:** `quality` (clippy), `tests` (coverage), `performance` (allocations), `tokens` (prompt sizes).
- **First verified self-improvement:** slag fixed its own pre-existing clippy warning at `config.rs:345` (needless borrow) that persisted across 5+ releases.

## What's new in v1.3.39

- **TOON tabular experiment ledger**: experiment log switched from JSONL to [TOON](https://github.com/toon-format/toon) (Token-Oriented Object Notation) tabular format. Field names written once in header, rows are compact comma-separated values. ~21% smaller on disk, larger savings when injected into smith prompts. Legacy JSONL files still readable (automatic fallback).
- **Compact crucible in prompts**: forge prompts now include a summary + current ingot + cracked ingots only, instead of the full PLAN.md. 57% reduction for small projects, 95%+ for large ones (50+ ingots).
- **TOON history injection**: retry prompts use compact tabular format (`HISTORY(id)[N]{h,status,dur,err,hash}:`) instead of verbose prose blocks.
- **Shorter section headers**: `=== BLUEPRINT ===` → `[BLUEPRINT]` across all flux prompts. Small per-prompt savings that compound over hundreds of invocations.

## What's new in v1.3.38

- **Experiment-driven forge loop** (inspired by [autoresearch](https://github.com/karpathy/autoresearch)): every heat is now a tracked experiment. Smith work is git-committed BEFORE verification; failures are git-reverted but preserved in history. Enable with `SLAG_GIT_EXPERIMENTS=1` or `--worktree` (always active).
- **Structured experiment ledger**: every heat (success and failure) recorded as JSONL in `logs/experiments.jsonl` with timing, commit hash, status, and description.
- **History-informed retries**: on retry, the smith receives structured experiment history (heat #, status, duration, error) instead of raw "CRACKED" messages.
- **Per-ingot time budget**: new `:budget N` field caps smith invocation wall-clock time per heat. Configurable via `SLAG_INGOT_BUDGET_SECS` env or per-ingot field.
- **Crash recovery protocol**: infrastructure failures (smith timeout, rate limit, missing CMD) no longer consume heats. Only real experiments count. Up to 6 free infra retries before chain failover.
- **Smart output truncation** (inspired by [pi coding-agent](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent)): slag messages keep first 5 + last 30 lines, capped at 4KB. Full output in logs.
- **Structured failure context**: retry messages use typed fields (Type, Exit, CMD, Files changed) instead of raw output dumps.
- **Flux caching**: blueprint and alloy files read once per ingot, reused across heats instead of re-reading from disk every heat.

## What's new in v1.3.37

- **Claude auto-detect guardrail:** if `ANTHROPIC_API_KEY` is present, auto-detection skips Claude while other supported smith CLIs are available, avoiding accidental API-key billing.
- **Claude subscription fallback:** if Claude is selected and fails with an API-key or billing-style error, slag now checks for subscription auth and retries once without `ANTHROPIC_API_KEY` when available.
- **Runtime detection tested locally:** end-to-end shim-based tests now confirm the actual selected smith matches the intended policy, not just the unit-level chain ordering.

## What's new in v1.3.36

- **Explicit smith selection:** `slag` now supports `--smith` and `--smith-chain`, so users can pick an agent without exporting env vars.
- **Claude-first preference restored:** auto-detection now prefers `claude`, then `codex`, then `gemini`, then the remaining supported smith CLIs.
- **`claude-plan` / `kimi-plan` selectors:** high-grade routing for the built-in Claude-compatible wrappers now resolves cleanly to plan mode.

## What's new in v1.3.33

- **Fix: `--dangerously-skip-permissions` no longer conflicts with `--permission-mode plan`:** The surveyor and other high-grade smith invocations were appending `--permission-mode plan` even when the base command already had `--dangerously-skip-permissions` (which implies `bypassPermissions`). Claude CLI rejects conflicting permission flags with exit 1. Slag now skips adding plan mode when bypass mode is already active.
- **Better error diagnostics:** When the smith process exits non-zero with empty stderr, slag now falls back to capturing stdout in the error message. Previously, errors reported to stdout (common in Claude CLI) were silently lost, producing unhelpful `exit 1:` messages.

## What's new in v1.3.32

- **Bare `slag` resumes quarried phases:** Previously, running `slag` (no args) mid-project with PHASES.md would only forge the current phase and never advance to remaining phases. Now PHASES.md presence always loads the multi-phase pipeline, so bare `slag` picks up exactly where it left off.

## What's new in v1.3.31

- **Effort control (`--effort`):** Smith invocations now support `--effort <level>` (low/medium/high) to control extended thinking budget, preventing surveyor timeouts. Surveyor defaults to `low` effort. Configure globally via `SLAG_EFFORT` or surveyor-specific via `SLAG_SURVEYOR_EFFORT`.

## What's new in v1.3.29

- **Commission chunking (quarrier phase):** Large commissions (>500 chars) are automatically decomposed into 2-5 ordered build phases before survey. Each phase runs the full survey → found → forge pipeline independently. Small commissions skip the quarrier entirely.
- **Resume support:** Quarried phases are persisted to `PHASES.md`. If a run is interrupted mid-phase, `slag resume` picks up from where it stopped.
- **`--no-quarry` flag:** Disable commission chunking to force single-pass behavior (old behavior).

## What's new in v1.3.28

- **Sanitized synthetic repair proofs:** outcome validator no longer assigns `echo "STATUS:PASS"` as proof commands — these validator output markers are stripped before creating synthetic repair ingots.
- **Extension exclusion from browser tests:** Chrome/Firefox/Safari extensions and Manifest V3 projects are no longer flagged as browser-testable, preventing impossible Playwright/screenshot requirements.
- **Subagent timeout guard:** when the primary outcome validator times out, slag no longer escalates to a 90s subagent call — total dead time reduced from 270s to 180s.
- **Identical-failure early bail:** after 3 consecutive identical failures in the heat loop, slag bails to resmelt instead of burning remaining heats on the same error.
- **Cycle-aware repair IDs:** synthetic repair ingots now use `v_auto_c{cycle}` IDs to avoid naming confusion across outcome cycles.
- **Softer confidence penalties:** browser test mismatch penalty reduced from -0.20 to -0.10, preventing impossible-to-reach confidence thresholds for non-Playwright-testable outcomes.

## What's new in v1.3.27

- **Claude became the default smith in v1.3.27:** auto-detection priority was reordered to `claude` first in that release.
- **Usage-limit failover:** when any smith hits a usage or rate limit, slag automatically fails over to the next smith in the chain instead of crashing.

## What's new in v1.3.26

- **Post-forge proof re-evaluation:** after each forge cycle, slag re-runs the `:proof` command for every cracked ingot. If the proof passes (because an earlier forged ingot already made the needed change), the cracked ingot is promoted to forged without invoking a smith. This eliminates entire retry cycles when overlapping ingots target the same files.

## What's new in v1.3.25

- **Sandbox failure detection:** analysis now recognizes read-only sandbox errors (`READ_ONLY_SANDBOX`, `filesystem writes are blocked`, `operation not permitted`) and immediately skips instead of retrying indefinitely.
- **Smelt history preserved:** retry cycles no longer reset the `:smelt` counter, so ingots that have been through re-smelt and reconsider reach the skip threshold naturally instead of spiraling.
- **Prompt repetition activated:** raised max-chars threshold from 12K to 40K so real forge prompts get repeated. Prompts exceeding the limit now get partial tail repetition (last ~2000 chars) instead of being skipped entirely.

## Install

**Binary** (recommended):
```bash
curl -sSf https://slag.dev/install.sh | sh
```

**Bash version** (no build required):
```bash
curl -fsSL https://slag.dev/slag.sh -o /usr/local/bin/slag && chmod +x /usr/local/bin/slag
```

**From source**:
```bash
cargo install --git https://github.com/sliday/slag
```

## Quick start

```bash
# Write your requirements
cat > PRD.md << 'EOF'
Build a REST API with user authentication, rate limiting,
and PostgreSQL storage. Include health check endpoint.
EOF

# Forge it
slag "Build the REST API from PRD.md"
```

slag reads `PRD.md`, analyzes it, designs tasks, executes them, and proves each one works.

## Usage

```
slag [OPTIONS] [COMMISSION]... [COMMAND]
```

**Commands:**

| Command | Description |
|---------|-------------|
| `slag "Build X from PRD.md"` | Start a new forge from a commission |
| `slag status` | Show crucible state (ingot counts and progress) |
| `slag resume` | Resume an existing forge |
| `slag update` | Self-update to latest release |
| `slag self-improve [target]` | Self-improve slag's code via GitHub clone + PR (freeform or predefined target) |

**Options:**

| Flag | Default | Description |
|------|---------|-------------|
| `--worktree` | off | Enable branch-per-ingot worktree isolation with master review |
| `--anvils N` | 3 | Max parallel anvil workers |
| `--skip-review` | off | Skip the master review phase (legacy behavior) |
| `--keep-branches` | off | Don't delete branches after review |
| `--ci-only` | off | Run CI checks but skip AI review |
| `--review-all` | off | Review even if CI fails |
| `--retry N` | 3 | Max retry cycles when ingots crack (0 = no retry) |
| `--verbose` (`--debug`) | off | Show detailed forge output (commands, retries, extended previews, and stall heartbeats) |
| `--no-outcome` | off | Disable independent outcome-validation closing loop |
| `--no-quarry` | off | Disable commission chunking (quarrier phase) |
| `--prompt-policy MODE` | `ask` | Operator prompt behavior: `ask`, `auto-requeue`, `auto-crack`, `auto-abort` |
| `--prompt-timeout-secs N` | 45 | Prompt timeout before default action |
| `--log-format FORMAT` | `text` | Output renderer format: `text` or `json` |
| `--effort LEVEL` | unset | Smith effort level: `low`, `medium`, `high` (controls extended thinking) |
| `--smith VALUE` | unset | Base smith selector or full command, e.g. `claude`, `claude-plan`, `codex` |
| `--smith-chain VALUE` | unset | Comma-separated fallback smith selectors or full commands |

**Model routing (env):**

| Variable | Default | Purpose |
|----------|---------|---------|
| `SLAG_SMITH` | auto-detected (`claude` / `codex` / `gemini` / `opencode` / `kimi`; native `kimi` fallback) | Main smith for survey/founder/forge |
| `SLAG_SMITH_CHAIN` | auto-generated from detected smiths | Comma-separated fallback chain for forge smith failover (aliases: `kimi`, `codex`, `gemini`, `opencode`, `claude`) |
| `SLAG_SMITH_SURVEYOR` | routed high-grade planning variant of `SLAG_SMITH` | Override model/flags for Surveyor phase |
| `SLAG_SMITH_FOUNDER` | `SLAG_SMITH` | Override model/flags for Founder phase |
| `SLAG_SMITH_REVIEW` | `SLAG_SMITH` | Override model/flags for Review phase |
| `SLAG_SMITH_RECOVERY` | `SLAG_SMITH` | Override model/flags for analysis/re-smelt/reconsider phases |
| `SLAG_SMITH_OUTCOME` | same routed planning variant used by Surveyor | Independent outcome validator (non-interactive by default; override to use a specific model/profile) |
| `SLAG_SMITH_INDEPENDENT` | unset (disabled) | Optional independent fallback smith for recovery escalation after rejected re-smelt/reconsider output |
| `SLAG_SMITH_SUBAGENT` | auto-detected (`claude` / `codex` / `gemini` / `opencode` / `kimi`; native `kimi` fallback) | Optional uncertainty fallback smith (used only on low-confidence founder/outcome cases) |
| `SLAG_CONFIDENCE_THRESHOLD` | `0.65` | Global default threshold for uncertainty escalation |
| `SLAG_FOUNDER_CONFIDENCE_THRESHOLD` | inherits `SLAG_CONFIDENCE_THRESHOLD` | Founder-specific escalation threshold |
| `SLAG_OUTCOME_CONFIDENCE_THRESHOLD` | inherits `SLAG_CONFIDENCE_THRESHOLD` | Outcome-specific escalation threshold |
| `SLAG_OUTCOME_TIMEOUT_SECS` | `180` | Max seconds for each validator/recast response before fallback fail path |
| `SLAG_SUBAGENT_TIMEOUT_SECS` | `90` | Max seconds for each subagent fallback invocation |
| `SLAG_PROOF_TIMEOUT_SECS` | `120` | Max seconds for proof/test shell commands before timeout failure |
| `SLAG_PROMPT_POLICY` | `ask` | Default operator prompt behavior (`ask`, `auto-requeue`, `auto-crack`, `auto-abort`) |
| `SLAG_PROMPT_TIMEOUT_SECS` | `45` | Default prompt timeout when flag is not provided |
| `SLAG_LOG_FORMAT` | `text` | Default log format (`text` or `json`) |
| `SLAG_EFFORT` | unset | Global smith effort level (`low`, `medium`, `high`) |
| `SLAG_SURVEYOR_EFFORT` | `low` | Surveyor-specific effort level (controls extended thinking budget) |
| `SLAG_PROMPT_REPEAT_MODE` | `non-plan` | Prompt repetition mode (`off`, `non-plan`, `always`) |
| `SLAG_PROMPT_REPEAT_COUNT` | `2` | Prompt repetitions when enabled (clamped `1..4`) |
| `SLAG_PROMPT_REPEAT_MAX_CHARS` | `40000` | Full repetition up to this size; partial tail repetition above |
| `SLAG_GIT_EXPERIMENTS` | unset | Enable git commit-before-verify / revert-on-fail experiment tracking (`1` or `true`) |
| `SLAG_INGOT_BUDGET_SECS` | unset | Default per-ingot wall-clock time budget in seconds (0 = disabled) |

When `SLAG_SMITH` is unset, slag picks the first compatible smith in this order:
`claude`, `codex`, `gemini`, `opencode`, `kimi` (Claude-compatible), then native `kimi` as last fallback.
If `ANTHROPIC_API_KEY` is present, auto-detection skips Claude while other supported smiths are available, to avoid accidental API-key billing. Explicit `--smith claude` or `SLAG_SMITH=claude` still overrides that policy.

Common selectors for `--smith` and `SLAG_SMITH`: `claude`, `claude-plan`, `codex`, `gemini`, `opencode`, `kimi`, `kimi-plan`.

Forge now uses a runtime failover chain: if the active smith hard-fails protocol/invocation for an ingot, slag retries that ingot on the next smith in `SLAG_SMITH_CHAIN` automatically.
For Claude CLI specifically, slag keeps the current auth mode first, but if `ANTHROPIC_API_KEY` is set and the run fails with an API-key or billing-style error, it checks whether Claude subscription auth is already available and retries once with `ANTHROPIC_API_KEY` removed.

## Progress display

slag shows emoji progress in the terminal:

```
[ ✅3  🔥1  🧱5 ] 37%
```

| Emoji | Status | Meaning |
|-------|--------|---------|
| 🧱 | queued | Ingot is ore, waiting to be forged |
| 🔥 | forging | Ingot is molten, currently being worked |
| ✅ | done | Ingot is forged, proof passed |
| ❌ | failed | Ingot cracked after exhausting all heats |

The percentage shows overall progress: forged ingots / total ingots.

By default, forge output is compact and optimized for readability. Use `--verbose` for full per-heat details and longer previews during Surveyor/Founder phases.
Set `SLAG_VERBOSE_HEARTBEAT_SECS` (default `15`) to control verbose heartbeat cadence for long-running anvils.
If a previous run crashed and left ingots in `molten` state, forge now prompts with options to `requeue` (default), `crack`, or `abort`; in non-interactive runs it defaults to `requeue`. The prompt includes a best-effort re-forge time estimate from recent logs.
Operator prompts are now policy-driven (`--prompt-policy`) and timeout-bounded (`--prompt-timeout-secs`) to prevent stalled loops.
Based on prompt repetition results (arXiv:2512.14982), smith calls now repeat prompts by default for non-plan invocations with configurable guardrails via `SLAG_PROMPT_REPEAT_*`.

## Language

slag uses metallurgical vocabulary. Here's the dictionary.

### Nouns

| Term | What it is | File/location |
|------|-----------|---------------|
| **Ore** | Raw requirements; the starting material | `PRD.md` |
| **Ingot** | A single task encoded as an S-expression | One line in `PLAN.md` |
| **Crucible** | The file holding all ingots | `PLAN.md` |
| **Blueprint** | Architecture analysis and forging plan | `BLUEPRINT.md` |
| **Quarrier** | Pre-survey decomposer that splits large commissions into phases | `PHASES.md` |
| **Anvil** | A parallel execution slot (background process) | In-memory |
| **Smith** | The AI agent that does the work (configured smith CLI) | CLI invocation |
| **Slag heap** | Debug logs dumped during forging | `logs/` directory |
| **Heat** | One attempt at forging an ingot (retry count) | `:heat` field |
| **Grade** | Complexity rating (1-5); high grade = plan mode | `:grade` field |
| **Proof** | Shell command that verifies the work (exit 0 = pass) | `:proof` field |
| **Skill** | Tool configuration for the smith (web, default) | `:skill` field |
| **Temper bar** | Progress visualization in the terminal | TUI output |
| **Sparks** | Animated spinner shown during work | TUI output |

### Verbs

| Term | What it does | Phase |
|------|-------------|-------|
| **Quarry** | Decompose large commissions into ordered build phases | Phase 0 |
| **Survey** | Analyze requirements, produce blueprint | Phase 1 |
| **Found** | Design and cast ingots from blueprint | Phase 2 |
| **Forge** | Execute an ingot: strike, run commands, verify | Phase 3 |
| **Strike** | Send work to the smith and get output | Phase 3 |
| **Smelt** | Process raw ore into workable material | Phase 3 |
| **Re-smelt** | Analyze a cracked ingot and rewrite/split it | Phase 3 (recovery) |
| **Reconsider** | Rethink a twice-cracked ingot's fundamental approach | Phase 3 (recovery) |
| **Temper** | Track and display forging progress | Phase 3 |
| **Review** | Master agent CI checks and code review before merge | Phase 3.5 (--worktree) |
| **Assay** | Final quality check, produce report | Phase 4 |
| **Crack** | Fail permanently after exhausting all heats | Terminal state |

### Ingot lifecycle

```
ore --> molten --> forged
                   \--> cracked --> [re-smelt] --> ore (retry)
                                                    \--> cracked --> [reconsider] --> ore (rethought)
                                                                                  --> ore + ore (decomposed)
                                                                                  --> cracked (truly impossible)
```

## How it works

slag runs a 5-phase pipeline (6 phases with `--worktree`). Large commissions add a Phase 0 quarrier that splits into 2-5 build phases:

```
PRD.md --> [QUARRIER] --> SURVEYOR --> BLUEPRINT.md --> FOUNDER --> PLAN.md --> FORGE --> OUTCOME --> PROGRESS.md
 (ore)    (decompose)    (analyze)    (blueprint)     (design)   (crucible)  (strike)   (validate)   (ledger)

With --worktree (master review enabled):
PRD.md --> SURVEYOR --> FOUNDER --> FORGE (branches) --> REVIEW --> OUTCOME --> ASSAY
                                         |                  |          |
                                    git worktrees      CI + AI     independent
                                    per ingot          review      validator
```

### Phase 1: Surveyor

Reads `PRD.md` and produces `BLUEPRINT.md` -- architecture decisions, dependency graph, risk assessment, and forging sequence. Uses smith plan mode.

### Phase 2: Founder

Reads the blueprint and casts S-expression ingots into `PLAN.md`:

```
(ingot :id "i1" :status ore :solo t :grade 1 :skill default :heat 0 :max 5
       :proof "test -f package.json" :work "Initialize project with package.json")
```

Founder now includes a format-recovery pass: if the model returns wrappers/prose instead of ingot lines, slag automatically re-casts with stricter output constraints.
The parser also extracts multiline/wrapped `(ingot ...)` expressions from mixed output, so benign formatting noise does not dead-stop the pipeline.

### Phase 3: Forge

The main loop. For each ingot:

1. **Pick** the next ore-status ingot
2. **Strike** -- invoke the smith with the task, context, and skill tools
3. **Run** -- extract and execute shell commands from smith output
4. **Proof** -- run the `:proof` command; exit 0 = forged, non-zero = retry

Independent ingots (`:solo t`) run on parallel anvils. Sequential ingots (`:solo nil`) run one at a time.

### Phase 3.5: Review (with `--worktree`)

When `--worktree` is enabled, each ingot is forged in an isolated git worktree branch (`forge/iN`). After forging completes, the Review phase:

1. **CI Checks** -- runs `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --all` on each branch
2. **Master Review** -- AI agent reviews the diff, code quality, and integration safety
3. **Merge Decision** -- approved branches merge to main; rejected branches are marked cracked for retry/recovery

Use `--ci-only` to skip AI review and auto-merge on CI pass. Use `--skip-review` for legacy behavior (merge forged worktree branches without master review). Use `--keep-branches` to preserve branches for debugging.

### Phase 3.6: Analysis & Retry

When ingots crack, slag analyzes failures and can retry automatically (up to `--retry N` cycles):

1. **Proof re-evaluation** -- re-runs `:proof` for every cracked ingot; promotes to forged if proof now passes (zero-cost check that eliminates overlapping work)
2. **Failure detection** -- identifies patterns: missing dependencies, protocol failures, proof mismatches, JSON errors, sandbox/permission blocks
3. **Fix application** -- converts parallel ingots to sequential if they have dependencies
4. **Strict retry contract** -- repaired ingots must change approach, keep concrete proofs, and avoid failed proof signatures
5. **Independent fallback lane** -- optional escalation to `SLAG_SMITH_INDEPENDENT` if primary repair output is rejected
6. **Regeneration** -- uses founder to regenerate ingots that can't be fixed simply
7. **Retry** -- re-runs forge with fixed/regenerated ingots
8. **Force retry prompt** -- when no recoverable ingots found, asks user to confirm force retry

This loop continues until all ingots forge, max retries exhausted, or user declines force retry.

### Phase 3.7: Outcome Validation (closing loop)

Even when all ingots are forged, slag runs an **independent validator pass** to verify user-visible outcomes (not just file existence).

1. **Independent check** -- separate tester/commenter prompt evaluates commission vs delivered behavior
2. **PASS/FAIL decision** -- `PASS` finishes pipeline, `FAIL` must include repair ingots
3. **Auto-repair loop** -- repair ingots are appended to `PLAN.md` and forged in the next cycle
4. **Behavior-first proofs** -- validator requires runtime-focused checks (browser/runtime assertions for web/sim apps)
5. **Screenshot requirement for web outcomes** -- browser/simulation TEST commands must write a non-empty screenshot artifact to `logs/outcome-smoke.png` (or `$SLAG_OUTCOME_SCREENSHOT`)
6. **Deterministic web smoke fallback** -- for uncertain web outcomes, slag can run `scripts/outcome_web_smoke.js` to verify page load, runtime metric > 0, console errors = 0, and screenshot output
7. **Confidence scoring + escalation** -- founder/outcome compute confidence scores and escalate once via `SLAG_SMITH_SUBAGENT` when below threshold
8. **Format recovery** -- if validator output is malformed (missing `STATUS:`/`TEST:`), slag re-runs validation with a strict recast prompt and fallback TEST inference
9. **Never dead-stop on validator drift/timeouts** -- if validator fails/times out/omits repair ingots, slag falls back to fail-path + synthetic repair ingot so the cycle continues

Disable this closing loop with `--no-outcome`.

### Phase 4: Assay

Final report. Counts forged vs cracked, writes results to `PROGRESS.md`.

## Ingot fields

```
(ingot :id "i3" :status ore :solo t :grade 2 :skill web :heat 0 :max 5
       :proof "curl -s localhost:3000/health | grep -q ok"
       :work "Add health check endpoint returning JSON {status: ok}")
```

| Field | Values | Meaning |
|-------|--------|---------|
| `:id` | string | Unique identifier |
| `:status` | ore / molten / forged / cracked | Lifecycle state |
| `:solo` | t / nil | Can run in parallel (t) or must be sequential (nil) |
| `:grade` | 1-5 | Complexity; grade >= 3 uses plan mode |
| `:skill` | default / web / ... | Tool configuration for the smith |
| `:heat` | 0-N | Current retry attempt |
| `:max` | 5-8+ | Max retries before cracking |
| `:smelt` | 0-2+ | Re-smelt/reconsider count (0 = never, 1 = re-smelted, 2 = reconsidered) |
| `:budget` | seconds (optional) | Per-ingot wall-clock time limit for smith invocation |
| `:proof` | shell command | Acceptance test (exit 0 = pass) |
| `:work` | string | Task description for the AI |

## Project files

| File | Role |
|------|------|
| `PRD.md` | Requirements input (ore) |
| `BLUEPRINT.md` | Surveyor analysis |
| `PLAN.md` | Ingot crucible (task list) |
| `PROGRESS.md` | Work history ledger |
| `PHASES.md` | Quarried build phases (multi-phase runs) |
| `AGENTS.md` | Agent recipe docs |
| `logs/` | Debug logs (slag heap) |
| `logs/experiments.toon` | Structured experiment ledger (TOON tabular) |

## Development

```bash
# Rust binary
cargo test --all
cargo clippy -- -D warnings
cargo run -- "Your commission"

# Website (slag.dev)
cd website
npm install
npm run dev       # Dev server at localhost:5173
npm run build     # Production build
npx wrangler pages deploy dist --project-name=slag-dev

# Bash tests
bash tests/test_slag.sh
```

### Repository structure

```
Cargo.toml              # Rust project
src/                    # Rust source (24 files)
slag.sh                 # Bash orchestrator (legacy)
install.sh              # curl | sh installer
website/                # slag.dev (Vite + Cloudflare Pages)
tests/                  # Bash test suite
example/                # Real slag run outputs
.github/workflows/      # CI + release automation
```

## Requirements

- **Rust binary**: one supported smith CLI in PATH (`claude`, `kimi`, `codex`, `gemini`, or `opencode`)
- **Bash version**: bash 4+, Claude CLI, curl, sed, awk
- **Optional**: Playwright (for `:skill web` ingots)

## License

MIT

## Warning

slag gives the configured smith autonomous shell access. It will create files, install packages, and run commands without asking. Use in a dedicated directory or container.
