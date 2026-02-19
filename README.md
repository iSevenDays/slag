# slag

[slag.dev](https://slag.dev)

**Smelt ideas, skim the bugs, forge the product.**

A task orchestrator for AI-powered development. Give it a product requirement, and it breaks it into verifiable tasks, executes them via Claude, and proves each one passed before moving on.

![slag-promo](https://github.com/user-attachments/assets/d12def06-6eab-4236-9634-bbbd09be6683)

## What's new in v1.3.12

- **Better retry quality:** re-smelt/reconsider outputs must change approach, keep concrete proofs, and avoid previously failed proof signatures.
- **Independent recovery lane:** set `SLAG_SMITH_INDEPENDENT` to run a separate fallback smith when primary repair output is rejected.
- **No stale retry loops:** forge refreshes ingot `:work`/`:proof` from `PLAN.md` before each heat.
- **Safer proof parsing:** quoted backslashes/quotes now round-trip cleanly in S-expression parser/writer.

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
| `--verbose` | off | Show detailed forge output (commands, retries, extended previews) |
| `--no-outcome` | off | Disable independent outcome-validation closing loop |

**Model routing (env):**

| Variable | Default | Purpose |
|----------|---------|---------|
| `SLAG_SMITH` | `claude --dangerously-skip-permissions -p` | Main smith for survey/founder/forge |
| `SLAG_SMITH_SURVEYOR` | `SLAG_SMITH --permission-mode plan` | Override model/flags for Surveyor phase |
| `SLAG_SMITH_FOUNDER` | `SLAG_SMITH` | Override model/flags for Founder phase |
| `SLAG_SMITH_REVIEW` | `SLAG_SMITH` | Override model/flags for Review phase |
| `SLAG_SMITH_RECOVERY` | `SLAG_SMITH` | Override model/flags for analysis/re-smelt/reconsider phases |
| `SLAG_SMITH_OUTCOME` | `SLAG_SMITH --permission-mode plan` | Independent outcome validator (non-interactive by default; override to use a specific model/profile) |
| `SLAG_SMITH_INDEPENDENT` | unset (disabled) | Optional independent fallback smith for recovery escalation after rejected re-smelt/reconsider output |
| `SLAG_SMITH_SUBAGENT` | `npx -y @anthropic-ai/claude-code -p` | Optional uncertainty fallback smith (used only on low-confidence founder/outcome cases) |
| `SLAG_CONFIDENCE_THRESHOLD` | `0.65` | Global default threshold for uncertainty escalation |
| `SLAG_FOUNDER_CONFIDENCE_THRESHOLD` | inherits `SLAG_CONFIDENCE_THRESHOLD` | Founder-specific escalation threshold |
| `SLAG_OUTCOME_CONFIDENCE_THRESHOLD` | inherits `SLAG_CONFIDENCE_THRESHOLD` | Outcome-specific escalation threshold |
| `SLAG_OUTCOME_TIMEOUT_SECS` | `180` | Max seconds for each validator/recast response before fallback fail path |
| `SLAG_SUBAGENT_TIMEOUT_SECS` | `90` | Max seconds for each subagent fallback invocation |
| `SLAG_PROOF_TIMEOUT_SECS` | `120` | Max seconds for proof/test shell commands before timeout failure |

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

## Language

slag uses metallurgical vocabulary. Here's the dictionary.

### Nouns

| Term | What it is | File/location |
|------|-----------|---------------|
| **Ore** | Raw requirements; the starting material | `PRD.md` |
| **Ingot** | A single task encoded as an S-expression | One line in `PLAN.md` |
| **Crucible** | The file holding all ingots | `PLAN.md` |
| **Blueprint** | Architecture analysis and forging plan | `BLUEPRINT.md` |
| **Anvil** | A parallel execution slot (background process) | In-memory |
| **Smith** | The AI agent that does the work (Claude) | Claude CLI invocation |
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
| **Survey** | Analyze requirements, produce blueprint | Phase 1 |
| **Found** | Design and cast ingots from blueprint | Phase 2 |
| **Forge** | Execute an ingot: strike, run commands, verify | Phase 3 |
| **Strike** | Send work to the smith (Claude) and get output | Phase 3 |
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

slag runs a 5-phase pipeline (6 phases with `--worktree`):

```
PRD.md --> SURVEYOR --> BLUEPRINT.md --> FOUNDER --> PLAN.md --> FORGE --> OUTCOME --> PROGRESS.md
 (ore)    (analyze)    (blueprint)     (design)   (crucible)  (strike)   (validate)   (ledger)

With --worktree (master review enabled):
PRD.md --> SURVEYOR --> FOUNDER --> FORGE (branches) --> REVIEW --> OUTCOME --> ASSAY
                                         |                  |          |
                                    git worktrees      CI + AI     independent
                                    per ingot          review      validator
```

### Phase 1: Surveyor

Reads `PRD.md` and produces `BLUEPRINT.md` -- architecture decisions, dependency graph, risk assessment, and forging sequence. Uses Claude's plan mode.

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
2. **Strike** -- invoke Claude with the task, context, and skill tools
3. **Run** -- extract and execute shell commands from Claude's output
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

1. **Failure detection** -- identifies patterns: missing dependencies, protocol failures, proof mismatches, JSON errors
2. **Fix application** -- converts parallel ingots to sequential if they have dependencies
3. **Strict retry contract** -- repaired ingots must change approach, keep concrete proofs, and avoid failed proof signatures
4. **Independent fallback lane** -- optional escalation to `SLAG_SMITH_INDEPENDENT` if primary repair output is rejected
5. **Regeneration** -- uses founder to regenerate ingots that can't be fixed simply
6. **Retry** -- re-runs forge with fixed/regenerated ingots
7. **Force retry prompt** -- when no recoverable ingots found, asks user to confirm force retry

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
| `:proof` | shell command | Acceptance test (exit 0 = pass) |
| `:work` | string | Task description for the AI |

## Project files

| File | Role |
|------|------|
| `PRD.md` | Requirements input (ore) |
| `BLUEPRINT.md` | Surveyor analysis |
| `PLAN.md` | Ingot crucible (task list) |
| `PROGRESS.md` | Work history ledger |
| `AGENTS.md` | Agent recipe docs |
| `logs/` | Debug logs (slag heap) |

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

- **Rust binary**: Claude CLI (`claude` in PATH)
- **Bash version**: bash 4+, Claude CLI, curl, sed, awk
- **Optional**: Playwright (for `:skill web` ingots)

## License

MIT

## Warning

slag gives Claude autonomous shell access. It will create files, install packages, and run commands without asking. Use in a dedicated directory or container.
