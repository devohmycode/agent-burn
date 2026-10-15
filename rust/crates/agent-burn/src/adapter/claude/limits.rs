use std::{env, time::Duration};

use serde_json::{Value, json};

use super::{
    claude_code,
    credentials::{Credentials, OAuthToken},
    oauth_refresh,
    usage_cache::{self, UsageCache},
};
use crate::{TimestampMs, parse_ts_timestamp, utc_now};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const FETCH_TIMEOUT_SECONDS: u64 = 5;
const FETCH_MAX_BYTES: u64 = 1_000_000;

/// A live Claude rate-limit window from the OAuth usage endpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct UsageWindow {
    pub(crate) utilization: f64,
    pub(crate) resets_at: Option<TimestampMs>,
}

/// A model-scoped weekly window (Fable, Opus, Sonnet) from the modern `limits` array.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScopedWindow {
    pub(crate) name: String,
    pub(crate) utilization: f64,
    pub(crate) resets_at: Option<TimestampMs>,
}

/// Extra-usage / spend credits reported beside the rate-limit windows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ExtraUsage {
    pub(crate) enabled: bool,
    pub(crate) used_usd: Option<f64>,
    pub(crate) limit_usd: Option<f64>,
    pub(crate) used_percent: Option<f64>,
}

/// Live Claude subscription meters from the OAuth usage endpoint.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ClaudeUsageLimits {
    pub(crate) five_hour: Option<UsageWindow>,
    pub(crate) seven_day: Option<UsageWindow>,
    pub(crate) scoped: Vec<ScopedWindow>,
    pub(crate) extra_usage: Option<ExtraUsage>,
}

/// The signed-in account's usage limits from Anthropic, mirroring the call
/// Claude Code's status line makes. Reports accept a shared reading up to
/// 15 minutes old while the endpoint is rate limited. Returns `None` when
/// offline, when no OAuth token is available, or when nothing usable is cached.
pub(crate) fn usage_limits(offline: bool) -> Option<ClaudeUsageLimits> {
    usage_body(offline, usage_cache::STALE_MS)
        .body
        .and_then(|(body, _)| parse_usage_limits(&body))
}

/// Limits for the quota collector, stamped with when Anthropic reported them
/// so a shared cached reading is never recorded as a newer observation.
pub(crate) fn live_usage_limits(offline: bool) -> Option<(ClaudeUsageLimits, TimestampMs)> {
    let (body, observed_at) = usage_body(offline, usage_cache::LIVE_MS).body?;
    Some((parse_usage_limits(&body)?, observed_at))
}

/// A usage body with when Anthropic reported it, whether Claude Code's
/// sign-in lapsed and Anthropic refused to renew it, and whether no sign-in
/// was stored when one was needed.
#[derive(Debug, Default)]
struct Usage {
    body: Option<(String, TimestampMs)>,
    sign_in_expired: bool,
    signed_out: bool,
}

fn usage_body(offline: bool, max_age_ms: i64) -> Usage {
    if offline {
        return Usage::default();
    }
    resolve_usage(
        &mut UsageCache::load(),
        Request {
            now: utc_now(),
            max_age_ms,
            force: env::var("AGENT_BURN_FORCE_REFRESH").as_deref() == Ok("1"),
        },
        Sources {
            token: || Credentials::load()?.token(),
            fetch: fetch_usage_body,
            claude_code: claude_code::cached_usage,
            renew: oauth_refresh::renew,
        },
    )
}

#[derive(Clone, Copy)]
struct Request {
    now: TimestampMs,
    max_age_ms: i64,
    /// A hard refresh: skip the shared fresh reading and ask Anthropic again.
    force: bool,
}

struct Sources<T, F, C, R> {
    token: T,
    fetch: F,
    /// The reading Claude Code cached for itself, which needs no request.
    claude_code: C,
    /// Renews the sign-in through Claude Code's refresh protocol; `true`
    /// renews a token Anthropic rejected before its expiry.
    renew: R,
}

