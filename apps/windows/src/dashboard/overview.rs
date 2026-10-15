//! The usage summary: API-equivalent costs and tokens, by agent and by
//! model, beside what subscriptions cost and what providers actually bill.
//! Every figure is the engine's value as is; only shares are derived.

use eframe::egui;

use crate::{
    dashboard::format,
    models::SummaryReport,
    state::Snapshot,
    ui::{
        components::fluent::{card, section, semibold},
        series,
        theme::{danger, is_light, menu_text, muted, track},
    },
};

/// Models listed before "Show all".
const TOP_MODELS: usize = 10;

#[derive(Debug, PartialEq)]
pub struct Overview {
    pub total_cost: f64,
    pub total_tokens: u64,
    pub agents: Vec<AgentLine>,
    pub models: Vec<ModelLine>,
    pub plans: Vec<PlanLine>,
    pub billed: Vec<BilledLine>,
}

#[derive(Debug, PartialEq)]
pub struct AgentLine {
    pub agent: String,
    pub cost: f64,
    pub tokens: u64,
    /// Share of the total API-equivalent cost, 0–100; unknown at zero total.
    pub share: Option<f64>,
}

#[derive(Debug, PartialEq)]
pub struct ModelLine {
    pub agent: String,
    pub model: String,
    pub cost: f64,
    pub tokens: u64,
    /// The engine's share of the agent's cost.
    pub share_of_agent: Option<f64>,
}

#[derive(Debug, PartialEq)]
pub struct PlanLine {
    pub agent: String,
    pub plan: Option<String>,
    pub price_per_month: Option<f64>,
    /// API-equivalent cost over the report period.
    pub period_usage: f64,
}

/// Money a provider charges beyond API-equivalent estimates.
#[derive(Debug, PartialEq)]
pub struct BilledLine {
    pub label: String,
    pub amount: f64,
    pub limit: Option<f64>,
}

impl Overview {
    /// `None` when no agent was detected: an empty report is not zero usage.
    pub fn from_summary(report: &SummaryReport) -> Option<Self> {
        if report.agents.is_empty() {
            return None;
        }
        let total = report.totals.total_cost;
        let share = |cost: f64| (total > 0.0).then(|| cost / total * 100.0);
        let mut agents: Vec<AgentLine> = report
            .agents
            .iter()
            .map(|agent| AgentLine {
                agent: agent.agent.clone(),
                cost: agent.total_cost,
                tokens: agent.total_tokens,
                share: share(agent.total_cost),
            })
            .collect();
        agents.sort_by(|a, b| b.cost.total_cmp(&a.cost));
        let mut models: Vec<ModelLine> = report
            .agents
            .iter()
            .flat_map(|agent| {
                agent.models.iter().map(|model| ModelLine {
                    agent: agent.agent.clone(),
                    model: model.model.clone(),
                    cost: model.total_cost,
                    tokens: model.total_tokens,
                    share_of_agent: model.percentage,
                })
            })
            .collect();
        models.sort_by(|a, b| b.cost.total_cmp(&a.cost));
        let plans = report
            .subscription
            .iter()
            .flat_map(|subscription| &subscription.agents)
            .map(|agent| PlanLine {
                agent: agent.agent.clone(),
                plan: agent.plan.clone(),
                price_per_month: agent.price_per_month,
                period_usage: agent.period_usage,
            })
            .collect();
        let mut billed = Vec::new();
        if let Some(account) = &report.claude_account
            && account.extra_enabled == Some(true)
            && let Some(used) = account.extra_used_usd
        {
            billed.push(BilledLine {
                label: "Claude extra usage this month".into(),
                amount: used,
                limit: account.extra_limit_usd,
            });
        }
        if let Some(account) = &report.cursor_account {
            if let Some(spent) = account.on_demand_spent_usd {
                billed.push(BilledLine {
                    label: "Cursor on-demand usage this cycle".into(),
                    amount: spent,
                    limit: account.on_demand_limit_usd,
                });
            }
            if let Some(spent) = account.plan_spend_usd {
                billed.push(BilledLine {
                    label: "Cursor plan usage this cycle".into(),
                    amount: spent,
                    limit: account.included_limit_usd,
                });
            }
        }
        Some(Self {
            total_cost: total,
            total_tokens: report.totals.total_tokens,
            agents,
            models,
            plans,
            billed,
        })
    }

    pub fn agent_name(line: &AgentLine) -> String {
        series::name(&line.agent)
    }
}

