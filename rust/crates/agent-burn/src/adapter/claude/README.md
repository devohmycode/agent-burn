# Claude Code Source

Default data directories:

- `~/.config/claude/projects/`
- `~/.claude/projects/`

`CLAUDE_CONFIG_DIR` can specify one path or comma-separated multiple paths. Data from valid directories is combined.

File shape:

```text
projects/{project}/{sessionId}/{file}.jsonl
projects/{project}/{sessionId}.jsonl
```

`projects/` is scanned recursively, so both nested session directories and legacy flat JSONL files can be loaded.

Sidechain entries:

- Claude Code may write `isSidechain: true` entries for isolated sidechain
  conversations such as `/btw` `aside_question` logs under `subagents/`. See
  the Claude Code
  [side questions documentation](https://code.claude.com/docs/en/interactive-mode#side-questions-with-btw).
- These files can replay parent conversation messages with the same message ID
  but a different request ID, including the parent cache-read usage.
- agent-burn keeps the parent entry and drops the replayed sidechain copy when at
  least one duplicate carries `isSidechain: true`. Distinct sidechain responses
  with their own message IDs are still counted.
- This behavior fixes the overcounting reported in
  [#913](https://github.com/Melvynx/agent-burn/issues/913).

The term `session` has two meanings in this codebase:

- Session report grouping uses project directories.
- For nested files, session reports derive `sessionId` from the session directory name.
- True Claude Code session ID may also appear in each JSONL entry's `sessionId` field.

Malformed JSONL lines are skipped during parsing.

Live `summary --value --json` can include `claudeAccount`. Agent Burn reads
`https://api.anthropic.com/api/oauth/usage` with the signed-in Claude Code
OAuth token. The parser accepts the modern `limits` array (`session`,
`weekly_all`, `weekly_scoped`) and the older top-level `five_hour` /
`seven_day` windows, plus `extra_usage` or `spend` credits. `--offline`
omits the account object.

Readings resolve in this order, and the newest one always wins:

1. The shared `claude-usage.json` reading when it is under 55 seconds old.
2. `cachedUsageUtilization` in `~/.claude.json`: the raw endpoint body Claude
   Code cached for itself, with `fetchedAtMs`. It is ignored when its
   `accountUuid` differs from `oauthAccount.accountUuid`. It is adopted into
   the shared cache, so it outlives Claude Code's own cache.
3. The OAuth endpoint, unless a 429 backoff is active. The token's `expiresAt`
   is checked first: an expired token is renewed and never sent, so it never
   earns a 429 backoff that would also block the renewed one. A 401 renews the
   token once and retries.

Renewal (`oauth_refresh.rs`) follows Claude Code 2.1's protocol, because the
refresh token is single use and two uncoordinated refreshers log every
`claude` session out:

1. Hold `~/.claude/.oauth_refresh.lock`, then the legacy `~/.claude.lock`
   (proper-lockfile directories, stale after 60 seconds). When a `claude`
   session holds them, wait up to 10 seconds and adopt what it stored.
2. Re-read the Keychain item `Claude Code-credentials` (or
   `~/.claude/.credentials.json`). A different, unexpired access token means
   another client renewed it first, so it is used as is.
3. `POST https://platform.claude.com/v1/oauth/token` with the refresh token,
   Claude Code's client id, and the stored scopes.
4. Write back only when the stored refresh token is still the one posted,
   merging the new tokens and expiries into the existing JSON. The Keychain
   write goes through `security -i` stdin, so no secret reaches `argv`.

A refused renewal sets `claudeAccountStatus: "signInExpired"` and is retried
after 10 minutes; only `/login` in `claude` recovers a revoked sign-in. When
Claude logs are detected but no sign-in is stored, live `summary --value --json`
sets `claudeAccountStatus: "signedOut"`.

`AGENT_BURN_FORCE_REFRESH=1` is the app's hard refresh. It skips the shared
fresh reading and retries a refused renewal right away.
