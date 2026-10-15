# Agent Burn for Windows

A usage dashboard and a taskbar widget for Claude Code, Codex and Cursor on
Windows 10 and 11 (x64). They run on the same `agent-burn` engine as the CLI
and the macOS app.

- **Dashboard.** Spending by day, model and agent, quota cards, and quota
  history with forecasts.
- **Widget.** Sits in the taskbar next to the notification area and shows
  the Claude 5-hour and weekly quotas, the Codex weekly quota and the Cursor
  spending. It moves above the taskbar when the taskbar has no room.
- **Notification-area icon.** Opens the dashboard and gives the widget's
  menu.

## Install

1. Download `agent-burn-windows-<version>-x64.zip` from the
   [Windows releases](https://github.com/Melvynx/agent-burn/releases).
2. Check it against the `.sha256` file published beside it:

   ```powershell
   (Get-FileHash agent-burn-windows-<version>-x64.zip -Algorithm SHA256).Hash
   ```

3. Extract the folder anywhere you can write, for example
   `%LOCALAPPDATA%\Programs\Agent Burn`. No administrator rights,
   installer, Rust or Node.js are needed.
4. Keep `agent-burn-windows.exe` and `agent-burn.exe` side by side. The
   application uses the engine beside it.

The executables are not code-signed. On first launch, Windows SmartScreen
may say it protected your PC: choose **More info**, then **Run anyway**.
Alternatively, before extracting, open the ZIP's **Properties** and tick
**Unblock**.

## Run

Start `agent-burn-windows.exe`. It opens the dashboard and starts the widget
and the notification-area icon in the background. Starting it again brings
the open dashboard forward instead of opening a second one.

- **Widget menu.** Click the widget to open the dashboard, or right-click it
  for its menu:
  - refresh the quotas now;
  - choose the provider shown;
  - hide the widget;
  - dock it back next to the notification area;
  - quit.
- **Floating widget.** Drag it off the taskbar to let it float. It stays
  where you put it on that screen.
- **Closing.** Closing the dashboard window keeps the widget and the icon
  running. **Quit Agent Burn**, in the navigation pane or the menu, ends
  them all.

## Sources

The engine finds the agents' local data by itself:

| Agent       | Default location                                           | Override                                       |
| ----------- | ---------------------------------------------------------- | ---------------------------------------------- |
| Claude Code | `%USERPROFILE%\.claude` and `%USERPROFILE%\.config\claude` | `CLAUDE_CONFIG_DIR`, or **Settings › Sources** |
| Codex       | `%USERPROFILE%\.codex`                                     | `CODEX_HOME`, or **Settings › Sources**        |
| Cursor      | `%APPDATA%\Cursor`                                         | `CURSOR_DATA_DIR`, or **Settings › Sources**   |

- The live quota meters use each agent's own sign-in, read by the engine.
  Agent Burn never copies tokens into its settings or histories.
- An agent that is not installed, or not signed in, shows as such instead
  of a zero.
- **Settings › Engine** can point to another `agent-burn.exe` or to a
  configuration file.
- **Settings › Plans** sets the subscription prices used for the "value"
  figures.

## Offline mode

**Settings › Engine › Offline mode** keeps the engine off the network. Costs
come from local logs only, and no live quota or update check is requested.

When the engine fails, the dashboard shows the last saved report and quota
readings, each marked with its date, until the engine answers again.

## Background collection

**Settings › Background collection › Collect quotas after closing**
registers a task in the Task Scheduler for your account only:

- it is named `Agent Burn quota collection`;
- it needs no administrator rights;
- it records the quotas every minute while you are signed in, also when
  Agent Burn is closed, so the quota history has no gaps.

Turning the option off removes the task.

## Start at sign-in

**Settings › Startup and closing › Start when you sign in** adds an "Agent
Burn" entry to your account's startup programs
(`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`).

- The entry starts the widget and the icon.
- **Open the dashboard too** also opens the window.
- Windows Settings › Apps › Startup lists the entry and can turn it off.

## Your data and its backups

Settings and histories are kept in `%LOCALAPPDATA%\Agent Burn`, apart from
the extracted folder:

| File                   | Content                                              |
| ---------------------- | ---------------------------------------------------- |
| `settings.json`        | Settings                                             |
| `metrics-history.json` | Daily costs, kept after the agents delete their logs |
| `report-cache.json`    | Last reports, shown when the engine fails            |
| `quota-readings.csv`   | Every quota reading, append-only                     |
| `quota-state.json`     | Latest reading per quota and last failures           |

- Each JSON file has a `.bak` copy of its last valid version.
- A damaged file is set aside as `<name>.damaged-<time>.json` and is never
  overwritten.
- To back up or move your history, copy the whole folder while Agent Burn
  is closed.
- `agent-burn-windows.exe --data <folder>` uses another folder, for example
  on a synced drive.

## Update

**Settings › Updates** checks the Windows releases of this repository when
the dashboard opens, unless offline mode is on. It links to the newer
archive; nothing is downloaded or replaced on its own.

To update:

1. Quit Agent Burn (**Quit Agent Burn**).
2. Extract the new archive over the old folder, or beside it.
3. Start the new `agent-burn-windows.exe`.

Your data stays in `%LOCALAPPDATA%\Agent Burn`. If you moved the folder, the
scheduled task and the startup entry follow the new location the next time
the dashboard opens.

## Uninstall

1. In **Settings**, turn off **Collect quotas after closing** and **Start
   when you sign in**. This removes the scheduled task and the startup
   entry.
2. Quit Agent Burn.
3. Delete the extracted folder.
4. To remove your history too, delete `%LOCALAPPDATA%\Agent Burn`.

If the application is already gone, remove the task and the entry by hand:

```powershell
schtasks /Delete /TN "Agent Burn quota collection" /F
reg delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v "Agent Burn" /f
```

## Differences from the macOS app

- **Placement.** The widget lives in the taskbar and the notification area,
  not in the menu bar.
- **Updates.** They are offered as a link to the ZIP. There are no automatic
  signed updates like Sparkle on macOS.
- **Signing.** The executables are not signed. Expect the SmartScreen prompt
  described above.
- **Background collection.** It uses the Task Scheduler instead of a launch
  agent.

## Tested platforms

- **Tested:** Windows 11 Pro x64 (build 26300), one screen, scales 100 to
  200 %.
- **Not yet tested:**
  - Windows 10;
  - several screens;
  - an auto-hidden taskbar;
  - a display-scale change while running;
  - an Explorer restart;
  - sleep and resume;
  - Windows on ARM (no ARM64 build is published).

## Build from source

You need Rust 1.96 (`rust-toolchain.toml`) with the MSVC toolchain. Network
access is required on the first build, which fetches the model pricing.

```powershell
pwsh apps/windows/build.ps1 -Check
```

With `just`, the same runs as `just windows::check`; `just windows::build`
skips the checks.

It builds both executables from the same checkout, with the locked Cargo
files and the C runtime linked in. It then writes
`apps/windows/dist/agent-burn-windows-<version>-x64.zip` and its `.sha256`.
`-Check` also runs rustfmt, clippy and the tests of both projects.
