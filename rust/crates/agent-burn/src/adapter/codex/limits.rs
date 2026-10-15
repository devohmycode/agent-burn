use std::{fs, time::Duration};

use serde_json::Value;

use super::{
    paths::codex_home_paths,
    plan::{CodexPlanSnapshot, RateWindow},
};
use crate::TimestampMs;

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const FETCH_TIMEOUT_SECONDS: u64 = 5;
const FETCH_MAX_BYTES: u64 = 1_000_000;
/// How long a session-log meter stays a trustworthy floor for the live meter.
const LOG_TRUST_MS: i64 = 10 * 60_000;
/// Concurrent live reads per refresh; the lowest one for the window wins.
const LIVE_READS: usize = 3;
/// Two readings belong to the same weekly window when their resets agree this closely.
const SAME_WINDOW_SECONDS: i64 = 60;

/// Prefer the live ChatGPT account meter. Fall back to session logs only when
/// the live snapshot is missing, or is missing a weekly window or plan name.
pub(crate) fn resolve_plan_snapshot(offline: bool) -> Option<CodexPlanSnapshot> {
    match usage_limits(offline) {
        Some(live) if live.weekly_window().is_some() && has_known_plan(&live) => Some(live),
        Some(live) => Some(overlay_logs(live)),
        None => super::latest_plan_snapshot(),
    }
}

fn has_known_plan(snapshot: &CodexPlanSnapshot) -> bool {
    !snapshot.plan_type.is_empty() && snapshot.plan_type != "unknown"
}

fn overlay_logs(mut live: CodexPlanSnapshot) -> CodexPlanSnapshot {
    if let Some(logs) = super::latest_plan_snapshot() {
        if !has_known_plan(&live) && has_known_plan(&logs) {
            live.plan_type = logs.plan_type;
        }
        if live.weekly_window().is_none() {
            live.primary = logs.primary;
            live.secondary = logs.secondary;
        }
    }
    live
}

/// Fetch the signed-in Codex account weekly limit from ChatGPT, mirroring the
/// dashboard meter. Returns `None` when offline, when no token is available, or
/// on any network error (never fatal).
///
/// The usage endpoint sometimes over-counts the weekly meter for the same window
/// (seen: 99% while Codex enforced 49%). An over-count never under-reports, so
/// concurrent reads keep the lowest one, and a fresh session-log meter that the
/// live value far exceeds replaces it.
pub(crate) fn usage_limits(offline: bool) -> Option<CodexPlanSnapshot> {
    if offline {
        return None;
    }
    let token = access_token()?;
    let others = (1..LIVE_READS)
        .map(|_| {
            let token = token.clone();
            std::thread::spawn(move || fetch_usage_limits(&token))
        })
        .collect::<Vec<_>>();
    let first = fetch_usage_limits(&token);
    let live = others
        .into_iter()
        .map(|read| read.join().ok().flatten())
        .fold(first, lower_meter)?;
    Some(prefer_fresh_logs(
        live,
        super::plan::latest_logged_snapshot(),
        crate::utc_now(),
    ))
}

fn lower_meter(
    first: Option<CodexPlanSnapshot>,
    second: Option<CodexPlanSnapshot>,
) -> Option<CodexPlanSnapshot> {
    match (first, second) {
        (Some(first), Some(second)) => match (first.weekly_window(), second.weekly_window()) {
            (Some(a), Some(b)) if same_window(a, b) && b.used_percent < a.used_percent => {
                Some(second)
            }
            _ => Some(first),
        },
        (first, second) => first.or(second),
    }
}

fn prefer_fresh_logs(
    mut live: CodexPlanSnapshot,
    logged: Option<(Option<TimestampMs>, CodexPlanSnapshot)>,
    now: TimestampMs,
) -> CodexPlanSnapshot {
    let Some((Some(logged_at), logs)) = logged else {
        return live;
    };
    let (Some(live_week), Some(log_week)) = (live.weekly_window(), logs.weekly_window()) else {
        return live;
    };
    let fresh = now.duration_since(logged_at) <= LOG_TRUST_MS;
    if fresh
        && same_window(live_week, log_week)
        && looks_overcounted(live_week.used_percent, log_week.used_percent)
    {
        for window in [&mut live.primary, &mut live.secondary]
            .into_iter()
            .flatten()
        {
            if *window == live_week {
                window.used_percent = log_week.used_percent;
            }
        }
    }
    live
}

fn same_window(a: RateWindow, b: RateWindow) -> bool {
    a.window_minutes == b.window_minutes
        && matches!(
            (a.resets_at, b.resets_at),
            (Some(a), Some(b)) if (a - b).abs() <= SAME_WINDOW_SECONDS
        )
}

