# Engine JSON contract

These fixtures record what `agent-burn.exe` prints for the three reports the
Windows application reads. `tests/fixtures.rs` decodes every one of them with
`src/models.rs` and checks the states each scenario stands for.

## Capture

```sh
cargo run --example capture_fixtures -- <path\to\agent-burn.exe>
```

`examples/capture_fixtures.rs` clears its environment, builds synthetic
profiles dated from the current time, runs the engine through the
application's own client, replaces the temporary folder with `C:\Fixture`,
and refuses any output that contains the user name, computer name or
profile path. No provider receives a token: Claude meters come from a Claude
Code usage cache, Codex meters from session logs. The engine only downloads
the public LiteLLM pricing file. `manifest.json` lists each run: arguments,
extra environment, exit code, and the stdout and stderr files.

| Command          | Arguments                                                                    |
| ---------------- | ---------------------------------------------------------------------------- |
| `summary`        | `summary --value --breakdown --json --no-color --timezone UTC`               |
| `harness-claude` | `harness claude --value --json --no-color --timezone UTC`                    |
| `harness-codex`  | `harness codex --value --json --no-color --timezone UTC`                     |
| `harness-cursor` | `harness cursor --value --json --no-color` (rejected: Cursor has no harness) |

| Scenario           | Profile                                                                                         | What it shows                                                                        |
| ------------------ | ----------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| `quotas`           | Claude and Codex logs, a fresh Claude Code usage cache, Codex `rate_limits` in session logs     | `claudeAccount`, quota windows for both harnesses, `codexAccountStatus: "signedOut"` |
| `single-source`    | Claude logs and a fresh Claude Code usage cache only                                            | one agent; `harness codex` without a source                                          |
| `partial`          | Expired Claude sign-in without refresh token, Codex meters in logs, Cursor installed signed out | `signInExpired` for Claude, a Codex window, `cursorAccountStatus: "signedOut"`       |
| `no-quota`         | Claude and Codex logs, no sign-in, cache or logged meter                                        | `signedOut` for both, `window: null` in both harnesses                               |
| `no-quota-offline` | Same profile, `--offline`                                                                       | no account field and no status                                                       |
| `no-source`        | Empty profile                                                                                   | `agents: []`, totals of zero, `subscription.period: null`                            |
| `error`            | `CLAUDE_CONFIG_DIR` names a missing folder; Codex logs valid                                    | exit 1 for `summary` and `harness claude`, exit 2 for `harness cursor`               |
| `live-accounts`    | Constructed, not captured                                                                       | Cursor usage and balances, Codex dashboard history and banked resets                 |

`live-accounts` adds to the `quotas` outputs what only signed-in accounts
produce, written the way `cursor/account.rs`, `all/subscription.rs` and
`codex/limit_usage.rs` print it. A capture from real accounts on 2026-10-13
had exactly the same keys, apart from Cursor grant entries (empty there) and
the sign-in statuses (absent for signed-in accounts).

## Reading rules

- Exit code 0: stdout holds one JSON object. stderr may still carry warnings
  (`WARN  …`), for example when a Cursor request failed.
- Any other exit code: stdout is empty and stderr explains, either
  `Error: CliError("…")` (exit 1) or a usage message (exit 2). An invalid
  `CLAUDE_CONFIG_DIR` fails the whole summary, not only Claude.
- A missing value is never zero. Fields the engine may omit or print as
  `null` are `Option`s in `src/models.rs`. Ratios are optional too: the engine
  prints a non-finite ratio as `null`. Costs and token counts are always
  printed; their absence is a decoding error.
- No detected agent gives `agents: []` with zero totals: check `agents`
  before showing a total.
- Unknown fields are ignored, so a newer engine keeps decoding.

## Units

| Kind                                                                          | Unit                                                                     |
| ----------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `cost`, `totalCost`, `…USD`, `pricePerMonth`, `periodUsage`, `apiEquivalent…` | US dollars, API-equivalent unless named otherwise                        |
| `…Percent`, `percentage`, `usedPercent`                                       | 0–100; `projectedUsePercent` may exceed 100                              |
| `…Ms`                                                                         | milliseconds since the Unix epoch                                        |
| `date`, `weekStart`, `period.from/to`                                         | `YYYY-MM-DD` in the report timezone (`--timezone`, else the system zone) |
| `windowMinutes`, `elapsedMinutes`                                             | minutes                                                                  |
| `tokens`, `totalTokens`                                                       | token count                                                              |

API-equivalent costs are what the usage would cost at API prices. They are
neither the subscription price (`pricePerMonth`) nor billed credits
(`cursorAccount`, `claudeAccount.extra…`).

## `summary --value --breakdown --json`

| Field                            | Presence                        | Notes                                                                                         |
| -------------------------------- | ------------------------------- | --------------------------------------------------------------------------------------------- |
| `totals.totalCost/totalTokens`   | always                          | zero without agents                                                                           |
| `agents[]`                       | always                          | `agent`, `totalCost`, `totalTokens`, `models[]`, `daily[]`, `tokenBreakdown`                  |
| `agents[].daily[].cursorModels…` | Cursor rows only                | Composer, Auto and other Cursor-hosted models                                                 |
| `models[]`, `daily[]`            | always                          | across agents; `percentage` is the cost share                                                 |
| `subscription.period`            | `null` without usage            | `from`, `to`                                                                                  |
| `subscription.agents[]`          | detected subscription harnesses | `plan`, `pricePerMonth` may be null; `window`, `estimate`, `shortWindow` null without a meter |
| `claudeAccount`                  | live runs with a Claude reading | session/weekly percent and reset, `scoped[]`, `extra…`, `observedAtMs`                        |
| `cursorAccount`                  | live runs with a Cursor sign-in | current billing cycle, whatever the report period                                             |
| `claudeAccountStatus`            | live runs                       | `signedOut` (logs, no sign-in) or `signInExpired` (renewal refused)                           |
| `codexAccountStatus`             | live runs                       | `signedOut` (logs, no ChatGPT token in `auth.json`)                                           |
| `cursorAccountStatus`            | live runs                       | `signedOut` (`state.vscdb` without token)                                                     |

A missing status means no sign-in problem was seen, not that the limits are
live. `liveLimits: true` means an account meter is known: live, from Claude
Code's cache, or from Codex session logs. It does not mean the meter was read
just now; `quotas` has `liveLimits: true` for Codex without any sign-in.

## `harness <claude|codex> --value --json`

| Field                                        | Presence                            | Notes                                                                                    |
| -------------------------------------------- | ----------------------------------- | ---------------------------------------------------------------------------------------- |
| `plan`, `pricePerMonth`                      | null when unknown                   |                                                                                          |
| `liveLimits`                                 | always                              | see above                                                                                |
| `window`                                     | null without a meter                | weekly window: `windowMinutes`, `usedPercent`, `elapsedPercent`, `apiEquivalentSpent`    |
| `daily[]`                                    | days of the window                  | `date`, `cost`; empty without a window                                                   |
| `topModels[]`, `spendMix[]`, `weeklyTrend[]` | always                              | may be empty                                                                             |
| `economics`, `estimate`                      | null without a plan price or window |                                                                                          |
| `resetCreditsAvailable`                      | Codex live only                     | banked rate-limit resets                                                                 |
| `limitUsageDaily[]`                          | Codex live only                     | ChatGPT dashboard history: `usedPercent`, `surfaces[]`, `models[]` (`speed` may be null) |

The harness window has no reset time and no observation time; the session
(5-hour) window is not in it. Reset times come from `claudeAccount` or from
the quota collector protocol (`AGENT_BURN_QUOTA_ONLY=1`).
