//! Every captured engine output decodes, and each scenario keeps the states
//! the dashboard relies on. See `tests/fixtures/README.md`.

use std::{fs, path::PathBuf};

use agent_burn_windows::models::{AccountStatus, HarnessReport, SummaryReport};
use serde_json::Value;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn manifest() -> Value {
    serde_json::from_str(&fs::read_to_string(fixtures().join("manifest.json")).unwrap()).unwrap()
}

fn summary(scenario: &str) -> SummaryReport {
    let path = fixtures().join(scenario).join("summary.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn harness(scenario: &str, agent: &str) -> HarnessReport {
    let path = fixtures()
        .join(scenario)
        .join(format!("harness-{agent}.json"));
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn every_captured_report_decodes() {
    let manifest = manifest();
    let runs = manifest["runs"].as_array().unwrap();
    let mut decoded = 0;
    for run in runs {
        let Some(stdout) = run["stdout"].as_str() else {
            continue;
        };
        let text = fs::read_to_string(fixtures().join(stdout)).unwrap();
        let result = match run["command"].as_str().unwrap() {
            "summary" => serde_json::from_str::<SummaryReport>(&text).map(drop),
            _ => serde_json::from_str::<HarnessReport>(&text).map(drop),
        };
        assert!(result.is_ok(), "{stdout}: {result:?}");
        decoded += 1;
    }
    assert_eq!(decoded, 19);
}

/// Reports captured from real accounts decode too; they stay outside the
/// repository. `AGENT_BURN_REAL_REPORTS=<dir> cargo test -- --ignored`, with
/// `summary.json`, `harness-claude.json` and `harness-codex.json` in `<dir>`.
#[test]
#[ignore = "needs AGENT_BURN_REAL_REPORTS"]
fn real_reports_decode() {
    let dir = PathBuf::from(std::env::var_os("AGENT_BURN_REAL_REPORTS").unwrap());
    let read = |name: &str| fs::read_to_string(dir.join(name)).unwrap();

    serde_json::from_str::<SummaryReport>(&read("summary.json")).unwrap();
    serde_json::from_str::<HarnessReport>(&read("harness-claude.json")).unwrap();
    serde_json::from_str::<HarnessReport>(&read("harness-codex.json")).unwrap();
}

#[test]
fn failed_runs_print_no_report_and_explain_why() {
    let manifest = manifest();
    let failures: Vec<&Value> = manifest["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|run| run["exitCode"] != 0)
        .collect();

    assert_eq!(failures.len(), 3);
    for run in failures {
        assert!(run["stdout"].is_null());
        let stderr = fs::read_to_string(fixtures().join(run["stderr"].as_str().unwrap())).unwrap();
        assert!(!stderr.trim().is_empty());
    }
}

#[test]
fn no_source_means_no_agent_not_zero_usage() {
    let report = summary("no-source");

    assert!(report.agents.is_empty());
    assert_eq!(report.subscription.unwrap().period, None);
}

#[test]
fn installed_agents_without_sign_in_are_named() {
    let report = summary("no-quota");

    assert_eq!(report.claude_account_status, Some(AccountStatus::SignedOut));
    assert_eq!(report.codex_account_status, Some(AccountStatus::SignedOut));
    assert_eq!(report.cursor_account_status, None);
    assert!(report.claude_account.is_none());
}

#[test]
fn offline_reports_carry_no_sign_in_state() {
    let report = summary("no-quota-offline");

    assert_eq!(report.claude_account_status, None);
    assert_eq!(report.codex_account_status, None);
}

#[test]
fn a_partial_report_keeps_each_source_apart() {
    let report = summary("partial");

    assert_eq!(
        report.claude_account_status,
        Some(AccountStatus::SignInExpired)
    );
    assert_eq!(report.cursor_account_status, Some(AccountStatus::SignedOut));
    let subscription = report.subscription.unwrap();
    let claude = subscription
        .agents
        .iter()
        .find(|agent| agent.agent == "claude")
        .unwrap();
    let codex = subscription
        .agents
        .iter()
        .find(|agent| agent.agent == "codex")
        .unwrap();
    assert!(claude.window.is_none());
    assert!(codex.window.is_some());
}

#[test]
fn a_harness_without_quota_has_no_window() {
    let report = harness("no-quota", "claude");

    assert!(!report.live_limits);
    assert!(report.window.is_none());
    assert!(report.daily.is_empty());
}

#[test]
fn quota_windows_keep_their_units() {
    let claude = harness("quotas", "claude");
    let account = summary("quotas").claude_account.unwrap();

    let window = claude.window.unwrap();
    assert_eq!(window.window_minutes, 10_080);
    assert_eq!(window.used_percent, Some(58.0));
    assert_eq!(account.weekly_used_percent, Some(58.0));
    assert!(account.weekly_resets_at_ms.unwrap() > account.observed_at_ms.unwrap());
    assert_eq!(account.extra_used_usd, Some(12.5));
}

#[test]
fn absent_cursor_amounts_stay_unknown() {
    let report = summary("live-accounts");

    let account = report.cursor_account.unwrap();
    assert_eq!(account.on_demand_limit_usd, None);
    assert_eq!(account.on_demand_spent_usd, None);
    assert_eq!(account.included_limit_usd, Some(20.0));
    let cursor = report
        .agents
        .iter()
        .find(|agent| agent.agent == "cursor")
        .unwrap();
    assert!(
        cursor
            .daily
            .iter()
            .all(|day| day.cursor_models_cost.is_some())
    );
}

#[test]
fn codex_dashboard_history_decodes() {
    let report = harness("live-accounts", "codex");

    assert_eq!(report.reset_credits_available, Some(2));
    assert_eq!(report.limit_usage_daily.len(), 2);
    assert_eq!(report.limit_usage_daily[1].models[0].speed, None);
}