/// An over-count roughly doubles the meter; real usage never climbs that far
/// above the enforced value within `LOG_TRUST_MS`.
fn looks_overcounted(live: f64, logged: f64) -> bool {
    live > logged * 1.5 + 3.0
}

/// A ChatGPT sign-in is stored in `auth.json`; API-key sign-ins have no live limits.
pub(crate) fn has_sign_in() -> bool {
    access_token().is_some()
}

pub(super) fn access_token() -> Option<String> {
    for home in codex_home_paths().ok()? {
        let path = home.join("auth.json");
        if let Some(token) = fs::read_to_string(path)
            .ok()
            .as_deref()
            .and_then(access_token_from_auth)
        {
            return Some(token);
        }
    }
    None
}

fn access_token_from_auth(json: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(json.trim()).ok()?;
    value
        .get("tokens")?
        .get("access_token")?
        .as_str()
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

fn fetch_usage_limits(token: &str) -> Option<CodexPlanSnapshot> {
    parse_usage_limits(&fetch_body(USAGE_URL, token)?)
}

pub(super) fn fetch_body(url: &str, token: &str) -> Option<String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(FETCH_TIMEOUT_SECONDS)))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            concat!("agent-burn/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    response
        .body_mut()
        .with_config()
        .limit(FETCH_MAX_BYTES)
        .read_to_string()
        .ok()
}

pub(super) fn parse_usage_limits(body: &str) -> Option<CodexPlanSnapshot> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let plan_type = value
        .get("plan_type")
        .and_then(Value::as_str)
        .filter(|plan| !plan.is_empty())
        .unwrap_or("unknown")
        .to_string();
    let rate_limit = value.get("rate_limit")?;
    Some(CodexPlanSnapshot {
        plan_type,
        limit_id: Some("codex".to_string()),
        primary: rate_window(rate_limit.get("primary_window")),
        secondary: rate_window(rate_limit.get("secondary_window")),
        reset_credits_available: reset_credits_available(&value),
    })
}

fn reset_credits_available(value: &Value) -> Option<u32> {
    let credits = value
        .get("rate_limit_reset_credits")
        .or_else(|| value.get("rateLimitResetCredits"))?;
    let count = credits
        .get("available_count")
        .or_else(|| credits.get("availableCount"))?;
    u32::try_from(count.as_u64()?).ok()
}

fn rate_window(value: Option<&Value>) -> Option<RateWindow> {
    let window = value.filter(|window| !window.is_null())?;
    let seconds = window.get("limit_window_seconds")?.as_u64()?;
    Some(RateWindow {
        used_percent: window.get("used_percent")?.as_f64()?,
        window_minutes: seconds / 60,
        resets_at: window.get("reset_at").and_then(Value::as_i64),
    })
}

#[cfg(test)]
mod tests {
    use agent_burn_test_support::{EnvVarGuard, Fixture};

    use super::*;

