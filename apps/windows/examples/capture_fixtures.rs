//! Regenerates `tests/fixtures/`: runs a real `agent-burn.exe` against
//! synthetic profiles and records what it prints.
//!
//! ```text
//! cargo run --example capture_fixtures -- <agent-burn.exe> [output dir]
//! ```
//!
//! The profiles hold no account: Claude's quota comes from a Claude Code usage
//! cache, Codex's from session-log meters, and no provider is ever sent a
//! token. Only the public LiteLLM pricing file is downloaded by the engine.
//! The process environment is cleared first, so the user's own agent
//! variables cannot leak real data in.

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use agent_burn_windows::cli_client::{self, EngineError, EngineRequest};
use serde_json::{Value, json};

const KEPT_VARIABLES: &[&str] = &["PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP"];
const ROOT_PLACEHOLDER: &str = r"C:\Fixture";

const COMMANDS: &[(&str, &[&str])] = &[
    (
        "summary",
        &[
            "summary",
            "--value",
            "--breakdown",
            "--json",
            "--no-color",
            "--timezone",
            "UTC",
        ],
    ),
    (
        "harness-claude",
        &[
            "harness",
            "claude",
            "--value",
            "--json",
            "--no-color",
            "--timezone",
            "UTC",
        ],
    ),
    (
        "harness-codex",
        &[
            "harness",
            "codex",
            "--value",
            "--json",
            "--no-color",
            "--timezone",
            "UTC",
        ],
    ),
    // Cursor has no harness: its data comes from `summary` and `cursorAccount`.
    (
        "harness-cursor",
        &["harness", "cursor", "--value", "--json", "--no-color"],
    ),
];

struct Scenario {
    name: &'static str,
    description: &'static str,
    claude_logs: bool,
    claude_code_cache: bool,
    claude_expired_sign_in: bool,
    codex_logs: bool,
    codex_meters: bool,
    cursor_without_token: bool,
    claude_config_dir: Option<&'static str>,
    extra_args: &'static [&'static str],
    commands: &'static [&'static str],
}

const ALL: &[&str] = &["summary", "harness-claude", "harness-codex"];

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "quotas",
        description: "Claude and Codex logs; Claude meters from a fresh Claude Code usage cache; Codex meters from session logs; no Codex sign-in.",
        claude_logs: true,
        claude_code_cache: true,
        claude_expired_sign_in: false,
        codex_logs: true,
        codex_meters: true,
        cursor_without_token: false,
        claude_config_dir: None,
        extra_args: &[],
        commands: ALL,
    },
    Scenario {
        name: "single-source",
        description: "Only Claude: logs and a fresh Claude Code usage cache.",
        claude_logs: true,
        claude_code_cache: true,
        claude_expired_sign_in: false,
        codex_logs: false,
        codex_meters: false,
        cursor_without_token: false,
        claude_config_dir: None,
        extra_args: &[],
        commands: ALL,
    },
    Scenario {
        name: "partial",
        description: "Claude sign-in expired without a refresh token; Codex meters from session logs; Cursor installed without a sign-in.",
        claude_logs: true,
        claude_code_cache: false,
        claude_expired_sign_in: true,
        codex_logs: true,
        codex_meters: true,
        cursor_without_token: true,
        claude_config_dir: None,
        extra_args: &[],
        commands: ALL,
    },
    Scenario {
        name: "no-quota",
        description: "Claude and Codex logs without any sign-in, usage cache or logged meter.",
        claude_logs: true,
        claude_code_cache: false,
        claude_expired_sign_in: false,
        codex_logs: true,
        codex_meters: false,
        cursor_without_token: false,
        claude_config_dir: None,
        extra_args: &[],
        commands: ALL,
    },
    Scenario {
        name: "no-quota-offline",
        description: "The no-quota profile with --offline: sign-ins and live meters are not consulted.",
        claude_logs: true,
        claude_code_cache: false,
        claude_expired_sign_in: false,
        codex_logs: true,
        codex_meters: false,
        cursor_without_token: false,
        claude_config_dir: None,
        extra_args: &["--offline"],
        commands: &["summary"],
    },
    Scenario {
        name: "no-source",
        description: "An empty profile: no agent data at all.",
        claude_logs: false,
        claude_code_cache: false,
        claude_expired_sign_in: false,
        codex_logs: false,
        codex_meters: false,
        cursor_without_token: false,
        claude_config_dir: None,
        extra_args: &[],
        commands: ALL,
    },
    Scenario {
        name: "error",
        description: "CLAUDE_CONFIG_DIR names a missing folder; Codex logs are valid.",
        claude_logs: false,
        claude_code_cache: false,
        claude_expired_sign_in: false,
        codex_logs: true,
        codex_meters: true,
        cursor_without_token: false,
        claude_config_dir: Some("missing-claude-config"),
        extra_args: &[],
        commands: &[
            "summary",
            "harness-claude",
            "harness-codex",
            "harness-cursor",
        ],
    },
];