/// Every source is tried before a report goes without meters: the shared
/// reading, Claude Code's own cache, then the OAuth endpoint with a sign-in
/// renewed when it expired or was rejected. The newest reading always wins.
fn resolve_usage<T, F, C, R>(
    cache: &mut UsageCache,
    Request {
        now,
        max_age_ms,
        force,
    }: Request,
    Sources {
        token,
        mut fetch,
        claude_code,
        mut renew,
    }: Sources<T, F, C, R>,
) -> Usage
where
    T: FnOnce() -> Option<OAuthToken>,
    F: FnMut(&str) -> Fetch,
    C: Fn() -> Option<(String, TimestampMs)>,
    R: FnMut(&OAuthToken, bool) -> Option<OAuthToken>,
{
    let cached = |cache: &UsageCache, max_age_ms| {
        cache
            .body_within(now, max_age_ms)
            .map(|(body, fetched_at)| (body.to_string(), fetched_at))
    };
    cache.adopt(claude_code(), now);
    if !force && let Some(body) = cached(cache, usage_cache::FRESH_MS) {
        return Usage {
            body: Some(body),
            ..Usage::default()
        };
    }
    let mut sign_in_expired = false;
    let token = token();
    let signed_out = token.is_none();
    if let Some(token) = token
        && !cache.is_blocked(now)
    {
        let mut renew = |cache: &mut UsageCache, token: &OAuthToken, rejected: bool| {
            if !cache.renew_due(now, force) {
                return None;
            }
            let renewed = renew(token, rejected);
            cache.store_renewal(renewed.is_some(), now);
            renewed
        };
        // Anthropic answers an expired token with a 429 and an hour-long
        // Retry-After, so an expired token is renewed and never sent.
        let token = if token.is_expired(now) {
            renew(cache, &token, false)
        } else {
            Some(token)
        };
        let mut attempt = token.map(|token| (fetch(&token.access_token), token));
        if let Some((Fetch::Unauthorized, token)) = &attempt {
            attempt = renew(cache, token, true).map(|token| (fetch(&token.access_token), token));
        }
        match attempt.map(|(fetch, _)| fetch) {
            Some(Fetch::Body(body)) => {
                cache.store_body(body.clone(), now);
                return Usage {
                    body: Some((body, now)),
                    ..Usage::default()
                };
            }
            Some(Fetch::RateLimited(retry_after)) => cache.store_rate_limit(retry_after, now),
            Some(Fetch::Failed) => {}
            Some(Fetch::Unauthorized) | None => sign_in_expired = true,
        }
    }
    Usage {
        body: cached(cache, max_age_ms),
        sign_in_expired,
        signed_out,
    }
}

/// Claude meters for `summary --value --json`.
pub(crate) struct AccountLoad {
    /// Latest meters, omitted when offline or when nothing usable is cached.
    pub(crate) account: Option<Value>,
    /// Claude Code's sign-in expired and Anthropic refused to renew it.
    pub(crate) sign_in_expired: bool,
    /// No Claude Code sign-in was stored when the meters needed one; never
    /// set offline or while a fresh shared reading serves the meters.
    pub(crate) signed_out: bool,
}

pub(crate) fn load_account(offline: bool) -> AccountLoad {
    let usage = usage_body(offline, usage_cache::STALE_MS);
    let account = usage.body.and_then(|(body, observed_at)| {
        let limits = parse_usage_limits(&body)?;
        if limits == ClaudeUsageLimits::default() {
            return None;
        }
        let mut account = account_json(&limits);
        account["observedAtMs"] = json!(observed_at.as_millis());
        Some(account)
    });
    AccountLoad {
        account,
        sign_in_expired: usage.sign_in_expired,
        signed_out: usage.signed_out,
    }
}

enum Fetch {
    Body(String),
    RateLimited(Option<i64>),
    Unauthorized,
    Failed,
}

fn fetch_usage_body(token: &str) -> Fetch {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(FETCH_TIMEOUT_SECONDS)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let Ok(mut response) = agent
        .get(USAGE_URL)
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            concat!("claude-code/", env!("CARGO_PKG_VERSION")),
        )
        .call()
    else {
        return Fetch::Failed;
    };
    match response.status().as_u16() {
        200 => {}
        401 => return Fetch::Unauthorized,
        429 => {
            return Fetch::RateLimited(
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse().ok()),
            );
        }
        _ => return Fetch::Failed,
    }
    match response
        .body_mut()
        .with_config()
        .limit(FETCH_MAX_BYTES)
        .read_to_string()
    {
        Ok(body) if parse_usage_limits(&body).is_some() => Fetch::Body(body),
        _ => Fetch::Failed,
    }
}

