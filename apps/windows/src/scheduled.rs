//! The scheduled quota collection: a Task Scheduler task of the
//! current user that runs `agent-burn-windows.exe --collect` every minute
//! while they are signed in, also once they quit the dashboard and the
//! widget. It needs no administrator rights: it runs with the user's own
//! sign-in, only while they are signed in. Task Scheduler starts a missed run
//! as soon as it can (after sleep), never starts a second copy while one
//! runs, and stops a run after two minutes; the collector's own lock covers
//! a collection started by hand meanwhile. The executable is GUI-only, so
//! no console window opens.
//!
//! The task is registered again whenever the application starts from
//! another place, so it never keeps pointing at a moved or removed copy
//! that is still started, and removed when the option is turned off.

use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use windows::Win32::{
    Foundation::SYSTEMTIME,
    System::{SystemInformation::GetLocalTime, Threading::CREATE_NO_WINDOW},
};

use crate::files;

/// The name of the task in the Task Scheduler library.
pub const TASK_NAME: &str = "Agent Burn quota collection";
/// Remembers which executable the task runs.
const RECORD_FILE: &str = "scheduled-task.json";

/// What the task should run.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub name: String,
    pub executable: PathBuf,
    /// Arguments after `--collect`.
    pub extra: Vec<String>,
}

impl Plan {
    /// The usual task, for `executable`.
    pub fn for_executable(executable: PathBuf) -> Self {
        Self {
            name: TASK_NAME.into(),
            executable,
            extra: Vec::new(),
        }
    }