    const LIVE_BODY: &str = r#"{
      "plan_type": "pro",
      "rate_limit": {
        "primary_window": {
          "used_percent": 85,
          "limit_window_seconds": 604800,
          "reset_at": 1788954006
        },
        "secondary_window": null
      },
      "additional_rate_limits": [
        {
          "limit_name": "GPT-5.3-Codex-Spark",
          "rate_limit": {
            "primary_window": {"used_percent": 0, "limit_window_seconds": 18000, "reset_at": 1},
            "secondary_window": {"used_percent": 100, "limit_window_seconds": 604800, "reset_at": 2}
          }
        }
      ]
    }"#;

    #[test]
    fn reads_banked_reset_credits_from_snake_and_camel_case() {
        let snake = parse_usage_limits(
            r#"{
              "plan_type": "plus",
              "rate_limit": {
                "primary_window": {
                  "used_percent": 10,
                  "limit_window_seconds": 604800,
                  "reset_at": 1
                }
              },
              "rate_limit_reset_credits": { "available_count": 2 }
            }"#,
        )
        .unwrap();
        assert_eq!(snake.reset_credits_available, Some(2));

        let camel = parse_usage_limits(
            r#"{
              "plan_type": "plus",
              "rate_limit": {
                "primary_window": {
                  "used_percent": 10,
                  "limit_window_seconds": 604800,
                  "reset_at": 1
                }
              },
              "rateLimitResetCredits": { "availableCount": 3 }
            }"#,
        )
        .unwrap();
        assert_eq!(camel.reset_credits_available, Some(3));
    }

    #[test]
    fn reads_account_weekly_used_percent_and_ignores_spark() {
        let snapshot = parse_usage_limits(LIVE_BODY).unwrap();

        assert_eq!(snapshot.plan_type, "pro");
        assert_eq!(snapshot.limit_id.as_deref(), Some("codex"));
        assert_eq!(
            snapshot.weekly_window(),
            Some(RateWindow {
                used_percent: 85.0,
                window_minutes: 10080,
                resets_at: Some(1788954006),
            })
        );
        assert!(snapshot.short_window().is_none());
    }

    fn weekly(used_percent: f64, resets_at: i64) -> CodexPlanSnapshot {
        CodexPlanSnapshot {
            plan_type: "pro".to_string(),
            limit_id: Some("codex".to_string()),
            primary: Some(RateWindow {
                used_percent,
                window_minutes: 10080,
                resets_at: Some(resets_at),
            }),
            secondary: None,
            reset_credits_available: None,
        }
    }

    fn weekly_used(snapshot: &CodexPlanSnapshot) -> f64 {
        snapshot.weekly_window().unwrap().used_percent
    }

    #[test]
    fn keeps_the_lower_of_two_reads_for_the_same_window() {
        let kept = lower_meter(Some(weekly(99.0, 1_000)), Some(weekly(49.0, 1_010))).unwrap();
        assert_eq!(weekly_used(&kept), 49.0);

        let kept = lower_meter(Some(weekly(49.0, 1_000)), Some(weekly(99.0, 1_000))).unwrap();
        assert_eq!(weekly_used(&kept), 49.0);

        let kept = lower_meter(None, Some(weekly(99.0, 1_000))).unwrap();
        assert_eq!(weekly_used(&kept), 99.0);
    }

    #[test]
    fn keeps_the_first_read_when_the_window_changed_between_reads() {
        let kept = lower_meter(Some(weekly(80.0, 1_000)), Some(weekly(2.0, 605_800))).unwrap();
        assert_eq!(weekly_used(&kept), 80.0);
    }

    #[test]
    fn replaces_an_overcount_with_a_fresh_log_meter() {
        let now = crate::parse_ts_timestamp("2026-09-30T04:10:00Z").unwrap();
        let logged_at = crate::parse_ts_timestamp("2026-09-30T04:05:00Z");
        let healed = prefer_fresh_logs(
            weekly(99.0, 1_000),
            Some((logged_at, weekly(49.0, 1_002))),
            now,
        );
        assert_eq!(weekly_used(&healed), 49.0);
    }

    #[test]
    fn keeps_the_live_meter_when_logs_cannot_contradict_it() {
        let now = crate::parse_ts_timestamp("2026-09-30T04:10:00Z").unwrap();
        let fresh = crate::parse_ts_timestamp("2026-09-30T04:05:00Z");
        let stale = crate::parse_ts_timestamp("2026-09-30T03:00:00Z");
        let cases = [
            (weekly(99.0, 1_000), Some((stale, weekly(49.0, 1_000)))),
            (weekly(52.0, 1_000), Some((fresh, weekly(49.0, 1_000)))),
            (weekly(99.0, 1_000), Some((fresh, weekly(49.0, 700_000)))),
            (weekly(99.0, 1_000), Some((None, weekly(49.0, 1_000)))),
            (weekly(99.0, 1_000), None),
        ];
        for (live, logged) in cases {
            let expected = weekly_used(&live);
            assert_eq!(weekly_used(&prefer_fresh_logs(live, logged, now)), expected);
        }
    }

    #[test]
    fn reads_access_token_from_auth_json() {
        let json = r#"{"tokens":{"access_token":"tok-live","refresh_token":"r"}}"#;
        assert_eq!(access_token_from_auth(json).as_deref(), Some("tok-live"));
    }

    #[test]
    fn chatgpt_tokens_in_auth_json_are_a_sign_in() {
        let fixture = Fixture::new();
        let _ = fixture.write_file(
            "auth.json",
            r#"{"tokens":{"access_token":"tok-live","refresh_token":"r"}}"#,
        );
        let _home = EnvVarGuard::set("CODEX_HOME", fixture.root());

        assert!(has_sign_in());
    }

    #[test]
    fn api_key_auth_json_is_no_sign_in() {
        let fixture = Fixture::new();
        let _ = fixture.write_file("auth.json", r#"{"OPENAI_API_KEY":"sk-test","tokens":null}"#);
        let _home = EnvVarGuard::set("CODEX_HOME", fixture.root());

        assert!(!has_sign_in());
    }

    #[test]
    fn missing_auth_json_is_no_sign_in() {
        let fixture = Fixture::new();
        let _home = EnvVarGuard::set("CODEX_HOME", fixture.root());

        assert!(!has_sign_in());
    }

    #[test]
    fn ignores_empty_access_token() {
        assert_eq!(
            access_token_from_auth(r#"{"tokens":{"access_token":""}}"#),
            None
        );
    }
}
