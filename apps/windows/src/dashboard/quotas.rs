//! The quota cards: one per provider, with the windows the engine
//! knows, how much is used, when each resets and how fresh the reading is.
//! A card without a reading says why instead of drawing an empty meter.

use std::time::{Duration, SystemTime};

use eframe::egui;

use crate::{
    dashboard::{clock, format},
    forecast,
    models::{AccountStatus, CursorAccount, HarnessReport},
    quota::{self, QuotaHistory, QuotaReading},
    settings::Provider,
    state::Snapshot,
    ui::{
        components::{
            fluent::{card, semibold},
            gauge::{self, GaugeState},
        },
        theme::{caution, danger, menu_text, muted},
    },
};

#[derive(Debug, PartialEq)]
pub struct QuotaCard {
    pub provider: Provider,
    pub plan: Option<String>,
    pub status: CardStatus,
    pub meters: Vec<Meter>,
    pub notes: Vec<String>,
    pub freshness: Freshness,
}

#[derive(Debug, PartialEq)]
pub enum CardStatus {
    /// Meters read for this collection.
    Live,
    /// Codex meters from session logs: no ChatGPT sign-in to read them live.
    FromLogs,
    /// The sign-in expired; meters, if any, are the last reading.
    Stale,
    SignedOut,
    /// Live meters are not read offline.
    Offline,
    /// The agent is detected but no meter was reported.
    NoReading,
    NotDetected,
    Unavailable(String),
}

#[derive(Debug, PartialEq)]
pub struct Meter {
    pub label: String,
    pub used_percent: Option<f64>,
    /// Share of the window already past.
    pub elapsed_percent: Option<f64>,
    pub reset: Option<Reset>,
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reset {
    At(SystemTime),
    /// Derived from the window's elapsed share at collection time.
    About(SystemTime),
    Expires(SystemTime),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Freshness {
    /// When the provider reported the meters.
    Observed(SystemTime),
    /// When this collection read them.
    Collected(SystemTime),
    /// Logged by Codex during a session; the time is not reported.
    Logged,
    None,
}

/// A reading older than this is the last known one, never shown as live.
pub const LIVE_FOR: Duration = Duration::from_secs(10 * 60);

/// The cards, with the saved quota readings when they are newer than
/// the reports.
pub fn cards(snapshot: &Snapshot, history: Option<&QuotaHistory>) -> Vec<QuotaCard> {
    let mut cards = vec![claude(snapshot), codex(snapshot), cursor(snapshot)];
    if let Some(history) = history {
        for card in &mut cards {
            apply_readings(card, history);
        }
    }
    for card in &mut cards {
        let name = match card.provider {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Cursor => continue,
        };
        if let Some((_, at, error)) = snapshot
            .harness_saved
            .iter()
            .find(|(saved, _, _)| *saved == name)
        {
            card.notes.push(format!(
                "Not updated ({error}); these are the meters read {}.",
                clock::local_date_time(*at)
            ));
        }
    }
    cards
}

/// The newest consistent reading of `series`.
fn latest(history: &QuotaHistory, series: &str) -> Option<QuotaReading> {
    forecast::current(history.readings.get(series)?).cloned()
}

fn observed(freshness: Freshness) -> Option<SystemTime> {
    match freshness {
        Freshness::Observed(at) | Freshness::Collected(at) => Some(at),
        Freshness::Logged | Freshness::None => None,
    }
}

/// Takes the saved readings that are newer than the card's meters, and the
/// last failures to read them.
fn apply_readings(card: &mut QuotaCard, history: &QuotaHistory) {
    let windows: &[(&str, &str)] = match card.provider {
        Provider::Claude => &[
            (quota::CLAUDE_SESSION, "5-hour session"),
            (quota::CLAUDE, "Weekly"),
        ],
        Provider::Codex => &[(quota::CODEX, "Weekly")],
        Provider::Cursor => &[],
    };
    let shown_at = observed(card.freshness);
    let mut newest: Option<SystemTime> = None;
    for (series, label) in windows {
        let Some(reading) = latest(history, series) else {
            continue;
        };
        let at = reading.observed_at();
        if shown_at.is_some_and(|shown| shown >= at) {
            continue;
        }
        let meter = Meter {
            label: (*label).to_string(),
            used_percent: Some(reading.window.used_percent),
            elapsed_percent: Some(reading.window.elapsed_percent),
            reset: clock::from_millis(reading.resets_at_ms()).map(Reset::At),
            detail: None,
        };
        match card.meters.iter_mut().find(|shown| shown.label == *label) {
            Some(shown) => *shown = meter,
            None => card.meters.push(meter),
        }
        newest = Some(newest.map_or(at, |known| known.max(at)));
    }
    if let Some(at) = newest {
        card.freshness = Freshness::Observed(at);
        // Read live by the quota collector, whatever the report said.
        if !matches!(card.status, CardStatus::Live) {
            card.status = CardStatus::Live;
            card.notes.retain(|note| !note.contains("session logs"));
        }
    }
    for (series, _) in windows {
        if let Some(failure) = history.failures.get(*series) {
            card.notes.push(format!(
                "The last attempt to read the live quota failed: {}",
                failure.message
            ));
        }
    }
}

fn detected(snapshot: &Snapshot, agent: &str) -> bool {
    snapshot
        .summary
        .as_ref()
        .is_ok_and(|summary| summary.agents.iter().any(|usage| usage.agent == agent))
}

fn harness<'a>(snapshot: &'a Snapshot, name: &str) -> Option<&'a Result<HarnessReport, String>> {
    snapshot.harness(name)
}