fn main() {
    let mut args = env::args_os().skip(1);
    let engine = PathBuf::from(
        args.next()
            .expect("usage: capture_fixtures <agent-burn.exe> [output dir]"),
    );
    let output = args.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
    });
    let forbidden = forbidden_strings();
    clear_environment();

    let root = tempfile::tempdir().expect("create the profile folder");
    let root_path = root.path().to_path_buf();
    let now = now_seconds();
    let version = engine_version(&engine);
    let mut runs = Vec::new();
    for scenario in SCENARIOS {
        let profile = root_path.join(scenario.name);
        build_profile(scenario, &profile, now);
        let folder = output.join(scenario.name);
        fs::create_dir_all(&folder).expect("create the fixture folder");
        for (command, base_args) in COMMANDS
            .iter()
            .filter(|(name, _)| scenario.commands.contains(name))
        {
            let args: Vec<&str> = base_args
                .iter()
                .chain(scenario.extra_args)
                .copied()
                .collect();
            if scenario.claude_code_cache {
                write_claude_code_cache(&profile, now_seconds());
            }
            let mut request = EngineRequest::new(args.iter().copied());
            if let Some(dir) = scenario.claude_config_dir {
                request = request.env("CLAUDE_CONFIG_DIR", profile.join(dir));
            }
            let (exit_code, stdout, stderr) = match cli_client::run(&engine, &request) {
                Ok(output) => (0, output.stdout, output.stderr),
                Err(EngineError::Failed { code, stderr }) => {
                    (code.unwrap_or(-1), Vec::new(), stderr)
                }
                Err(error) => panic!("{} {command}: {error}", scenario.name),
            };
            let stdout = anonymize(
                &String::from_utf8(stdout).expect("UTF-8 stdout"),
                &root_path,
            );
            let stderr = anonymize(&stderr, &root_path);
            for text in [&stdout, &stderr] {
                if let Some(found) = forbidden
                    .iter()
                    .find(|value| text.to_lowercase().contains(value.as_str()))
                {
                    panic!(
                        "{} {command}: output contains the local value {found:?}",
                        scenario.name
                    );
                }
            }
            let stdout_file = (!stdout.is_empty()).then(|| {
                let name = format!("{command}.json");
                let value: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
                fs::write(
                    folder.join(&name),
                    serde_json::to_string_pretty(&value).unwrap() + "\n",
                )
                .unwrap();
                format!("{}/{name}", scenario.name)
            });
            let stderr_file = (!stderr.is_empty()).then(|| {
                let name = format!("{command}.stderr.txt");
                fs::write(folder.join(&name), format!("{stderr}\n")).unwrap();
                format!("{}/{name}", scenario.name)
            });
            println!("{:<17} {command:<15} exit {exit_code}", scenario.name);
            runs.push(json!({
                "scenario": scenario.name,
                "command": command,
                "args": args,
                "env": scenario.claude_config_dir.map(|dir| json!({ "CLAUDE_CONFIG_DIR": format!(r"{ROOT_PLACEHOLDER}\{}\{dir}", scenario.name) })),
                "exitCode": exit_code,
                "stdout": stdout_file,
                "stderr": stderr_file,
            }));
        }
    }
    runs.extend(live_accounts(&output, now));
    let mut scenarios: Vec<Value> = SCENARIOS
        .iter()
        .map(|scenario| json!({ "name": scenario.name, "description": scenario.description }))
        .collect();
    scenarios.push(json!({
        "name": LIVE_ACCOUNTS,
        "description": "Constructed, not captured: the quotas outputs plus the shapes only signed-in accounts produce (Cursor usage and balances, Codex dashboard history and banked resets).",
        "constructed": true,
    }));
    let manifest = json!({
        "engine": version,
        "capturedAtMs": now * 1000,
        "scenarios": scenarios,
        "runs": runs,
    });
    fs::write(
        output.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
}

