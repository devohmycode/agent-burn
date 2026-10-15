//! Source discovery as a Windows launcher sees it: a profile folder with spaces
//! and accents, `HOME` set elsewhere by Unix-like tools, and custom directories.
#![cfg(windows)]

use std::{path::Path, process::Command};

use agent_burn_test_support::Fixture;
use serde_json::Value;

const CLAUDE_LOG: &str = r#"{"timestamp":"2026-01-09T10:00:00.000Z","sessionId":"session-alpha","message":{"role":"assistant","id":"msg-alpha-1","model":"claude-sonnet-4-20250514","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":25,"cache_read_input_tokens":10}},"requestId":"req-alpha-1","costUSD":0.12,"version":"1.0.0"}
"#;
const CODEX_LOG: &str = r#"{"timestamp":"2026-05-13T09:00:00.000Z","type":"turn_context","payload":{"model":"gpt-5.2-codex"}}
{"timestamp":"2026-05-13T09:01:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"model":"gpt-5.2-codex","last_token_usage":{"input_tokens":1000,"cached_input_tokens":250,"output_tokens":125,"reasoning_output_tokens":75,"total_tokens":1200},"total_token_usage":{"input_tokens":1000,"cached_input_tokens":250,"output_tokens":125,"reasoning_output_tokens":75,"total_tokens":1200}}}}
"#;

fn write_sources(fixture: &Fixture, claude_dir: &str, codex_dir: &str) {
    let _ = fixture.write_file(
        format!("{claude_dir}/projects/C--Users-Lea-projet/session-alpha.jsonl"),
        CLAUDE_LOG,
    );
    let _ = fixture.write_file(
        format!("{codex_dir}/sessions/2026/05/13/rollout-2026-05-13T09-00-00.jsonl"),
        CODEX_LOG,
    );
}

/// Agents detected by an offline summary whose environment holds only `vars`.
fn detected_agents(vars: &[(&str, &Path)]) -> Vec<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_agent-burn"))
        .args([
            "summary",
            "--offline",
            "--json",
            "--no-color",
            "--timezone",
            "UTC",
        ])
        .env_clear()
        .env("SYSTEMROOT", std::env::var_os("SYSTEMROOT").unwrap())
        .envs(vars.iter().copied())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut agents = value["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["agent"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    agents.sort();
    agents
}

#[test]
fn finds_sources_in_a_profile_folder_with_spaces_and_accents() {
    let fixture = Fixture::new();
    write_sources(&fixture, "Profil Lea Été/.claude", "Profil Lea Été/.codex");

    let agents = detected_agents(&[("USERPROFILE", &fixture.path("Profil Lea Été"))]);

    assert_eq!(agents, ["claude", "codex"]);
}

#[test]
fn profile_folder_wins_over_a_home_set_elsewhere() {
    let fixture = Fixture::new();
    write_sources(&fixture, "Profil Lea Été/.claude", "Profil Lea Été/.codex");
    let msys_home = fixture.create_dir_all("msys64/home/lea");

    let agents = detected_agents(&[
        ("USERPROFILE", &fixture.path("Profil Lea Été")),
        ("HOME", &msys_home),
    ]);

    assert_eq!(agents, ["claude", "codex"]);
}

#[test]
fn reads_custom_source_directories_with_spaces_and_accents() {
    let fixture = Fixture::new();
    write_sources(
        &fixture,
        "Données agents/Claude perso",
        "Données agents/Codex perso",
    );
    let empty_profile = fixture.create_dir_all("Profil vide");

    let agents = detected_agents(&[
        ("USERPROFILE", &empty_profile),
        (
            "CLAUDE_CONFIG_DIR",
            &fixture.path("Données agents/Claude perso"),
        ),
        ("CODEX_HOME", &fixture.path("Données agents/Codex perso")),
    ]);

    assert_eq!(agents, ["claude", "codex"]);
}