fn subscription_plan(snapshot: &Snapshot, agent: &str) -> Option<String> {
    let summary = snapshot.summary.as_ref().ok()?;
    summary
        .subscription
        .as_ref()?
        .agents
        .iter()
        .find(|entry| entry.agent == agent)?
        .plan
        .clone()
}

/// When a window ends, from its length and elapsed share at collection.
fn derived_reset(
    collected_at: SystemTime,
    window_minutes: u64,
    elapsed: Option<f64>,
) -> Option<Reset> {
    let elapsed =
        elapsed.filter(|elapsed| elapsed.is_finite() && (0.0..=100.0).contains(elapsed))?;
    let left = window_minutes as f64 * 60.0 * (1.0 - elapsed / 100.0);
    Some(Reset::About(collected_at + Duration::from_secs_f64(left)))
}

fn claude(snapshot: &Snapshot) -> QuotaCard {
    let offline = snapshot.selection.options.offline;
    let summary = snapshot.summary.as_ref().ok();
    let account = summary.and_then(|summary| summary.claude_account.as_ref());
    let status = summary.and_then(|summary| summary.claude_account_status);
    let harness = harness(snapshot, "claude");
    let window = harness.and_then(|report| report.as_ref().ok()?.window.as_ref());
    let mut meters = Vec::new();
    let mut freshness = Freshness::None;
    if let Some(account) = account {
        let at = |millis: Option<i64>| millis.and_then(clock::from_millis).map(Reset::At);
        meters.push(Meter {
            label: "5-hour session".into(),
            used_percent: account.session_used_percent,
            elapsed_percent: None,
            reset: at(account.session_resets_at_ms),
            detail: None,
        });
        meters.push(Meter {
            label: "Weekly".into(),
            used_percent: account.weekly_used_percent,
            elapsed_percent: window.and_then(|window| window.elapsed_percent),
            reset: at(account.weekly_resets_at_ms),
            detail: None,
        });
        for scoped in &account.scoped {
            meters.push(Meter {
                label: format!("Weekly · {}", scoped.name),
                used_percent: scoped.used_percent,
                elapsed_percent: None,
                reset: at(scoped.resets_at_ms),
                detail: None,
            });
        }
        if account.extra_enabled == Some(true) {
            meters.push(Meter {
                label: "Extra usage".into(),
                used_percent: account.extra_used_percent,
                elapsed_percent: None,
                reset: None,
                detail: match (account.extra_used_usd, account.extra_limit_usd) {
                    (Some(used), Some(limit)) => Some(format!(
                        "{} of {} this month",
                        format::money(used),
                        format::money(limit)
                    )),
                    (Some(used), None) => Some(format!("{} this month", format::money(used))),
                    _ => None,
                },
            });
        }
        freshness = account
            .observed_at_ms
            .and_then(clock::from_millis)
            .map_or(Freshness::None, Freshness::Observed);
    } else if let Some(window) = window {
        meters.push(Meter {
            label: "Weekly".into(),
            used_percent: window.used_percent,
            elapsed_percent: window.elapsed_percent,
            reset: derived_reset(
                snapshot.harness_time("claude"),
                window.window_minutes,
                window.elapsed_percent,
            ),
            detail: None,
        });
        freshness = Freshness::Collected(snapshot.harness_time("claude"));
    }
    let card_status = match (status, &snapshot.summary, harness) {
        (_, Err(error), None | Some(Err(_))) if meters.is_empty() => {
            CardStatus::Unavailable(error.clone())
        }
        (Some(AccountStatus::SignInExpired), _, _) => CardStatus::Stale,
        (Some(AccountStatus::SignedOut), _, _) if meters.is_empty() => CardStatus::SignedOut,
        (Some(AccountStatus::SignedOut), _, _) => CardStatus::Stale,
        _ if !meters.is_empty() => CardStatus::Live,
        _ if offline => CardStatus::Offline,
        _ if detected(snapshot, "claude") => CardStatus::NoReading,
        _ => CardStatus::NotDetected,
    };
    QuotaCard {
        provider: Provider::Claude,
        plan: harness
            .and_then(|report| report.as_ref().ok()?.plan.clone())
            .or_else(|| subscription_plan(snapshot, "claude")),
        status: card_status,
        meters,
        notes: Vec::new(),
        freshness,
    }
}

