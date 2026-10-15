//! Engine reports, as `summary --value --breakdown --json` and
//! `harness <claude|codex> --value --json` print them.
//!
//! A value the engine may omit or print as `null` is an `Option`, so a missing
//! value is never read as zero. Sums the engine always prints (costs, tokens)
//! are required: their absence fails decoding instead of showing zero. Ratios
//! are optional because the engine prints a non-finite ratio as `null`.
//! Unknown fields are ignored, so a newer engine still decodes.
//!
//! Units: costs and amounts in US dollars; percentages 0–100 (a projection may
//! exceed 100); `*Ms` fields in milliseconds since the Unix epoch; `date` and
//! `weekStart` as `YYYY-MM-DD` in the report's timezone; window lengths in
//! minutes. See `tests/fixtures/README.md`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SummaryReport {
    /// Zero when no agent is detected: check `agents` before showing totals.
    pub totals: Totals,
    #[serde(default)]
    pub agents: Vec<AgentUsage>,
    #[serde(default)]
    pub models: Vec<ModelUsage>,
    #[serde(default)]
    pub daily: Vec<DailyUsage>,
    /// Present with `--value`.
    pub subscription: Option<Subscription>,
    pub claude_account: Option<ClaudeAccount>,
    pub claude_account_status: Option<AccountStatus>,
    pub codex_account_status: Option<AccountStatus>,
    pub cursor_account: Option<CursorAccount>,
    pub cursor_account_status: Option<AccountStatus>,
}