fn parse_usage_limits(body: &str) -> Option<ClaudeUsageLimits> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let mut limits = ClaudeUsageLimits {
        five_hour: parse_window(value.get("five_hour")),
        seven_day: parse_window(value.get("seven_day")),
        scoped: Vec::new(),
        extra_usage: parse_extra_usage(&value),
    };
    apply_limits_array(&mut limits, value.get("limits"));
    Some(limits)
}

fn account_json(limits: &ClaudeUsageLimits) -> Value {
    json!({
        "sessionUsedPercent": limits.five_hour.map(|window| window.utilization),
        "sessionResetsAtMs": limits
            .five_hour
            .and_then(|window| window.resets_at)
            .map(TimestampMs::as_millis),
        "weeklyUsedPercent": limits.seven_day.map(|window| window.utilization),
        "weeklyResetsAtMs": limits
            .seven_day
            .and_then(|window| window.resets_at)
            .map(TimestampMs::as_millis),
        "scoped": limits
            .scoped
            .iter()
            .map(|window| {
                json!({
                    "name": window.name,
                    "usedPercent": window.utilization,
                    "resetsAtMs": window.resets_at.map(TimestampMs::as_millis),
                })
            })
            .collect::<Vec<_>>(),
        "extraEnabled": limits.extra_usage.as_ref().map(|extra| extra.enabled),
        "extraUsedUSD": limits.extra_usage.and_then(|extra| extra.used_usd),
        "extraLimitUSD": limits.extra_usage.and_then(|extra| extra.limit_usd),
        "extraUsedPercent": limits.extra_usage.and_then(|extra| extra.used_percent),
    })
}

fn apply_limits_array(limits: &mut ClaudeUsageLimits, value: Option<&Value>) {
    let Some(entries) = value.and_then(Value::as_array) else {
        return;
    };
    for entry in entries {
        let Some(kind) = entry.get("kind").and_then(Value::as_str) else {
            continue;
        };
        let Some(window) = parse_limit_entry(entry) else {
            continue;
        };
        match kind {
            "session" if limits.five_hour.is_none() => limits.five_hour = Some(window),
            "weekly_all" if limits.seven_day.is_none() => limits.seven_day = Some(window),
            "weekly_scoped" => {
                if let Some(name) = scoped_name(entry) {
                    limits.scoped.push(ScopedWindow {
                        name,
                        utilization: window.utilization,
                        resets_at: window.resets_at,
                    });
                }
            }
            _ => {}
        }
    }
}

fn parse_limit_entry(entry: &Value) -> Option<UsageWindow> {
    let utilization = entry
        .get("percent")
        .and_then(json_percent)
        .or_else(|| entry.get("utilization").and_then(json_percent))?;
    Some(UsageWindow {
        utilization,
        resets_at: entry
            .get("resets_at")
            .and_then(Value::as_str)
            .and_then(parse_reset_time),
    })
}