fn codex(snapshot: &Snapshot) -> QuotaCard {
    let offline = snapshot.selection.options.offline;
    let summary = snapshot.summary.as_ref().ok();
    let status = summary.and_then(|summary| summary.codex_account_status);
    let harness = harness(snapshot, "codex");
    let report = harness.and_then(|report| report.as_ref().ok());
    let mut meters = Vec::new();
    let mut notes = Vec::new();
    if let Some(window) = report.and_then(|report| report.window.as_ref()) {
        meters.push(Meter {
            label: "Weekly".into(),
            used_percent: window.used_percent,
            elapsed_percent: window.elapsed_percent,
            reset: derived_reset(
                snapshot.harness_time("codex"),
                window.window_minutes,
                window.elapsed_percent,
            ),
            detail: None,
        });
    }
    let short = summary
        .and_then(|summary| summary.subscription.as_ref())
        .and_then(|subscription| {
            subscription
                .agents
                .iter()
                .find(|agent| agent.agent == "codex")
        })
        .and_then(|agent| agent.short_window.as_ref());
    if let Some(short) = short {
        meters.push(Meter {
            label: format!("{} window", short.label),
            used_percent: short.used_percent,
            elapsed_percent: None,
            reset: None,
            detail: None,
        });
    }
    if let Some(resets) = report.and_then(|report| report.reset_credits_available) {
        notes.push(format!(
            "{resets} banked limit reset{} available.",
            if resets == 1 { "" } else { "s" }
        ));
    }
    let signed_out = status == Some(AccountStatus::SignedOut);
    // Without a sign-in, or offline, the engine falls back to session logs.
    let from_logs = signed_out || offline;
    if from_logs && !meters.is_empty() {
        let why = if signed_out {
            "No ChatGPT sign-in"
        } else {
            "Offline"
        };
        notes.push(format!(
            "{why}: these meters come from Codex session logs and may be older than this collection."
        ));
    }
    let card_status = match harness {
        Some(Err(error)) if meters.is_empty() => CardStatus::Unavailable(error.clone()),
        _ if signed_out && meters.is_empty() => CardStatus::SignedOut,
        _ if from_logs && !meters.is_empty() => CardStatus::FromLogs,
        _ if !meters.is_empty() => CardStatus::Live,
        _ if offline => CardStatus::Offline,
        _ if detected(snapshot, "codex") => CardStatus::NoReading,
        _ => CardStatus::NotDetected,
    };
    let freshness = match card_status {
        CardStatus::FromLogs => Freshness::Logged,
        CardStatus::Live => Freshness::Collected(snapshot.harness_time("codex")),
        _ => Freshness::None,
    };
    QuotaCard {
        provider: Provider::Codex,
        plan: report
            .and_then(|report| report.plan.clone())
            .or_else(|| subscription_plan(snapshot, "codex")),
        status: card_status,
        meters,
        notes,
        freshness,
    }
}