const LIVE_ACCOUNTS: &str = "live-accounts";

/// Adds to the `quotas` capture what only signed-in accounts produce, as the
/// engine writes it (`cursor/account.rs`, `all/subscription.rs`,
/// `codex/limit_usage.rs`); real captures confirm the key set.
fn live_accounts(output: &Path, now: i64) -> Vec<Value> {
    let read = |name: &str| -> Value {
        serde_json::from_str(&fs::read_to_string(output.join("quotas").join(name)).unwrap())
            .unwrap()
    };
    let today = iso(now)[..10].to_string();
    let yesterday = iso(now - 86_400)[..10].to_string();

    let mut summary = read("summary.json");
    summary
        .as_object_mut()
        .unwrap()
        .remove("codexAccountStatus");
    // Cursor usage: (date, cost, tokens, Cursor-model cost, Cursor-model tokens).
    let cursor_days = [
        (&yesterday, 0.42, 51_000, 0.30, 36_000),
        (&today, 0.18, 22_000, 0.18, 22_000),
    ];
    let cursor_models = [
        ("composer-1", 0.48, 58_000),
        ("claude-sonnet-4-5", 0.12, 15_000),
    ];
    let (cursor_cost, cursor_tokens) = (0.60, 73_000);
    summary["agents"].as_array_mut().unwrap().push(json!({
        "agent": "cursor",
        "daily": cursor_days.iter().map(|(date, cost, tokens, models_cost, models_tokens)| json!({
            "date": date, "cost": cost, "tokens": tokens,
            "cursorModelsCost": models_cost, "cursorModelsTokens": models_tokens,
        })).collect::<Vec<_>>(),
        "models": cursor_models.iter().map(|(model, cost, tokens)| json!({
            "model": model, "totalCost": cost, "totalTokens": tokens,
            "percentage": cost / cursor_cost * 100.0,
        })).collect::<Vec<_>>(),
        "tokenBreakdown": { "input": 9_000, "output": 4_000, "cacheRead": 60_000, "cacheWrite": 0 },
        "totalCost": cursor_cost,
        "totalTokens": cursor_tokens,
    }));
    let total_cost = summary["totals"]["totalCost"].as_f64().unwrap() + cursor_cost;
    summary["totals"]["totalCost"] = json!(total_cost);
    summary["totals"]["totalTokens"] =
        json!(summary["totals"]["totalTokens"].as_u64().unwrap() + cursor_tokens);
    let daily = summary["daily"].as_array_mut().unwrap();
    for (date, cost, tokens, _, _) in cursor_days {
        match daily.iter_mut().find(|row| row["date"] == json!(date)) {
            Some(row) => {
                row["cost"] = json!(row["cost"].as_f64().unwrap() + cost);
                row["tokens"] = json!(row["tokens"].as_u64().unwrap() + tokens);
            }
            None => daily.push(json!({ "date": date, "cost": cost, "tokens": tokens })),
        }
    }
    daily.sort_by(|a, b| a["date"].as_str().cmp(&b["date"].as_str()));
    let models = summary["models"].as_array_mut().unwrap();
    for (model, cost, tokens) in cursor_models {
        models.push(json!({ "model": model, "totalCost": cost, "totalTokens": tokens }));
    }
    for model in models.iter_mut() {
        model["percentage"] = json!(model["totalCost"].as_f64().unwrap() / total_cost * 100.0);
    }
    models.sort_by(|a, b| {
        b["totalCost"]
            .as_f64()
            .unwrap()
            .total_cmp(&a["totalCost"].as_f64().unwrap())
    });
    let subscriptions = summary["subscription"]["agents"].as_array_mut().unwrap();
    for agent in subscriptions
        .iter_mut()
        .filter(|agent| agent["agent"] == "codex")
    {
        agent["resetCreditsAvailable"] = json!(2);
    }
    subscriptions.push(json!({
        "agent": "cursor", "plan": "Pro", "pricePerMonth": 20, "periodUsage": cursor_cost,
        "liveLimits": false, "window": null, "estimate": null, "shortWindow": null,
        "resetCreditsAvailable": null,
    }));
    summary["cursorAccount"] = json!({
        "billingCycleStartMs": (now - 10 * 86_400) * 1000,
        "billingCycleEndMs": (now + 20 * 86_400) * 1000,
        "includedLimitUSD": 20.0,
        "includedRemainingUSD": 19.4,
        "includedPercentUsed": 3.0,
        "includedSpendUSD": 0.6,
        "bonusSpendUSD": 0.0,
        "planSpendUSD": 0.6,
        "onDemandSpentUSD": null,
        "onDemandLimitUSD": null,
        "activeRemainingUSD": 9.13,
        "activeLimitUSD": 10.0,
        "activePercentUsed": 8.7,
        "grants": [{
            "kind": "promo",
            "totalUSD": 10.0,
            "remainingUSD": 9.13,
            "expiresAtMs": (now + 20 * 86_400) * 1000,
        }],
    });

    let mut codex = read("harness-codex.json");
    codex["resetCreditsAvailable"] = json!(2);
    codex["limitUsageDaily"] = json!([
        {
            "date": yesterday, "usedPercent": 21.0,
            "surfaces": [{ "surface": "cli", "usedPercent": 15.0 }, { "surface": "desktop_app", "usedPercent": 6.0 }],
            "models": [{ "model": "gpt-5.3-codex", "speed": "standard", "usedPercent": 21.0 }],
        },
        {
            "date": today, "usedPercent": 4.5,
            "surfaces": [{ "surface": "cli", "usedPercent": 4.5 }],
            "models": [{ "model": "gpt-5.3-codex", "speed": null, "usedPercent": 4.5 }],
        },
    ]);

    let folder = output.join(LIVE_ACCOUNTS);
    fs::create_dir_all(&folder).unwrap();
    [("summary", summary, 0), ("harness-codex", codex, 2)]
        .into_iter()
        .map(|(command, value, base)| {
            fs::write(
                folder.join(format!("{command}.json")),
                serde_json::to_string_pretty(&value).unwrap() + "\n",
            )
            .unwrap();
            println!("{LIVE_ACCOUNTS:<17} {command:<15} constructed");
            json!({
                "scenario": LIVE_ACCOUNTS,
                "command": command,
                "args": COMMANDS[base].1,
                "env": null,
                "exitCode": 0,
                "stdout": format!("{LIVE_ACCOUNTS}/{command}.json"),
                "stderr": null,
            })
        })
        .collect()
}