fn scoped_name(entry: &Value) -> Option<String> {
    let scope = entry.get("scope")?;
    scope
        .get("model")
        .and_then(|model| model.get("display_name").or_else(|| model.get("id")))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| {
            scope
                .as_str()
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
}

fn parse_window(value: Option<&Value>) -> Option<UsageWindow> {
    let window = value.filter(|window| !window.is_null())?;
    Some(UsageWindow {
        utilization: json_percent(window.get("utilization")?)?,
        resets_at: window
            .get("resets_at")
            .and_then(Value::as_str)
            .and_then(parse_reset_time),
    })
}

fn parse_extra_usage(value: &Value) -> Option<ExtraUsage> {
    if let Some(extra) = value.get("extra_usage").filter(|extra| !extra.is_null()) {
        let enabled = extra
            .get("is_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let used_usd = extra
            .get("used_credits")
            .and_then(Value::as_f64)
            .filter(|amount| amount.is_finite())
            .map(|amount| amount / 100.0);
        let limit_usd = extra
            .get("monthly_limit")
            .and_then(Value::as_f64)
            .filter(|amount| amount.is_finite() && *amount > 0.0)
            .map(|amount| amount / 100.0);
        let used_percent = extra.get("utilization").and_then(json_percent);
        if enabled || used_usd.is_some() || limit_usd.is_some() {
            return Some(ExtraUsage {
                enabled,
                used_usd,
                limit_usd,
                used_percent,
            });
        }
    }
    parse_spend(value.get("spend")?)
}

fn parse_spend(spend: &Value) -> Option<ExtraUsage> {
    if spend.is_null() {
        return None;
    }
    let enabled = spend
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let used_usd = money(spend.get("used"));
    let limit_usd = money(spend.get("limit"));
    let used_percent = spend.get("percent").and_then(json_percent);
    (enabled || used_usd.is_some() || limit_usd.is_some()).then_some(ExtraUsage {
        enabled,
        used_usd,
        limit_usd,
        used_percent,
    })
}

fn money(value: Option<&Value>) -> Option<f64> {
    let value = value.filter(|value| !value.is_null())?;
    if let Some(amount) = value.as_f64().filter(|amount| amount.is_finite()) {
        return Some(amount);
    }
    let amount = value
        .get("amount_minor")
        .and_then(Value::as_f64)
        .filter(|amount| amount.is_finite())?;
    let exponent = value.get("exponent").and_then(Value::as_i64).unwrap_or(2);
    Some(amount / 10f64.powi(i32::try_from(exponent).ok()?))
}

fn json_percent(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|percent| percent.is_finite())
        .map(|percent| percent.clamp(0.0, 100.0))
}

/// Parse an RFC 3339 reset time, tolerating the sub-millisecond precision the
/// usage endpoint emits (e.g. `…:59.434813+00:00`) which the strict shared
/// parser rejects. Sub-second digits are truncated to milliseconds.
fn parse_reset_time(value: &str) -> Option<TimestampMs> {
    if let Some(timestamp) = parse_ts_timestamp(value) {
        return Some(timestamp);
    }
    let dot = value.find('.')?;
    let after = &value[dot + 1..];
    let fractional_len = after.bytes().take_while(u8::is_ascii_digit).count();
    let millis: String = after[..fractional_len]
        .chars()
        .chain(std::iter::repeat('0'))
        .take(3)
        .collect();
    let normalized = format!("{}.{millis}{}", &value[..dot], &after[fractional_len..]);
    parse_ts_timestamp(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format_rfc3339_millis;

    #[test]
    fn parses_usage_windows_from_response_body() {
        let body = r#"{"five_hour":{"utilization":23,"resets_at":"2026-06-13T15:39:59.434793+00:00"},"seven_day":{"utilization":12,"resets_at":"2026-06-14T08:59:59.434813+00:00"}}"#;

        let limits = parse_usage_limits(body).unwrap();

        let seven_day = limits.seven_day.unwrap();
        assert_eq!(seven_day.utilization, 12.0);
        assert_eq!(
            format_rfc3339_millis(seven_day.resets_at.unwrap()),
            "2026-06-14T08:59:59.434Z"
        );
        assert_eq!(limits.five_hour.unwrap().utilization, 23.0);
    }

    #[test]
    fn treats_null_windows_as_absent() {
        let limits = parse_usage_limits(r#"{"five_hour":null,"seven_day":null}"#).unwrap();
        assert_eq!(limits, ClaudeUsageLimits::default());
    }

    #[test]
    fn reads_session_and_weekly_from_modern_limits_array() {
        let limits = parse_usage_limits(
            r#"{
              "five_hour": null,
              "seven_day": null,
              "limits": [
                {"kind":"session","percent":18,"resets_at":"2026-09-12T12:00:00Z"},
                {"kind":"weekly_all","percent":9,"resets_at":"2026-09-14T08:59:59.434813+00:00"},
                {"kind":"weekly_scoped","percent":15,"resets_at":"2026-09-14T08:59:59Z","scope":{"model":{"display_name":"Fable"}}}
              ]
            }"#,
        )
        .unwrap();

        let session = limits.five_hour.unwrap();
        assert_eq!(session.utilization, 18.0);
        assert_eq!(
            format_rfc3339_millis(session.resets_at.unwrap()),
            "2026-09-12T12:00:00.000Z"
        );

        let weekly = limits.seven_day.unwrap();
        assert_eq!(weekly.utilization, 9.0);
        assert_eq!(
            format_rfc3339_millis(weekly.resets_at.unwrap()),
            "2026-09-14T08:59:59.434Z"
        );
        assert_eq!(limits.scoped.len(), 1);
        assert_eq!(limits.scoped[0].name, "Fable");
        assert_eq!(limits.scoped[0].utilization, 15.0);
    }

    #[test]
    fn prefers_legacy_windows_when_both_shapes_are_present() {
        let limits = parse_usage_limits(
            r#"{
              "five_hour":{"utilization":23,"resets_at":"2026-06-13T15:39:59Z"},
              "seven_day":{"utilization":12,"resets_at":"2026-06-14T08:59:59Z"},
              "limits":[{"kind":"session","percent":99},{"kind":"weekly_all","percent":88}]
            }"#,
        )
        .unwrap();
        assert_eq!(limits.five_hour.unwrap().utilization, 23.0);
        assert_eq!(limits.seven_day.unwrap().utilization, 12.0);
    }

    #[test]
    fn reads_extra_usage_credits_in_cents() {
        let limits = parse_usage_limits(
            r#"{
              "five_hour":null,
              "seven_day":null,
              "extra_usage":{
                "is_enabled":true,
                "monthly_limit":1700,
                "used_credits":190.0,
                "utilization":11.18
              }
            }"#,
        )
        .unwrap();
        let extra = limits.extra_usage.unwrap();
        assert!(extra.enabled);
        assert_eq!(extra.used_usd, Some(1.9));
        assert_eq!(extra.limit_usd, Some(17.0));
        assert_eq!(extra.used_percent, Some(11.18));
    }

    #[test]
    fn reads_spend_object_as_extra_usage() {
        let limits = parse_usage_limits(
            r#"{
              "spend":{
                "enabled":true,
                "percent":4,
                "used":{"amount_minor":6123,"currency":"USD","exponent":2},
                "limit":{"amount_minor":150000,"currency":"USD","exponent":2}
              }
            }"#,
        )
        .unwrap();
        let extra = limits.extra_usage.unwrap();
        assert!(extra.enabled);
        assert_eq!(extra.used_usd, Some(61.23));
        assert_eq!(extra.limit_usd, Some(1500.0));
        assert_eq!(extra.used_percent, Some(4.0));
    }

    #[test]
    fn account_json_exposes_session_weekly_scoped_and_credits() {
        let limits = parse_usage_limits(
            r#"{
              "limits":[
                {"kind":"session","percent":18,"resets_at":"2026-09-12T12:00:00Z"},
                {"kind":"weekly_all","percent":9,"resets_at":"2026-09-14T08:59:59Z"},
                {"kind":"weekly_scoped","percent":15,"resets_at":"2026-09-14T08:59:59Z","scope":{"model":{"display_name":"Fable"}}}
              ],
              "extra_usage":{"is_enabled":true,"monthly_limit":100000,"used_credits":2500,"utilization":2.5}
            }"#,
        )
        .unwrap();
        let account = account_json(&limits);
        assert_eq!(account["sessionUsedPercent"], 18.0);
        assert_eq!(account["weeklyUsedPercent"], 9.0);
        assert_eq!(account["scoped"][0]["name"], "Fable");
        assert_eq!(account["scoped"][0]["usedPercent"], 15.0);
        assert_eq!(account["extraEnabled"], true);
        assert_eq!(account["extraUsedUSD"], 25.0);
        assert_eq!(account["extraLimitUSD"], 1000.0);
        assert_eq!(account["extraUsedPercent"], 2.5);
    }

    fn stale_cache(now: TimestampMs) -> UsageCache {
        UsageCache {
            fetched_at: now.checked_sub_millis(5 * 60_000),
            body: Some(r#"{"five_hour":null}"#.into()),
            ..UsageCache::default()
        }
    }

    fn signed_in(access_token: &str, expires_at: Option<TimestampMs>) -> OAuthToken {
        OAuthToken {
            access_token: access_token.into(),
            expires_at,
            refresh_token: Some("refresh".into()),
            scopes: Vec::new(),
            client_id: None,
        }
    }

    fn token(expires_at: Option<TimestampMs>) -> Option<OAuthToken> {
        Some(signed_in("sk-ant-oat", expires_at))
    }

    fn request(now: TimestampMs, max_age_ms: i64) -> Request {
        Request {
            now,
            max_age_ms,
            force: false,
        }
    }

    fn no_claude_code() -> Option<(String, TimestampMs)> {
        None
    }

    fn refused(_: &OAuthToken, _: bool) -> Option<OAuthToken> {
        None
    }

    fn no_renewal(_: &OAuthToken, _: bool) -> Option<OAuthToken> {
        panic!("the sign-in must not be renewed")
    }

    fn at(seconds: i64) -> TimestampMs {
        TimestampMs::from_unix_seconds(1_800_000_000 + seconds).unwrap()
    }

    const BODY: &str = r#"{"five_hour":{"utilization":7}}"#;

    #[test]
    fn renews_an_expired_sign_in_instead_of_sending_it() {
        let now = at(0);
        let mut cache = stale_cache(now);
        let mut sent = Vec::new();

        let usage = resolve_usage(
            &mut cache,
            request(now, usage_cache::LIVE_MS),
            Sources {
                token: || token(Some(now)),
                fetch: |access: &str| {
                    sent.push(access.to_string());
                    Fetch::Body(BODY.into())
                },
                claude_code: no_claude_code,
                renew: |stale: &OAuthToken, rejected: bool| {
                    assert!(!rejected);
                    assert_eq!(stale.access_token, "sk-ant-oat");
                    Some(signed_in("sk-ant-oat-new", at(28_800).into()))
                },
            },
        );

        assert_eq!(sent, ["sk-ant-oat-new"]);
        assert!(!usage.sign_in_expired);
        assert_eq!(usage.body, Some((BODY.into(), now)));
        assert_eq!(cache.renew_blocked_until, None);
    }

    #[test]
    fn refused_renewal_keeps_the_last_reading_and_backs_off() {
        let now = at(0);
        let mut cache = stale_cache(now);

        let usage = resolve_usage(
            &mut cache,
            request(now, usage_cache::STALE_MS),
            Sources {
                token: || token(Some(now)),
                fetch: |_: &str| -> Fetch { panic!("an expired token must not reach Anthropic") },
                claude_code: no_claude_code,
                renew: refused,
            },
        );

        assert!(usage.sign_in_expired);
        assert_eq!(usage.body.map(|(_, at)| at), cache.fetched_at);
        assert_eq!(cache.blocked_until, None);
        assert!(cache.renew_blocked_until.is_some());

        let usage = resolve_usage(
            &mut cache,
            request(at(60), usage_cache::STALE_MS),
            Sources {
                token: || token(Some(now)),
                fetch: |_: &str| -> Fetch { panic!("an expired token must not reach Anthropic") },
                claude_code: no_claude_code,
                renew: no_renewal,
            },
        );
        assert!(usage.sign_in_expired);
    }

    #[test]
    fn hard_refresh_retries_a_refused_renewal() {
        let now = at(0);
        let mut cache = UsageCache {
            renew_blocked_until: Some(at(600)),
            ..stale_cache(now)
        };

        let usage = resolve_usage(
            &mut cache,
            Request {
                force: true,
                ..request(now, usage_cache::LIVE_MS)
            },
            Sources {
                token: || token(Some(now)),
                fetch: |_: &str| Fetch::Body(BODY.into()),
                claude_code: no_claude_code,
                renew: |_: &OAuthToken, _| Some(signed_in("new", at(28_800).into())),
            },
        );

        assert_eq!(usage.body.map(|(_, at)| at), Some(now));
        assert_eq!(cache.renew_blocked_until, None);
    }

    #[test]
    fn rejected_token_is_renewed_once_and_retried() {
        let now = at(0);
        let mut cache = stale_cache(now);
        let mut sent = Vec::new();

        let usage = resolve_usage(
            &mut cache,
            request(now, usage_cache::LIVE_MS),
            Sources {
                token: || token(None),
                fetch: |access: &str| {
                    sent.push(access.to_string());
                    if access == "sk-ant-oat" {
                        Fetch::Unauthorized
                    } else {
                        Fetch::Body(BODY.into())
                    }
                },
                claude_code: no_claude_code,
                renew: |_: &OAuthToken, rejected: bool| {
                    assert!(rejected);
                    Some(signed_in("renewed", at(28_800).into()))
                },
            },
        );

        assert_eq!(sent, ["sk-ant-oat", "renewed"]);
        assert!(!usage.sign_in_expired);
        assert_eq!(usage.body.map(|(_, at)| at), Some(now));
    }

    #[test]
    fn rejected_token_without_renewal_reports_expired_sign_in() {
        let now = at(0);
        let mut cache = stale_cache(now);

        let usage = resolve_usage(
            &mut cache,
            request(now, usage_cache::STALE_MS),
            Sources {
                token: || token(None),
                fetch: |_: &str| Fetch::Unauthorized,
                claude_code: no_claude_code,
                renew: refused,
            },
        );

        assert!(usage.sign_in_expired);
        assert!(!usage.signed_out);
        assert!(usage.body.is_some());
        assert_eq!(cache.blocked_until, None);
    }

    #[test]
    fn missing_sign_in_reports_signed_out_and_keeps_the_last_reading() {
        let now = at(0);
        let mut cache = stale_cache(now);

        let usage = resolve_usage(
            &mut cache,
            request(now, usage_cache::STALE_MS),
            Sources {
                token: || -> Option<OAuthToken> { None },
                fetch: |_: &str| -> Fetch { panic!("no sign-in means no request") },
                claude_code: no_claude_code,
                renew: no_renewal,
            },
        );

        assert!(usage.signed_out);
        assert!(!usage.sign_in_expired);
        assert_eq!(usage.body.map(|(_, at)| at), cache.fetched_at);
    }

    #[test]
    fn rate_limit_holds_back_requests_and_renewals() {
        let now = at(0);
        let mut cache = UsageCache {
            blocked_until: now.checked_add_millis(60_000),
            ..stale_cache(now)
        };

        let usage = resolve_usage(
            &mut cache,
            request(now, usage_cache::STALE_MS),
            Sources {
                token: || token(Some(now)),
                fetch: |_: &str| -> Fetch { panic!("a rate-limited token must wait") },
                claude_code: no_claude_code,
                renew: no_renewal,
            },
        );

        assert!(!usage.sign_in_expired);
        assert_eq!(usage.body.map(|(_, at)| at), cache.fetched_at);
    }

    #[test]
    fn fresh_shared_reading_skips_the_keychain() {
        let now = at(0);
        let mut cache = UsageCache {
            fetched_at: now.checked_sub_millis(10_000),
            ..stale_cache(now)
        };

        let usage = resolve_usage(
            &mut cache,
            request(now, usage_cache::FRESH_MS),
            Sources {
                token: || -> Option<OAuthToken> {
                    panic!("a fresh shared reading must not read credentials")
                },
                fetch: |_: &str| -> Fetch { panic!("a fresh shared reading must not refetch") },
                claude_code: no_claude_code,
                renew: no_renewal,
            },
        );

        assert!(!usage.sign_in_expired);
        assert_eq!(usage.body.map(|(_, at)| at), cache.fetched_at);
    }

    #[test]
    fn fresh_claude_code_reading_replaces_a_request() {
        let now = at(0);
        let mut cache = stale_cache(now);

        let usage = resolve_usage(
            &mut cache,
            request(now, usage_cache::LIVE_MS),
            Sources {
                token: || -> Option<OAuthToken> { panic!("Claude Code already fetched") },
                fetch: |_: &str| -> Fetch { panic!("Claude Code already fetched") },
                claude_code: || Some((BODY.into(), at(-20))),
                renew: no_renewal,
            },
        );

        assert_eq!(usage.body.map(|(_, at)| at), Some(at(-20)));
        assert_eq!(cache.fetched_at, Some(at(-20)));
    }

    #[test]
    fn hard_refresh_skips_the_fresh_shared_reading() {
        let now = at(0);
        let mut cache = UsageCache {
            fetched_at: Some(at(-10)),
            ..stale_cache(now)
        };

        let usage = resolve_usage(
            &mut cache,
            Request {
                force: true,
                ..request(now, usage_cache::LIVE_MS)
            },
            Sources {
                token: || token(None),
                fetch: |_: &str| Fetch::Body(BODY.into()),
                claude_code: no_claude_code,
                renew: no_renewal,
            },
        );

        assert_eq!(usage.body.map(|(_, at)| at), Some(now));
    }
}