fn cursor(snapshot: &Snapshot) -> QuotaCard {
    let offline = snapshot.selection.options.offline;
    let summary = snapshot.summary.as_ref().ok();
    let account = summary.and_then(|summary| summary.cursor_account.as_ref());
    let status = summary.and_then(|summary| summary.cursor_account_status);
    let mut meters = Vec::new();
    let mut notes = Vec::new();
    if let Some(account) = account {
        meters = cursor_meters(account);
        if let (Some(spent), Some(limit)) =
            (account.on_demand_spent_usd, account.on_demand_limit_usd)
        {
            notes.push(format!(
                "On-demand usage: {} of {}.",
                format::money(spent),
                format::money(limit)
            ));
        }
    }
    if offline {
        notes.push("Offline: Cursor provides no new report.".into());
    }
    let card_status = match (&snapshot.summary, status) {
        (Err(error), _) if meters.is_empty() => CardStatus::Unavailable(error.clone()),
        (_, Some(AccountStatus::SignedOut)) => CardStatus::SignedOut,
        _ if !meters.is_empty() => CardStatus::Live,
        _ if offline => CardStatus::Offline,
        _ if detected(snapshot, "cursor") => CardStatus::NoReading,
        _ => CardStatus::NotDetected,
    };
    QuotaCard {
        provider: Provider::Cursor,
        plan: subscription_plan(snapshot, "cursor"),
        freshness: if card_status == CardStatus::Live {
            Freshness::Collected(snapshot.collected_at)
        } else {
            Freshness::None
        },
        status: card_status,
        meters,
        notes,
    }
}

pub fn show(
    ui: &mut egui::Ui,
    snapshot: &Snapshot,
    history: Option<&QuotaHistory>,
    now: SystemTime,
) {
    for quota in cards(snapshot, history) {
        card(ui, |ui| show_card(ui, &quota, now));
        ui.add_space(6.0);
    }
}

fn show_card(ui: &mut egui::Ui, quota: &QuotaCard, now: SystemTime) {
    let provider = quota.provider.label();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(provider)
                .font(semibold(17.0))
                .color(menu_text()),
        );
        if let Some(plan) = &quota.plan {
            ui.label(egui::RichText::new(plan).color(muted()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(freshness_text(quota.freshness, now))
                    .color(muted())
                    .size(12.0),
            );
        });
    });
    let message = match &quota.status {
        CardStatus::SignedOut => Some((
            caution(),
            format!("Not signed in: sign in to {provider} to read its quota."),
        )),
        CardStatus::Stale if quota.meters.is_empty() => Some((
            caution(),
            match quota.provider {
                Provider::Claude => {
                    "Sign-in expired: run claude and /login to renew it.".to_string()
                }
                _ => format!("Sign-in expired: sign in to {provider} again."),
            },
        )),
        CardStatus::Stale => Some((
            caution(),
            "Sign-in expired: these are the last meters read.".to_string(),
        )),
        CardStatus::Offline => Some((muted(), "Offline: live quotas are not read.".to_string())),
        CardStatus::NoReading => Some((muted(), "No quota reading was reported.".to_string())),
        CardStatus::NotDetected => Some((
            muted(),
            format!("No {provider} usage was found on this computer."),
        )),
        CardStatus::Unavailable(error) => Some((
            danger(),
            match quota.provider {
                // Cursor balances come with the usage report.
                Provider::Cursor => format!("The usage report could not be read: {error}"),
                _ => format!("The quota could not be read: {error}"),
            },
        )),
        CardStatus::Live | CardStatus::FromLogs => None,
    };
    if let Some((color, text)) = message {
        ui.colored_label(color, text);
    }
    let stale = quota.status == CardStatus::Stale;
    for meter in &quota.meters {
        ui.add_space(6.0);
        show_meter(ui, meter, stale, now);
    }
    for note in &quota.notes {
        ui.label(egui::RichText::new(note).color(muted()).size(12.0));
    }
}