/// Values that would identify this machine or its user in a fixture.
fn forbidden_strings() -> Vec<String> {
    ["USERNAME", "COMPUTERNAME", "USERPROFILE", "USERDOMAIN"]
        .iter()
        .filter_map(|key| env::var(key).ok())
        .filter(|value| value.len() >= 3)
        .map(|value| value.to_lowercase())
        .collect()
}

fn clear_environment() {
    let keys: Vec<OsString> = env::vars_os().map(|(key, _)| key).collect();
    for key in keys {
        let kept = KEPT_VARIABLES
            .iter()
            .any(|kept| key.to_string_lossy().eq_ignore_ascii_case(kept));
        if !kept {
            // SAFETY: no other thread runs yet.
            unsafe { env::remove_var(&key) };
        }
    }
}

fn engine_version(engine: &Path) -> String {
    let output =
        cli_client::run(engine, &EngineRequest::new(["--version"])).expect("engine --version");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn anonymize(text: &str, root: &Path) -> String {
    let root = root.display().to_string();
    text.replace(
        &root.replace('\\', r"\\"),
        &ROOT_PLACEHOLDER.replace('\\', r"\\"),
    )
    .replace(&root, ROOT_PLACEHOLDER)
    .trim()
    .to_string()
}

fn build_profile(scenario: &Scenario, profile: &Path, now: i64) {
    let roaming = profile.join(r"AppData\Roaming");
    fs::create_dir_all(&roaming).unwrap();
    fs::create_dir_all(profile.join(r"AppData\Local")).unwrap();
    // SAFETY: the generator is single-threaded between engine runs.
    unsafe {
        env::set_var("USERPROFILE", profile);
        env::set_var("APPDATA", &roaming);
        env::set_var("LOCALAPPDATA", profile.join(r"AppData\Local"));
        env::set_var("AGENT_BURN_CACHE_DIR", profile.join("agent-burn-cache"));
    }
    if scenario.claude_logs {
        write_claude_logs(profile, now);
    }
    if scenario.claude_expired_sign_in {
        write(
            &profile.join(r".claude\.credentials.json"),
            &json!({ "claudeAiOauth": {
                "accessToken": "expired-placeholder",
                "expiresAt": 1000,
                "scopes": ["user:inference", "user:profile"],
            }})
            .to_string(),
        );
    }
    if scenario.codex_logs {
        write_codex_logs(profile, now, scenario.codex_meters);
    }
    if scenario.cursor_without_token {
        write(&roaming.join(r"Cursor\User\globalStorage\state.vscdb"), "");
    }
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

const CLAUDE_ACCOUNT: &str = "00000000-0000-4000-8000-000000000001";

fn write_claude_logs(profile: &Path, now: i64) {
    let entries = [
        (
            1,
            "claude-sonnet-4-5-20250929",
            1200,
            800,
            3000,
            42000,
            Some(0.071),
        ),
        (1, "claude-opus-4-1-20250805", 300, 500, 1000, 9000, None),
        (
            3,
            "claude-sonnet-4-5-20250929",
            2500,
            1100,
            0,
            61000,
            Some(0.112),
        ),
        (9, "claude-haiku-4-5-20251001", 4000, 900, 1500, 12000, None),
    ];
    let lines: Vec<String> = entries
        .iter()
        .enumerate()
        .map(
            |(index, (days_ago, model, input, output, cache_write, cache_read, cost))| {
                let mut line = json!({
                    "timestamp": iso(now - days_ago * 86_400 - 3_600),
                    "sessionId": "session-0001",
                    "message": {
                        "role": "assistant",
                        "id": format!("msg-{index:04}"),
                        "model": model,
                        "usage": {
                            "input_tokens": input,
                            "output_tokens": output,
                            "cache_creation_input_tokens": cache_write,
                            "cache_read_input_tokens": cache_read,
                        },
                    },
                    "requestId": format!("req-{index:04}"),
                    "version": "2.1.0",
                });
                if let Some(cost) = cost {
                    line["costUSD"] = json!(cost);
                }
                line.to_string()
            },
        )
        .collect();
    write(
        &profile.join(r".claude\projects\C--Users-example-project\session-0001.jsonl"),
        &(lines.join("\n") + "\n"),
    );
    write(
        &profile.join(".claude.json"),
        &json!({ "oauthAccount": {
            "accountUuid": CLAUDE_ACCOUNT,
            "userRateLimitTier": "default_claude_max_5x",
        }})
        .to_string(),
    );
}

/// Claude Code's own usage reading, fresh as of `now`.
fn write_claude_code_cache(profile: &Path, now: i64) {
    write(
        &profile.join(".claude.json"),
        &json!({
            "oauthAccount": {
                "accountUuid": CLAUDE_ACCOUNT,
                "userRateLimitTier": "default_claude_max_5x",
            },
            "cachedUsageUtilization": {
                "fetchedAtMs": now * 1000,
                "accountUuid": CLAUDE_ACCOUNT,
                "utilization": {
                    "five_hour": { "utilization": 37, "resets_at": iso(now + 2 * 3_600) },
                    "seven_day": { "utilization": 58, "resets_at": iso(now + 3 * 86_400) },
                    "limits": [{
                        "kind": "weekly_scoped",
                        "percent": 21,
                        "resets_at": iso(now + 3 * 86_400),
                        "scope": { "model": { "display_name": "Opus" } },
                    }],
                    "extra_usage": {
                        "is_enabled": true,
                        "used_credits": 1250,
                        "monthly_limit": 5000,
                        "utilization": 25,
                    },
                },
            },
        })
        .to_string(),
    );
}

fn write_codex_logs(profile: &Path, now: i64, meters: bool) {
    let mut lines = vec![json!({
        "timestamp": iso(now - 2 * 86_400),
        "type": "turn_context",
        "payload": { "model": "gpt-5.3-codex" },
    })];
    let mut total = (0u64, 0u64, 0u64, 0u64);
    for (offset, input, cached, output, reasoning) in [
        (2 * 86_400 - 60, 18_000u64, 12_000u64, 1_400u64, 600u64),
        (86_400 + 7_200, 26_000, 20_000, 2_100, 900),
        (5_400, 9_000, 6_500, 800, 300),
    ] {
        total = (
            total.0 + input,
            total.1 + cached,
            total.2 + output,
            total.3 + reasoning,
        );
        let mut payload = json!({
            "type": "token_count",
            "info": {
                "last_token_usage": usage(input, cached, output, reasoning),
                "total_token_usage": usage(total.0, total.1, total.2, total.3),
            },
        });
        if meters {
            payload["rate_limits"] = json!({
                "limit_id": "codex",
                "primary": { "used_percent": 42.0, "window_minutes": 300, "resets_at": now + 2 * 3_600 },
                "secondary": { "used_percent": 63.0, "window_minutes": 10_080, "resets_at": now + 3 * 86_400 },
                "plan_type": "plus",
            });
        }
        lines.push(
            json!({ "timestamp": iso(now - offset), "type": "event_msg", "payload": payload }),
        );
    }
    let started = iso(now - 2 * 86_400).replace(':', "-");
    let day = &started[..10];
    let path = profile
        .join(r".codex\sessions")
        .join(&day[..4])
        .join(&day[5..7])
        .join(&day[8..10])
        .join(format!(
            "rollout-{}-00000000-0000-4000-8000-000000000002.jsonl",
            &started[..19]
        ));
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    write(&path, &(text.join("\n") + "\n"));
}

fn usage(input: u64, cached: u64, output: u64, reasoning: u64) -> Value {
    json!({
        "input_tokens": input,
        "cached_input_tokens": cached,
        "output_tokens": output,
        "reasoning_output_tokens": reasoning,
        "total_tokens": input + output,
    })
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// `seconds` since the epoch as `YYYY-MM-DDTHH:MM:SS.000Z`.
fn iso(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        time / 3_600,
        time % 3_600 / 60,
        time % 60
    )
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
