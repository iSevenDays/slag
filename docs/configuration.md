# Slag Configuration Reference

Full env-var reference for slag. Slag-specific variables take precedence over OpenAI-compat fallbacks, which take precedence over capability profile defaults.

Run `slag smith doctor` to print the resolved configuration for each smith in your chain.

---

## Smith selection

| Variable | Default | Purpose |
|----------|---------|---------|
| `SLAG_SMITH` | auto-detected | Main smith. Accepts selectors (`claude`, `codex`, `gemini`, `opencode`, `kimi`, `vllm`) or a full command string. |
| `SLAG_SMITH_CHAIN` | auto-generated | Comma-separated failover chain. Each entry is a selector or full command. |
| `SLAG_SMITH_SURVEYOR` | routed high-grade variant of `SLAG_SMITH` | Surveyor phase override. |
| `SLAG_SMITH_FOUNDER` | `SLAG_SMITH` | Founder phase override. |
| `SLAG_SMITH_REVIEW` | `SLAG_SMITH` | Review phase override. |
| `SLAG_SMITH_RECOVERY` | `SLAG_SMITH` | Re-smelt / reconsider phase override. |
| `SLAG_SMITH_OUTCOME` | routed high-grade variant of `SLAG_SMITH` | Outcome-validation phase override. |
| `SLAG_SMITH_INDEPENDENT` | unset (disabled) | Optional independent fallback for recovery escalation. |
| `SLAG_SMITH_SUBAGENT` | auto-detected | Subagent fallback for low-confidence founder/outcome cases. |

**Auto-detection order:** `claude` → `codex` → `gemini` → `opencode` → `kimi` (Claude-compat) → `kimi` (native) → `vllm` (when `SLAG_VLLM_BASE_URL` is set).
If `ANTHROPIC_API_KEY` is present, auto-detection skips Claude while other smiths are available to avoid accidental API-key billing.

---

## vLLM / HTTP smith

Slag-specific variables win over OpenAI-compat fallbacks.

| Variable | Fallback | Default | Purpose |
|----------|----------|---------|---------|
| `SLAG_VLLM_BASE_URL` | `OPENAI_BASE_URL` | **required** | Base URL of the vLLM server (e.g. `http://192.168.0.24:8080`). Trailing slash stripped. |
| `SLAG_VLLM_MODEL` | `OPENAI_MODEL` | `"auto"` | Model ID to request. `"auto"` lets vLLM select the currently-loaded model. Set to the exact HuggingFace model ID when multiple models are served. |
| `SLAG_VLLM_API_KEY` | `OPENAI_API_KEY` | `"EMPTY"` | Bearer token. Always sends a non-empty value (`EMPTY` by default) — never `Bearer None` (vLLM issue #33412). |
| `SLAG_VLLM_TIMEOUT_SECS` | `SLAG_SMITH_TIMEOUT_SECS` | `300` | Per-request timeout in seconds. |
| `SLAG_VLLM_ENABLE_THINKING` | — | `true` | Enable extended thinking / reasoning mode on capable models. Set to `0` or `false` to disable. |

**Worked precedence example:** `SLAG_VLLM_API_KEY` unset + `OPENAI_API_KEY=sk-xxx` → adapter uses `sk-xxx`. If both unset → adapter sends `EMPTY`.

---

## Prompt behavior

| Variable | Default | Purpose |
|----------|---------|---------|
| `SLAG_PROMPT_REPEAT_MODE` | `non-plan` | Prompt repetition mode: `off`, `non-plan` (repeat unless plan-mode command), `always`. |
| `SLAG_PROMPT_REPEAT_COUNT` | `2` | Repetitions when mode is active (clamped 1–4). |
| `SLAG_PROMPT_REPEAT_MAX_CHARS` | `40000` | Full repetition up to this size; tail-only repetition above. |
| `SLAG_PROMPT_BUDGET_TOKENS` | unset (no limit) | Approximate token budget for prompt context sections. When set, truncates `[BLUEPRINT]`, `[CRUCIBLE]`, and `[LEDGER]` head-tail (30%/30%) if total exceeds `N × 4` chars. Example: `SLAG_PROMPT_BUDGET_TOKENS=8000` enforces ~32K char budget. |

---

## Timing and timeouts

| Variable | Default | Purpose |
|----------|---------|---------|
| `SLAG_SMITH_TIMEOUT_SECS` | `300` | Global smith subprocess timeout. |
| `SLAG_OUTCOME_TIMEOUT_SECS` | `180` | Per-call timeout for outcome validator. |
| `SLAG_SUBAGENT_TIMEOUT_SECS` | `90` | Per-call timeout for subagent fallback. |
| `SLAG_PROOF_TIMEOUT_SECS` | `120` | Shell proof/test command timeout. |
| `SLAG_INGOT_BUDGET_SECS` | unset (disabled) | Per-ingot wall-clock time budget (0 = disabled). |
| `SLAG_STALL_MULTIPLIER` | `2.0` | Stall detection: abort anvil when elapsed > multiplier × average sibling time. |
| `SLAG_STALL_FLOOR_SECS` | `600` | Minimum stall threshold regardless of multiplier. |

---

## Quality thresholds

| Variable | Default | Purpose |
|----------|---------|---------|
| `SLAG_CONFIDENCE_THRESHOLD` | `0.65` | Global escalation threshold for uncertainty. |
| `SLAG_FOUNDER_CONFIDENCE_THRESHOLD` | inherits `SLAG_CONFIDENCE_THRESHOLD` | Founder-specific threshold. |
| `SLAG_OUTCOME_CONFIDENCE_THRESHOLD` | inherits `SLAG_CONFIDENCE_THRESHOLD` | Outcome-specific threshold. |

---

## Forge behavior

| Variable | Default | Purpose |
|----------|---------|---------|
| `SLAG_GIT_EXPERIMENTS` | unset | Enable commit-before-verify / revert-on-fail experiment tracking (`1` or `true`). |
| `SLAG_PROMPT_POLICY` | `ask` | Operator prompt behavior: `ask`, `auto-requeue`, `auto-crack`, `auto-abort`. |
| `SLAG_PROMPT_TIMEOUT_SECS` | `45` | Prompt timeout before default action. |
| `SLAG_LOG_FORMAT` | `text` | Output format: `text` or `json`. |
| `SLAG_EFFORT` | unset | Global smith effort level: `low`, `medium`, `high`. |
| `SLAG_SURVEYOR_EFFORT` | `low` | Surveyor-specific effort level. |

---

## Testing and bench

| Variable | Default | Purpose |
|----------|---------|---------|
| `SLAG_BENCH` | unset | Set to `1` to enable the fixture-based smith adherence bench (`cargo test smith_fixtures`). Not included in default `cargo test --all`. |