fn show_meter(ui: &mut egui::Ui, meter: &Meter, stale: bool, now: SystemTime) {
    let state = GaugeState::of(meter.used_percent);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(&meter.label).color(menu_text()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (text, color) = match (stale, state.label()) {
                (true, _) => (Some("Last known".to_string()), muted()),
                (false, Some(label)) => (
                    Some(label.to_string()),
                    match state {
                        GaugeState::Exhausted => danger(),
                        GaugeState::NearLimit => caution(),
                        _ => muted(),
                    },
                ),
                (false, None) => (None, muted()),
            };
            if let Some(text) = text {
                ui.label(egui::RichText::new(text).color(color).strong());
            }
        });
    });
    gauge::bar(ui, meter.used_percent, meter.elapsed_percent, stale);
    let mut parts = Vec::new();
    match meter.used_percent.filter(|used| used.is_finite()) {
        Some(used) => {
            parts.push(format!("{} used", format::percent(used)));
            parts.push(format!("{} left", format::percent((100.0 - used).max(0.0))));
        }
        None => parts.push("Usage unknown".into()),
    }
    if let Some(elapsed) = meter.elapsed_percent.filter(|elapsed| elapsed.is_finite()) {
        parts.push(format!(
            "{} of the window elapsed",
            format::percent(elapsed)
        ));
    }
    if let Some(detail) = &meter.detail {
        parts.push(detail.clone());
    }
    match meter.reset {
        Some(Reset::At(time)) => parts.push(format!(
            "resets {} ({})",
            format::until(time, now),
            clock::local_date_time(time)
        )),
        Some(Reset::About(time)) => parts.push(format!(
            "resets {} (~{})",
            format::until(time, now).replacen("in ", "in about ", 1),
            clock::local_date_time(time)
        )),
        Some(Reset::Expires(time)) => parts.push(format!(
            "expires {} ({})",
            format::until(time, now),
            clock::local_date_time(time)
        )),
        None => {}
    }
    ui.label(
        egui::RichText::new(parts.join(" · "))
            .color(muted())
            .size(12.0),
    );
}

/// Whether a reading at `time` is still live at `now`.
pub fn is_live(time: SystemTime, now: SystemTime) -> bool {
    now.duration_since(time).unwrap_or_default() <= LIVE_FOR
}

/// When the meters were read, saying whether they are live or the last
/// known ones.
fn freshness_text(freshness: Freshness, now: SystemTime) -> String {
    let state = |time: SystemTime| {
        if is_live(time, now) {
            "Live"
        } else {
            "Last known"
        }
    };
    match freshness {
        Freshness::Observed(time) => format!(
            "{} · reported {} ({})",
            state(time),
            format::ago(time, now),
            clock::local_date_time(time)
        ),
        Freshness::Collected(time) => format!(
            "{} · read {} ({})",
            state(time),
            format::ago(time, now),
            clock::local_date_time(time)
        ),
        Freshness::Logged => "From session logs; reading time not reported".into(),
        Freshness::None => String::new(),
    }
}

