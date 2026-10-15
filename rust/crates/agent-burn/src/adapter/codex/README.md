# Codex Source

Data source:

```text
${CODEX_HOME:-~/.codex}/sessions/
${CODEX_HOME:-~/.codex}/archived_sessions/
```

When both directories contain the same relative JSONL path for one Codex home,
the active `sessions/` copy wins.

Weekly quota uses the live ChatGPT `wham/usage` account meter when a Codex
OAuth token is available and `--offline` is not set. Session `rate_limits`
envelopes are the fallback. When Codex logs are detected but `auth.json` holds
no ChatGPT token (an API-key sign-in included), live `summary --value --json`
sets `codexAccountStatus: "signedOut"`. Model-specific Spark meters (`codex_bengalfox`)
are not the account weekly quota.

`wham/usage` sometimes over-counts the weekly meter for the same window (seen:
99% while Codex enforced 49%). The live read keeps the lowest of three
concurrent requests, and a session-log meter from the last ten minutes with the
same reset replaces a live value that far exceeds it. Session files are ordered
by file name, which carries the start time, so every Codex home is compared
chronologically.

Relevant JSONL event:

- `type === "event_msg"`
- `payload.type === "token_count"`
- `payload.info.total_token_usage` is cumulative.
- `payload.info.last_token_usage` is the current turn delta.
- If only cumulative totals exist, subtract prior totals to recover deltas.

Token mapping:

- `input_tokens` - total input tokens.
- `cached_input_tokens` - cached prompt tokens.
- `output_tokens` - completion tokens, including reasoning cost.
- `reasoning_output_tokens` - informational breakdown; already included in output billing.
- `total_tokens` - provided directly or recomputed as input plus output for legacy entries.

Pricing uses model metadata from `turn_context`. Early sessions without metadata fall back to `gpt-5`, mark `isFallbackModel === true`, and expose fallback rows as approximate in aggregate JSON.