/// Why an installed agent's limits cannot load. Never set offline; absence
/// means no sign-in problem was seen, not that the limits are live.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AccountStatus {
    SignedOut,
    SignInExpired,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub total_cost: f64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsage {
    pub agent: String,
    pub total_cost: f64,
    pub total_tokens: u64,
    #[serde(default)]
    pub models: Vec<ModelUsage>,
    #[serde(default)]
    pub daily: Vec<DailyUsage>,
    pub token_breakdown: Option<TokenBreakdown>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub total_cost: f64,
    pub total_tokens: u64,
    /// Share of the enclosing cost, 0–100.
    pub percentage: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    pub date: String,
    pub cost: f64,
    /// Absent from harness rows.
    pub tokens: Option<u64>,
    /// Cursor rows only: Composer, Auto and other Cursor-hosted models.
    pub cursor_models_cost: Option<f64>,
    pub cursor_models_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TokenBreakdown {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    /// Null when no usage was found.
    pub period: Option<Period>,
    #[serde(default)]
    pub agents: Vec<SubscriptionAgent>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Period {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionAgent {
    pub agent: String,
    pub plan: Option<String>,
    pub price_per_month: Option<f64>,
    /// API-equivalent cost over the report period, not a bill.
    pub period_usage: f64,
    /// An account meter is known (live, Claude Code's cache or Codex session
    /// logs); not a promise that it was read just now.
    pub live_limits: bool,
    pub window: Option<SubscriptionWindow>,
    pub estimate: Option<QuotaEstimate>,
    pub short_window: Option<ShortWindow>,
    pub reset_credits_available: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionWindow {
    /// `weekly`, `5h`, ….
    pub label: String,
    pub used_percent: Option<f64>,
    pub elapsed_minutes: Option<f64>,
    pub api_equivalent_spent: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShortWindow {
    pub label: String,
    pub used_percent: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuotaEstimate {
    pub full_quota_value: Option<f64>,
    pub monthly_value: Option<f64>,
    /// May exceed 100.
    pub projected_use_percent: Option<f64>,
    pub value_multiple: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAccount {
    pub session_used_percent: Option<f64>,
    pub session_resets_at_ms: Option<i64>,
    pub weekly_used_percent: Option<f64>,
    pub weekly_resets_at_ms: Option<i64>,
    #[serde(default)]
    pub scoped: Vec<ScopedWindow>,
    pub extra_enabled: Option<bool>,
    #[serde(rename = "extraUsedUSD")]
    pub extra_used_usd: Option<f64>,
    #[serde(rename = "extraLimitUSD")]
    pub extra_limit_usd: Option<f64>,
    pub extra_used_percent: Option<f64>,
    /// When Anthropic reported these meters.
    pub observed_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScopedWindow {
    pub name: String,
    pub used_percent: Option<f64>,
    pub resets_at_ms: Option<i64>,
}

/// Cursor balances for the current billing cycle, whatever period the report
/// covers. Distinct from API-equivalent costs.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CursorAccount {
    pub billing_cycle_start_ms: Option<i64>,
    pub billing_cycle_end_ms: Option<i64>,
    #[serde(rename = "includedLimitUSD")]
    pub included_limit_usd: Option<f64>,
    #[serde(rename = "includedRemainingUSD")]
    pub included_remaining_usd: Option<f64>,
    pub included_percent_used: Option<f64>,
    #[serde(rename = "includedSpendUSD")]
    pub included_spend_usd: Option<f64>,
    #[serde(rename = "bonusSpendUSD")]
    pub bonus_spend_usd: Option<f64>,
    #[serde(rename = "planSpendUSD")]
    pub plan_spend_usd: Option<f64>,
    #[serde(rename = "onDemandSpentUSD")]
    pub on_demand_spent_usd: Option<f64>,
    #[serde(rename = "onDemandLimitUSD")]
    pub on_demand_limit_usd: Option<f64>,
    /// Promotional credits while they last, then the included allowance.
    #[serde(rename = "activeRemainingUSD")]
    pub active_remaining_usd: Option<f64>,
    #[serde(rename = "activeLimitUSD")]
    pub active_limit_usd: Option<f64>,
    pub active_percent_used: Option<f64>,
    #[serde(default)]
    pub grants: Vec<CursorGrant>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CursorGrant {
    pub kind: Option<String>,
    #[serde(rename = "totalUSD")]
    pub total_usd: Option<f64>,
    #[serde(rename = "remainingUSD")]
    pub remaining_usd: Option<f64>,
    pub expires_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HarnessReport {
    pub agent: String,
    pub plan: Option<String>,
    pub price_per_month: Option<f64>,
    pub api_equivalent_per_month: f64,
    /// See [`SubscriptionAgent::live_limits`].
    pub live_limits: bool,
    /// The weekly window; it carries no reset time or observation time.
    pub window: Option<HarnessWindow>,
    pub estimate: Option<QuotaEstimate>,
    pub economics: Option<Economics>,
    /// Days of the current window; empty without one.
    #[serde(default)]
    pub daily: Vec<DailyUsage>,
    #[serde(default)]
    pub top_models: Vec<HarnessModel>,
    #[serde(default)]
    pub spend_mix: Vec<SpendCategory>,
    #[serde(default)]
    pub weekly_trend: Vec<WeeklyCost>,
    pub image_generations: Option<ImageGenerations>,
    pub reset_credits_available: Option<u32>,
    /// Codex only, from the live ChatGPT dashboard.
    #[serde(default)]
    pub limit_usage_daily: Vec<LimitUsageDay>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HarnessWindow {
    pub window_minutes: u64,
    pub used_percent: Option<f64>,
    pub elapsed_percent: Option<f64>,
    pub api_equivalent_spent: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Economics {
    pub price_per_month: f64,
    pub api_equivalent_per_month: f64,
    /// Negative when the plan costs more than the usage is worth.
    pub subsidy_per_month: f64,
    pub value_multiple: Option<f64>,
    pub discount_percent: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct HarnessModel {
    pub model: String,
    pub cost: f64,
    pub tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpendCategory {
    pub key: String,
    pub label: String,
    pub tokens: u64,
    pub token_percent: Option<f64>,
    #[serde(rename = "costUSD")]
    pub cost_usd: f64,
    pub cost_percent: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyCost {
    pub week_start: String,
    pub cost: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImageGenerations {
    pub count: u64,
    pub price_per_image_estimate: f64,
    pub estimated_cost: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LimitUsageDay {
    pub date: String,
    pub used_percent: Option<f64>,
    #[serde(default)]
    pub surfaces: Vec<SurfaceUsage>,
    #[serde(default)]
    pub models: Vec<LimitModelUsage>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceUsage {
    pub surface: String,
    pub used_percent: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LimitModelUsage {
    pub model: String,
    pub speed: Option<String>,
    pub used_percent: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_ratio_stays_unknown() {
        let model: ModelUsage = serde_json::from_str(
            r#"{"model":"m","totalCost":0,"totalTokens":0,"percentage":null}"#,
        )
        .unwrap();

        assert_eq!(model.percentage, None);
    }

    #[test]
    fn a_missing_cost_fails_instead_of_reading_zero() {
        let error = serde_json::from_str::<Totals>(r#"{"totalTokens":10}"#).unwrap_err();

        assert!(error.to_string().contains("totalCost"));
    }

    #[test]
    fn an_unknown_account_status_still_decodes() {
        let status: AccountStatus = serde_json::from_str(r#""rateLimited""#).unwrap();

        assert_eq!(status, AccountStatus::Unknown);
    }
}