/// Promotional credits, while some remain, then the included allowance.
fn cursor_meters(account: &CursorAccount) -> Vec<Meter> {
    let left = |remaining: Option<f64>, limit: Option<f64>| match (remaining, limit) {
        (Some(remaining), Some(limit)) => Some(format!(
            "{} left of {}",
            format::money(remaining),
            format::money(limit)
        )),
        (None, Some(limit)) => Some(format!("{} allowance", format::money(limit))),
        _ => None,
    };
    let mut meters = Vec::new();
    let promotions: Vec<_> = account
        .grants
        .iter()
        .filter(|grant| {
            grant.kind.as_deref() == Some("promo")
                && grant.remaining_usd.is_some_and(|remaining| remaining > 0.0)
        })
        .collect();
    if !promotions.is_empty() {
        meters.push(Meter {
            label: "Promotional credits".into(),
            used_percent: account.active_percent_used,
            elapsed_percent: None,
            reset: promotions
                .iter()
                .filter_map(|grant| grant.expires_at_ms)
                .min()
                .and_then(clock::from_millis)
                .map(Reset::Expires),
            detail: left(account.active_remaining_usd, account.active_limit_usd),
        });
    }
    let included_known =
        account.included_percent_used.is_some() || account.included_limit_usd.is_some();
    if included_known {
        meters.push(Meter {
            label: "Included allowance".into(),
            used_percent: account.included_percent_used,
            elapsed_percent: None,
            reset: account
                .billing_cycle_end_ms
                .and_then(clock::from_millis)
                .map(Reset::At),
            detail: left(account.included_remaining_usd, account.included_limit_usd),
        });
    }
    meters
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::state::{EngineOptions, Selection, fixture_collector};

    fn snapshot(scenario: &str, offline: bool) -> Snapshot {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(scenario);
        let selection = Selection {
            options: EngineOptions {
                offline,
                ..EngineOptions::default()
            },
            ..Selection::default()
        };
        fixture_collector(folder)(&selection)
    }

    #[test]
    fn a_newer_saved_reading_replaces_the_report_and_dates_the_card() {
        let snapshot = snapshot("quotas", false);
        let report_at = match cards(&snapshot, None)[0].freshness {
            Freshness::Observed(at) => at,
            other => panic!("{other:?}"),
        };
        let later = report_at + Duration::from_secs(600);
        let ms = later
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let mut history = QuotaHistory::default();
        history.readings.insert(
            quota::CLAUDE_SESSION.into(),
            vec![QuotaReading {
                series: quota::CLAUDE_SESSION.into(),
                observed_at_ms: ms,
                window: quota::QuotaWindow {
                    window_minutes: 300.0,
                    used_percent: 81.0,
                    elapsed_percent: 50.0,
                    api_equivalent_spent: 0.0,
                },
            }],
        );
        history.failures.insert(
            quota::CLAUDE.into(),
            quota::Failure {
                message: "The live quota could not be read (offline).".into(),
                at_ms: ms,
            },
        );

        let claude = cards(&snapshot, Some(&history)).remove(0);

        assert_eq!(claude.freshness, Freshness::Observed(instant(ms)));
        let session = claude
            .meters
            .iter()
            .find(|meter| meter.label == "5-hour session")
            .unwrap();
        assert_eq!(session.used_percent, Some(81.0));
        assert!(
            claude
                .notes
                .iter()
                .any(|note| note.contains("could not be read (offline)"))
        );
        // An older saved reading leaves the report's meters.
        let earlier = report_at - Duration::from_secs(600);
        let mut older = history.clone();
        if let Some(readings) = older.readings.get_mut(quota::CLAUDE_SESSION) {
            readings[0].observed_at_ms = earlier
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
        }
        let kept = cards(&snapshot, Some(&older)).remove(0);
        assert_ne!(kept.meters[0].used_percent, Some(81.0));
        assert_eq!(kept.freshness, Freshness::Observed(report_at));
    }

    fn instant(ms: i64) -> SystemTime {
        std::time::UNIX_EPOCH + Duration::from_millis(ms as u64)
    }

    #[test]
    fn the_dashboard_and_the_widget_show_the_same_reading_and_time() {
        let snapshot = snapshot("no-quota", false);
        let ms = 1_791_979_200_000;
        let mut history = QuotaHistory::default();
        history.readings.insert(
            quota::CODEX.into(),
            vec![QuotaReading {
                series: quota::CODEX.into(),
                observed_at_ms: ms,
                window: quota::QuotaWindow {
                    window_minutes: 10_080.0,
                    used_percent: 37.0,
                    elapsed_percent: 40.0,
                    api_equivalent_spent: 0.0,
                },
            }],
        );

        let codex = cards(&snapshot, Some(&history)).remove(1);
        let widget = crate::widget::view::view(&history, Provider::Codex, ms + 60_000);

        assert_eq!(codex.freshness, Freshness::Observed(instant(ms)));
        assert_eq!(codex.meters[0].used_percent, Some(37.0));
        let crate::widget::view::RowState::Read {
            remaining,
            observed_at_ms,
            ..
        } = widget.rows[0].state
        else {
            panic!("no reading in the widget");
        };
        assert_eq!((100.0 - remaining, observed_at_ms), (37.0, ms));
    }

    #[test]
    fn an_old_reading_is_never_called_live() {
        let now = SystemTime::now();
        let fresh = freshness_text(Freshness::Observed(now - Duration::from_secs(60)), now);
        let old = freshness_text(
            Freshness::Observed(now - Duration::from_secs(2 * 3600)),
            now,
        );
        let collected = freshness_text(
            Freshness::Collected(now - Duration::from_secs(11 * 60)),
            now,
        );
        assert!(fresh.starts_with("Live · reported"), "{fresh}");
        assert!(old.starts_with("Last known · reported"), "{old}");
        assert!(collected.starts_with("Last known · read"), "{collected}");
    }

    fn card(scenario: &str, offline: bool, provider: Provider) -> QuotaCard {
        cards(&snapshot(scenario, offline), None)
            .into_iter()
            .find(|card| card.provider == provider)
            .unwrap()
    }

    fn used(card: &QuotaCard) -> Vec<(String, Option<f64>)> {
        card.meters
            .iter()
            .map(|meter| (meter.label.clone(), meter.used_percent))
            .collect()
    }

    #[test]
    fn claude_shows_session_and_week_apart() {
        let card = card("quotas", false, Provider::Claude);

        assert_eq!(card.status, CardStatus::Live);
        assert_eq!(
            used(&card),
            [
                ("5-hour session".to_string(), Some(37.0)),
                ("Weekly".to_string(), Some(58.0)),
                ("Weekly · Opus".to_string(), Some(21.0)),
                ("Extra usage".to_string(), Some(25.0)),
            ]
        );
        assert!(matches!(card.meters[0].reset, Some(Reset::At(_))));
        assert!(card.meters[1].elapsed_percent.is_some());
        assert_eq!(
            card.meters[3].detail.as_deref(),
            Some("$12.50 of $50.00 this month")
        );
        assert!(matches!(card.freshness, Freshness::Observed(_)));
    }

    #[test]
    fn codex_meters_from_logs_say_so() {
        let card = card("quotas", false, Provider::Codex);

        assert_eq!(card.status, CardStatus::FromLogs);
        assert_eq!(
            used(&card),
            [
                ("Weekly".to_string(), Some(63.0)),
                ("5h window".to_string(), Some(42.0)),
            ]
        );
        assert!(matches!(card.meters[0].reset, Some(Reset::About(_))));
        assert_eq!(card.freshness, Freshness::Logged);
    }

    #[test]
    fn a_disconnected_account_draws_no_meter() {
        let partial_claude = card("partial", false, Provider::Claude);
        let partial_cursor = card("partial", false, Provider::Cursor);
        let no_quota_codex = card("no-quota", false, Provider::Codex);

        assert_eq!(partial_claude.status, CardStatus::Stale);
        assert!(partial_claude.meters.is_empty());
        assert_eq!(partial_cursor.status, CardStatus::SignedOut);
        assert!(partial_cursor.meters.is_empty());
        assert_eq!(no_quota_codex.status, CardStatus::SignedOut);
        assert!(no_quota_codex.meters.is_empty());
    }

    #[test]
    fn offline_and_absent_providers_are_named() {
        assert_eq!(
            card("no-quota-offline", true, Provider::Claude).status,
            CardStatus::Offline
        );
        let cursor = card("no-quota-offline", true, Provider::Cursor);
        assert_eq!(cursor.status, CardStatus::Offline);
        assert_eq!(cursor.notes, ["Offline: Cursor provides no new report."]);
        assert_eq!(
            card("no-source", false, Provider::Codex).status,
            CardStatus::NotDetected
        );
    }

    #[test]
    fn cursor_keeps_promotional_credits_apart_from_the_allowance() {
        let card = card("live-accounts", false, Provider::Cursor);

        assert_eq!(card.status, CardStatus::Live);
        assert_eq!(
            used(&card),
            [
                ("Promotional credits".to_string(), Some(8.7)),
                ("Included allowance".to_string(), Some(3.0)),
            ]
        );
        assert!(matches!(card.meters[0].reset, Some(Reset::Expires(_))));
        assert_eq!(
            card.meters[0].detail.as_deref(),
            Some("$9.13 left of $10.00")
        );
        assert_eq!(
            card.meters[1].detail.as_deref(),
            Some("$19.40 left of $20.00")
        );
    }

    #[test]
    fn a_failed_report_explains_itself() {
        let card = card("error", false, Provider::Claude);

        assert!(
            matches!(&card.status, CardStatus::Unavailable(message) if message.contains("CLAUDE_CONFIG_DIR"))
        );
        assert!(card.meters.is_empty());
    }

    #[test]
    fn a_derived_reset_ends_the_window() {
        let collected = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

        let reset = derived_reset(collected, 10_080, Some(75.0));

        assert_eq!(
            reset,
            Some(Reset::About(
                collected + Duration::from_secs(10_080 * 60 / 4)
            ))
        );
        assert_eq!(derived_reset(collected, 10_080, None), None);
        assert_eq!(derived_reset(collected, 10_080, Some(140.0)), None);
    }
}