    fn arguments(&self) -> String {
        std::iter::once("--collect".to_string())
            .chain(self.extra.iter().map(|argument| quote(argument)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// `argument` quoted for a command line when it holds a space or a quote.
fn quote(argument: &str) -> String {
    if argument.is_empty() || argument.contains([' ', '\t', '"']) {
        format!("\"{}\"", argument.replace('"', "\\\""))
    } else {
        argument.to_string()
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The current user as Task Scheduler names them: `DOMAIN\user`.
pub fn current_user() -> Option<String> {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|user| !user.is_empty())?;
    Some(match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => format!("{domain}\\{user}"),
        _ => user,
    })
}

/// Now in local time, as Task Scheduler writes a start boundary.
fn local_now() -> String {
    let time: SYSTEMTIME = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

/// The task definition, in the Task Scheduler XML schema.
pub fn task_xml(plan: &Plan, user: &str, start: &str) -> String {
    let user = escape(user);
    let repetition = "<Repetition><Interval>PT1M</Interval><StopAtDurationEnd>false</StopAtDurationEnd></Repetition>";
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.3" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Reads the Claude, Codex and Cursor quota meters for Agent Burn every minute while you are signed in. Turn it off in Agent Burn, Settings, Background collection.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
      {repetition}
    </LogonTrigger>
    <TimeTrigger>
      <Enabled>true</Enabled>
      <StartBoundary>{start}</StartBoundary>
      {repetition}
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT2M</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        command = escape(&quote(&plan.executable.display().to_string())),
        arguments = escape(&plan.arguments()),
    )
}

fn schtasks(arguments: &[&str]) -> std::io::Result<Output> {
    Command::new("schtasks.exe")
        .args(arguments)
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()
}

fn failure(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr);
    let text = text.trim();
    if text.is_empty() {
        format!("schtasks ended with code {:?}", output.status.code())
    } else {
        text.to_string()
    }
}

/// Whether a task of that name exists.
pub fn exists(name: &str) -> Result<bool, String> {
    let output = schtasks(&["/Query", "/TN", name]).map_err(|error| error.to_string())?;
    Ok(output.status.success())
}

/// Registers the task of `plan`, replacing one of the same name.
pub fn register(plan: &Plan, folder: &Path) -> Result<(), String> {
    let user = current_user().ok_or("the current user is unknown")?;
    let xml = task_xml(plan, &user, &local_now());
    // Task Scheduler reads its XML as UTF-16, with a byte order mark.
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    let definition = folder.join(format!(".{}.task.xml", std::process::id()));
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    std::fs::write(&definition, bytes).map_err(|error| error.to_string())?;
    let result = schtasks(&[
        "/Create",
        "/TN",
        &plan.name,
        "/XML",
        &definition.display().to_string(),
        "/F",
    ]);
    let _ = std::fs::remove_file(&definition);
    let output = result.map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(failure(&output));
    }
    files::write_atomically(
        &folder.join(RECORD_FILE),
        &serde_json::to_vec(plan).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

/// Removes the task; a missing task is not an error.
pub fn unregister(name: &str, folder: &Path) -> Result<(), String> {
    if !exists(name)? {
        let _ = std::fs::remove_file(folder.join(RECORD_FILE));
        return Ok(());
    }
    let output = schtasks(&["/Delete", "/TN", name, "/F"]).map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(failure(&output));
    }
    let _ = std::fs::remove_file(folder.join(RECORD_FILE));
    Ok(())
}

/// The plan last registered from this application folder.
fn recorded(folder: &Path) -> Option<Plan> {
    serde_json::from_slice(&std::fs::read(folder.join(RECORD_FILE)).ok()?).ok()
}

/// What `ensure` found or did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schedule {
    Registered,
    /// Registered again, to run this copy of the application.
    Updated,
    AlreadyRegistered,
    Removed,
    Off,
}

/// Makes the task match the setting: registered for `plan` when `enabled`
/// (again when it runs another executable or went missing), removed when
/// not.
pub fn ensure(enabled: bool, plan: &Plan, folder: &Path) -> Result<Schedule, String> {
    if !enabled {
        return if exists(&plan.name)? {
            unregister(&plan.name, folder).map(|()| Schedule::Removed)
        } else {
            let _ = std::fs::remove_file(folder.join(RECORD_FILE));
            Ok(Schedule::Off)
        };
    }
    let present = exists(&plan.name)?;
    let same = recorded(folder).as_ref() == Some(plan);
    if present && same {
        return Ok(Schedule::AlreadyRegistered);
    }
    register(plan, folder)?;
    Ok(if present {
        Schedule::Updated
    } else {
        Schedule::Registered
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(executable: &str) -> Plan {
        Plan::for_executable(PathBuf::from(executable))
    }

    #[test]
    fn the_task_runs_the_collector_every_minute_as_the_user_without_overlap() {
        let xml = task_xml(
            &plan(r"C:\Program Files\Agent Burn\agent-burn-windows.exe"),
            r"PC\Léa",
            "2026-10-14T17:00:00",
        );

        for expected in [
            "<Command>&quot;C:\\Program Files\\Agent Burn\\agent-burn-windows.exe&quot;</Command>",
            "<Arguments>--collect</Arguments>",
            "<Interval>PT1M</Interval>",
            "<LogonType>InteractiveToken</LogonType>",
            "<RunLevel>LeastPrivilege</RunLevel>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<StartWhenAvailable>true</StartWhenAvailable>",
            "<ExecutionTimeLimit>PT2M</ExecutionTimeLimit>",
            "<StartBoundary>2026-10-14T17:00:00</StartBoundary>",
            r"<UserId>PC\Léa</UserId>",
            "<LogonTrigger>",
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
        ] {
            assert!(xml.contains(expected), "missing {expected}");
        }
        assert!(!xml.contains("HighestAvailable"));
    }

    #[test]
    fn paths_and_arguments_are_escaped() {
        let mut tested = plan(r"D:\R&D <tools>\agent-burn-windows.exe");
        tested.extra = vec!["--data".into(), r"C:\Temp\Agent Burn test".into()];

        let xml = task_xml(&tested, "PC\\me", "2026-10-14T17:00:00");

        assert!(xml.contains(
            "<Command>&quot;D:\\R&amp;D &lt;tools&gt;\\agent-burn-windows.exe&quot;</Command>"
        ));
        assert!(xml.contains(
            "<Arguments>--collect --data &quot;C:\\Temp\\Agent Burn test&quot;</Arguments>"
        ));
    }

    #[test]
    fn a_plan_is_remembered_with_its_executable() {
        let folder = tempfile::tempdir().unwrap();
        let first = plan(r"C:\Apps\Agent Burn\agent-burn-windows.exe");
        files::write_atomically(
            &folder.path().join(RECORD_FILE),
            &serde_json::to_vec(&first).unwrap(),
        )
        .unwrap();

        assert_eq!(recorded(folder.path()), Some(first));
        // Another copy of the application is another plan.
        assert_ne!(
            recorded(folder.path()),
            Some(plan(r"D:\Moved\agent-burn-windows.exe"))
        );
    }
}
