//! Checks the releases of the Windows adaptation for a newer version and
//! offers its archive. Nothing is downloaded or replaced here: the link opens
//! in the browser, and the settings and histories stay in the user's folder.

use std::{os::windows::process::CommandExt, path::PathBuf, process::Command};

use serde::Deserialize;
use windows::{
    Win32::{
        System::Threading::CREATE_NO_WINDOW, UI::Shell::ShellExecuteW,
        UI::WindowsAndMessaging::SW_SHOWNORMAL,
    },
    core::{HSTRING, w},
};

/// Where the Windows archives are published; never the macOS feed.
pub const REPOSITORY: &str = "Melvynx/agent-burn";
/// Windows releases are tagged `windows-v<version>`.
pub const TAG_PREFIX: &str = "windows-v";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    /// The release page, on the repository above.
    pub page: String,
    /// The ZIP to download, on the repository above.
    pub archive: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    Current,
    Available(Release),
    Failed(String),
}

#[derive(Deserialize)]
struct Published {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// `major.minor.patch`; anything else is not a version.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.').map(|part| part.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// The newest published Windows release above `current`, from the GitHub
/// releases listing. Links leaving the repository are refused.
pub fn newest(listing: &str, current: &str) -> Result<Option<Release>, String> {
    let current = parse_version(current).ok_or("this version cannot be compared")?;
    let releases: Vec<Published> =
        serde_json::from_str(listing).map_err(|_| "the releases list could not be read")?;
    let ours = format!("https://github.com/{REPOSITORY}/");
    let best = releases
        .into_iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            let version = parse_version(release.tag_name.strip_prefix(TAG_PREFIX)?)?;
            let archive = release.assets.iter().find(|asset| {
                asset.name.starts_with("agent-burn-windows-") && asset.name.ends_with(".zip")
            })?;
            if !release.html_url.starts_with(&ours)
                || !archive.browser_download_url.starts_with(&ours)
            {
                return None;
            }
            Some((
                version,
                Release {
                    version: release.tag_name[TAG_PREFIX.len()..].to_string(),
                    page: release.html_url,
                    archive: archive.browser_download_url.clone(),
                },
            ))
        })
        .max_by_key(|(version, _)| *version);
    Ok(best
        .filter(|(version, _)| *version > current)
        .map(|(_, release)| release))
}

/// `curl.exe`, part of Windows 10 1803 and later, from the system folder only.
fn curl() -> PathBuf {
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(system).join("System32").join("curl.exe")
}

/// Asks the repository; a failure is reported, never raised.
pub fn check() -> Check {
    check_with(&curl())
}

fn check_with(program: &std::path::Path) -> Check {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=30");
    let output = Command::new(program)
        .args(["-fsSL", "--max-time", "15", "-H"])
        .arg("Accept: application/vnd.github+json")
        .args(["-H", "User-Agent: agent-burn-windows"])
        .arg(&url)
        .creation_flags(CREATE_NO_WINDOW.0)
        .output();
    let output = match output {
        Ok(output) if output.status.success() => output,
        Ok(_) => return Check::Failed("GitHub could not be reached".into()),
        Err(_) => return Check::Failed("curl.exe is not available".into()),
    };
    match newest(&String::from_utf8_lossy(&output.stdout), CURRENT) {
        Ok(Some(release)) => Check::Available(release),
        Ok(None) => Check::Current,
        Err(error) => Check::Failed(error),
    }
}

/// Opens a link of the repository in the default browser.
pub fn open(url: &str) {
    if !url.starts_with(&format!("https://github.com/{REPOSITORY}/")) {
        return;
    }
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(url),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, extra: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/{REPOSITORY}/releases/tag/{tag}",{extra}
            "assets":[{{"name":"agent-burn-windows-x-x64.zip","browser_download_url":"https://github.com/{REPOSITORY}/releases/download/{tag}/agent-burn-windows-x-x64.zip"}}]}}"#
        )
    }

    #[test]
    fn versions_compare_by_number() {
        assert_eq!(parse_version("0.10.2"), Some((0, 10, 2)));
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.2.x"), None);
    }

    #[test]
    fn only_a_newer_published_windows_release_is_offered() {
        let listing = format!(
            "[{},{},{},{},{}]",
            release("windows-v0.3.0", r#""draft":true,"#),
            release("windows-v0.4.0", r#""prerelease":true,"#),
            release("macos-v9.0.0", ""),
            release("windows-v0.2.0", ""),
            release("windows-v0.10.0", ""),
        );
        let found = newest(&listing, "0.9.0").unwrap().unwrap();
        assert_eq!(found.version, "0.10.0");
        assert!(
            found
                .archive
                .ends_with("/windows-v0.10.0/agent-burn-windows-x-x64.zip")
        );
        assert_eq!(newest(&listing, "0.10.0").unwrap(), None);
        assert_eq!(newest(&listing, "1.0.0").unwrap(), None);
    }

    #[test]
    fn a_release_without_archive_or_outside_the_repository_is_ignored() {
        let foreign = release("windows-v2.0.0", "").replace(REPOSITORY, "someone/else");
        let bare = r#"{"tag_name":"windows-v3.0.0","html_url":"https://github.com/Melvynx/agent-burn/releases/tag/windows-v3.0.0","assets":[]}"#;
        assert_eq!(
            newest(&format!("[{foreign},{bare}]"), "0.1.0").unwrap(),
            None
        );
    }

    #[test]
    fn a_failed_check_is_reported_not_raised() {
        assert!(newest("<html>rate limited</html>", "0.1.0").is_err());
        let missing = std::env::temp_dir().join("no-such-curl-agent-burn.exe");
        assert!(matches!(check_with(&missing), Check::Failed(_)));
    }

    /// Asks the real repository: `cargo test -- --ignored the_real_repository`.
    #[test]
    #[ignore = "network"]
    fn the_real_repository_answers() {
        let check = check();
        println!("{check:?}");
        assert!(!matches!(check, Check::Failed(_)));
    }
}