pub fn show(ui: &mut egui::Ui, snapshot: &Snapshot) {
    let summary = match &snapshot.summary {
        Ok(summary) => summary,
        Err(error) => {
            card(ui, |ui| {
                ui.colored_label(
                    danger(),
                    format!("The usage report could not be read: {error}"),
                );
            });
            return;
        }
    };
    let Some(overview) = Overview::from_summary(summary) else {
        card(ui, |ui| {
            ui.label("No agent usage was found for this period.");
            ui.label(
                egui::RichText::new(
                    "Agent Burn reads the logs of Claude Code, Codex, Cursor and other coding agents on this computer. Check the source folders in Settings.",
                )
                .color(muted()),
            );
        });
        return;
    };

    section(ui, "API-equivalent usage", |ui| {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format::money(overview.total_cost))
                        .font(semibold(32.0))
                        .color(menu_text()),
                )
                .on_hover_text(format::exact_money(overview.total_cost));
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} tokens",
                        format::tokens(overview.total_tokens)
                    ))
                    .size(18.0)
                    .color(muted()),
                )
                .on_hover_text(format!("{} tokens", overview.total_tokens));
            });
            ui.label(
                egui::RichText::new(
                    "What this usage would cost at API prices. It is not what your subscriptions cost or what you are billed.",
                )
                .color(muted())
                .size(12.0),
            );
        });
    });

    section(ui, "By agent", |ui| {
        card(ui, |ui| {
            egui::Grid::new("agents")
                .num_columns(4)
                .spacing([18.0, 10.0])
                .show(ui, |ui| {
                    for line in &overview.agents {
                        ui.horizontal(|ui| {
                            swatch(ui, &line.agent);
                            ui.label(Overview::agent_name(line));
                        });
                        ui.label(format::money(line.cost))
                            .on_hover_text(format::exact_money(line.cost));
                        ui.label(egui::RichText::new(format::tokens(line.tokens)).color(muted()))
                            .on_hover_text(format!("{} tokens", line.tokens));
                        share_bar(ui, &line.agent, line.share);
                        ui.end_row();
                    }
                });
        });
    });

    section(ui, "By model", |ui| {
        card(ui, |ui| {
            let id = ui.id().with("all-models");
            let mut all = ui.data(|data| data.get_temp::<bool>(id).unwrap_or(false));
            let shown = if all {
                overview.models.len()
            } else {
                TOP_MODELS.min(overview.models.len())
            };
            egui::Grid::new("models")
                .num_columns(5)
                .spacing([18.0, 8.0])
                .striped(false)
                .show(ui, |ui| {
                    for heading in ["Model", "Agent", "Cost", "Tokens", "Share of agent"] {
                        ui.label(egui::RichText::new(heading).color(muted()).size(12.0));
                    }
                    ui.end_row();
                    for line in &overview.models[..shown] {
                        ui.label(&line.model);
                        ui.horizontal(|ui| {
                            swatch(ui, &line.agent);
                            ui.label(series::name(&line.agent));
                        });
                        ui.label(format::money(line.cost))
                            .on_hover_text(format::exact_money(line.cost));
                        ui.label(egui::RichText::new(format::tokens(line.tokens)).color(muted()))
                            .on_hover_text(format!("{} tokens", line.tokens));
                        ui.label(
                            line.share_of_agent
                                .map_or_else(|| "unknown".into(), format::percent),
                        );
                        ui.end_row();
                    }
                });
            if overview.models.len() > TOP_MODELS {
                let label = if all {
                    "Show fewer".to_string()
                } else {
                    format!("Show all {} models", overview.models.len())
                };
                if ui.button(label).clicked() {
                    all = !all;
                    ui.data_mut(|data| data.insert_temp(id, all));
                }
            }
        });
    });

    if !overview.plans.is_empty() {
        section(ui, "Subscriptions", |ui| {
            card(ui, |ui| {
                egui::Grid::new("plans")
                    .num_columns(3)
                    .spacing([18.0, 8.0])
                    .show(ui, |ui| {
                        for heading in ["Agent", "Plan", "API-equivalent usage this period"] {
                            ui.label(egui::RichText::new(heading).color(muted()).size(12.0));
                        }
                        ui.end_row();
                        for plan in &overview.plans {
                            ui.horizontal(|ui| {
                                swatch(ui, &plan.agent);
                                ui.label(series::name(&plan.agent));
                            });
                            let name = plan.plan.clone().unwrap_or_else(|| "Plan unknown".into());
                            ui.label(match plan.price_per_month {
                                Some(price) => format!("{name} · {} / month", format::money(price)),
                                None => format!("{name} · price unknown"),
                            });
                            ui.label(format::money(plan.period_usage))
                                .on_hover_text(format::exact_money(plan.period_usage));
                            ui.end_row();
                        }
                    });
                ui.label(
                    egui::RichText::new(
                        "Plan prices are what the subscriptions cost per month; usage is valued at API prices over the selected period.",
                    )
                    .color(muted())
                    .size(12.0),
                );
            });
        });
    }

    if !overview.billed.is_empty() {
        section(ui, "Billed by providers", |ui| {
            card(ui, |ui| {
                for line in &overview.billed {
                    ui.horizontal(|ui| {
                        ui.label(&line.label);
                        let amount = match line.limit {
                            Some(limit) => format!(
                                "{} of {}",
                                format::money(line.amount),
                                format::money(limit)
                            ),
                            None => format::money(line.amount),
                        };
                        ui.label(egui::RichText::new(amount).strong());
                    });
                }
                ui.label(
                    egui::RichText::new(
                        "Amounts the providers report as charged or counted against your plan, for their current cycle.",
                    )
                    .color(muted())
                    .size(12.0),
                );
            });
        });
    }
}

fn swatch(ui: &mut egui::Ui, agent: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 2.0, series::color(series::slot(agent), is_light()));
}

fn share_bar(ui: &mut egui::Ui, agent: &str, share: Option<f64>) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(140.0, 8.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, track());
        if let Some(share) = share {
            let width = rect.width() * (share.clamp(0.0, 100.0) / 100.0) as f32;
            if width > 0.5 {
                painter.rect_filled(
                    egui::Rect::from_min_size(rect.min, egui::vec2(width.max(4.0), rect.height())),
                    4.0,
                    series::color(series::slot(agent), is_light()),
                );
            }
        }
        ui.label(
            egui::RichText::new(share.map_or_else(|| "–".into(), format::percent)).color(muted()),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(scenario: &str) -> SummaryReport {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(scenario)
            .join("summary.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn raw(scenario: &str) -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(scenario)
            .join("summary.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn no_source_shows_no_overview() {
        assert_eq!(Overview::from_summary(&fixture("no-source")), None);
    }

    #[test]
    fn one_source_keeps_the_engine_values() {
        let json = raw("single-source");
        let overview = Overview::from_summary(&fixture("single-source")).unwrap();

        assert_eq!(overview.agents.len(), 1);
        assert_eq!(
            overview.total_cost,
            json["totals"]["totalCost"].as_f64().unwrap()
        );
        assert_eq!(
            overview.total_tokens,
            json["totals"]["totalTokens"].as_u64().unwrap()
        );
        assert_eq!(overview.agents[0].agent, "claude");
        assert_eq!(overview.agents[0].share, Some(100.0));
    }

    #[test]
    fn several_sources_match_the_report_exactly() {
        let json = raw("live-accounts");
        let overview = Overview::from_summary(&fixture("live-accounts")).unwrap();

        assert_eq!(
            overview.total_cost,
            json["totals"]["totalCost"].as_f64().unwrap()
        );
        assert_eq!(
            overview.agents.len(),
            json["agents"].as_array().unwrap().len()
        );
        for agent in json["agents"].as_array().unwrap() {
            let line = overview
                .agents
                .iter()
                .find(|line| line.agent == agent["agent"].as_str().unwrap())
                .unwrap();
            assert_eq!(line.cost, agent["totalCost"].as_f64().unwrap());
            assert_eq!(line.tokens, agent["totalTokens"].as_u64().unwrap());
            for model in agent["models"].as_array().unwrap() {
                let found = overview.models.iter().any(|line| {
                    line.agent == agent["agent"].as_str().unwrap()
                        && line.model == model["model"].as_str().unwrap()
                        && line.cost == model["totalCost"].as_f64().unwrap()
                        && line.tokens == model["totalTokens"].as_u64().unwrap()
                });
                assert!(found, "{model}");
            }
        }
        assert!(
            overview
                .agents
                .windows(2)
                .all(|pair| pair[0].cost >= pair[1].cost)
        );
    }

    #[test]
    fn subscription_prices_stay_apart_from_api_equivalent_costs() {
        let overview = Overview::from_summary(&fixture("live-accounts")).unwrap();

        let cursor = overview
            .plans
            .iter()
            .find(|plan| plan.agent == "cursor")
            .unwrap();
        assert_eq!(cursor.price_per_month, Some(20.0));
        assert_eq!(cursor.period_usage, 0.6);
        let labels: Vec<&str> = overview
            .billed
            .iter()
            .map(|line| line.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Claude extra usage this month",
                "Cursor plan usage this cycle"
            ]
        );
        assert_eq!(overview.billed[0].amount, 12.5);
        assert_eq!(overview.billed[0].limit, Some(50.0));
    }
}
