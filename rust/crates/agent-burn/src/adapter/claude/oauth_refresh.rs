//! Renews Claude Code's OAuth sign-in with the same protocol Claude Code
//! 2.1 uses, so a refresh by Agent Burn and one by a `claude` session never
//! burn the single-use refresh token twice:
//!
//! 1. hold `~/.claude/.oauth_refresh.lock` and the legacy `~/.claude.lock`
//!    (proper-lockfile directories, stale after 60 seconds);
//! 2. re-read the stored sign-in and adopt it when someone else renewed it;
//! 3. post the refresh token, then write back only when the stored refresh
//!    token is still the one that was posted.

use std::{
    fs, io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime},
};

use serde_json::{Value, json};

use super::credentials::{Credentials, OAuthToken, RefreshedToken};
use crate::{TimestampMs, home, utc_now};

const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const LOCK_STALE: Duration = Duration::from_secs(60);
const LOCK_WAIT: Duration = Duration::from_secs(10);
const LOCK_POLL: Duration = Duration::from_millis(250);
const SAVE_ATTEMPTS: usize = 3;

/// A usable sign-in replacing `stale`: renewed here, or adopted from the
/// `claude` session that renewed it first. `force` renews a token Anthropic
/// rejected even though it has not reached its expiry.
pub(super) fn renew(stale: &OAuthToken, force: bool) -> Option<OAuthToken> {
    let Some(_lock) = RefreshLock::acquire() else {
        // A `claude` session holds the lock while it renews; its result is
        // stored by the time the lock is released or abandoned.
        return Credentials::load()?
            .token()
            .filter(|token| token.access_token != stale.access_token);
    };
    renew_locked(
        &stale.access_token,
        force,
        utc_now(),
        Credentials::load,
        post_refresh,
        save,
    )
}

fn renew_locked<L, P, S>(
    stale_access_token: &str,
    force: bool,
    now: TimestampMs,
    load: L,
    post: P,
    save: S,
) -> Option<OAuthToken>
where
    L: Fn() -> Option<Credentials>,
    P: FnOnce(&OAuthToken, &str, TimestampMs) -> Option<RefreshedToken>,
    S: Fn(&Credentials, &RefreshedToken) -> bool,
{
    let stored = load()?;
    let token = stored.token()?;
    if token.access_token != stale_access_token && !token.is_expired(now) {
        return Some(token);
    }
    if !force && !token.expires_soon(now) {
        return Some(token);
    }
    let posted = token.refresh_token.clone()?;
    let refreshed = post(&token, &posted, now)?;
    let current = load()?;
    let current_token = current.token();
    let stored_refresh = current_token
        .as_ref()
        .and_then(|token| token.refresh_token.as_deref());
    if stored_refresh.is_some_and(|stored| stored != posted) {
        return current_token;
    }
    // A lost write leaves the rotated refresh token nowhere, so it is retried;
    // the renewed access token still serves this run either way.
    let _ = (0..SAVE_ATTEMPTS).any(|_| save(&current, &refreshed));
    Some(OAuthToken {
        access_token: refreshed.access_token,
        expires_at: Some(refreshed.expires_at),
        refresh_token: Some(refreshed.refresh_token),
        scopes: refreshed.scopes.unwrap_or(token.scopes),
        client_id: token.client_id,
    })
}

fn save(credentials: &Credentials, refreshed: &RefreshedToken) -> bool {
    credentials.save(refreshed)
}

fn post_refresh(
    token: &OAuthToken,
    refresh_token: &str,
    now: TimestampMs,
) -> Option<RefreshedToken> {
    let mut body = json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": token.client_id.as_deref().unwrap_or(CLIENT_ID),
    });
    if !token.scopes.is_empty() {
        body["scope"] = json!(token.scopes.join(" "));
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut response = agent
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            concat!("claude-code/", env!("CARGO_PKG_VERSION")),
        )
        .send(body.to_string())
        .ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .ok()?;
    parse_refresh(&text, refresh_token, now)
}

fn parse_refresh(body: &str, posted: &str, now: TimestampMs) -> Option<RefreshedToken> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let after = |key: &str| {
        let seconds = value.get(key).and_then(Value::as_i64)?;
        now.checked_add_millis(seconds.checked_mul(1000)?)
    };
    Some(RefreshedToken {
        access_token: text("access_token")?.to_string(),
        refresh_token: text("refresh_token").unwrap_or(posted).to_string(),
        expires_at: after("expires_in")?,
        refresh_token_expires_at: after("refresh_token_expires_in"),
        scopes: text("scope").map(|scope| scope.split_whitespace().map(str::to_string).collect()),
    })
}

/// Both of Claude Code's refresh locks, taken in its order and released on drop.
struct RefreshLock(Vec<PathBuf>);

impl RefreshLock {
    fn acquire() -> Option<Self> {
        let dir = home::home_dir()?.join(".claude");
        let current = dir.join(".oauth_refresh.lock");
        let legacy = PathBuf::from(format!(
            "{}.lock",
            fs::canonicalize(&dir).unwrap_or(dir).display()
        ));
        let deadline = Instant::now() + LOCK_WAIT;
        let mut held = Self(Vec::new());
        for path in [current, legacy] {
            if !lock_dir(&path, deadline) {
                return None;
            }
            held.0.push(path);
        }
        Some(held)
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        for path in self.0.iter().rev() {
            let _ = fs::remove_dir(path);
        }
    }
}

fn lock_dir(path: &Path, deadline: Instant) -> bool {
    loop {
        match fs::create_dir(path) {
            Ok(()) => return true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if is_stale(path) {
                    let _ = fs::remove_dir(path);
                    continue;
                }
            }
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(LOCK_POLL);
    }
}

fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > LOCK_STALE)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    fn at(seconds: i64) -> TimestampMs {
        TimestampMs::from_unix_seconds(1_800_000_000 + seconds).unwrap()
    }

    fn stored(access: &str, refresh: &str, expires_at: TimestampMs) -> Credentials {
        Credentials::from_value(json!({
            "claudeAiOauth": {
                "accessToken": access,
                "refreshToken": refresh,
                "expiresAt": expires_at.as_millis(),
                "scopes": ["user:inference", "user:profile"],
            }
        }))
    }

    fn refreshed(now: TimestampMs) -> RefreshedToken {
        RefreshedToken {
            access_token: "access-new".into(),
            refresh_token: "refresh-new".into(),
            expires_at: now.checked_add_millis(28_800_000).unwrap(),
            refresh_token_expires_at: None,
            scopes: None,
        }
    }

    #[test]
    fn renews_an_expired_sign_in_and_stores_it() {
        let posted = RefCell::new(None);
        let saved = Cell::new(0);
        let token = renew_locked(
            "access-old",
            false,
            at(10),
            || Some(stored("access-old", "refresh-old", at(0))),
            |token, refresh, now| {
                *posted.borrow_mut() = Some((token.scopes.join(" "), refresh.to_string()));
                Some(refreshed(now))
            },
            |_, refreshed| {
                saved.set(saved.get() + 1);
                refreshed.refresh_token == "refresh-new"
            },
        )
        .unwrap();
        assert_eq!(token.access_token, "access-new");
        assert_eq!(token.expires_at, at(10).checked_add_millis(28_800_000));
        assert_eq!(
            posted.into_inner(),
            Some(("user:inference user:profile".into(), "refresh-old".into()))
        );
        assert_eq!(saved.get(), 1);
    }

    #[test]
    fn adopts_a_sign_in_claude_renewed_first() {
        let token = renew_locked(
            "access-old",
            false,
            at(10),
            || Some(stored("access-claude", "refresh-claude", at(28_800))),
            |_, _, _| panic!("must not spend the refresh token"),
            |_, _| panic!("must not write"),
        )
        .unwrap();
        assert_eq!(token.access_token, "access-claude");
    }

    #[test]
    fn keeps_a_token_that_is_not_expiring_unless_forced() {
        let load = || Some(stored("access-old", "refresh-old", at(3_600)));
        let kept = renew_locked(
            "access-old",
            false,
            at(0),
            load,
            |_, _, _| None,
            |_, _| true,
        );
        assert_eq!(kept.unwrap().access_token, "access-old");
        let forced = renew_locked(
            "access-old",
            true,
            at(0),
            load,
            |_, _, now| Some(refreshed(now)),
            |_, _| true,
        );
        assert_eq!(forced.unwrap().access_token, "access-new");
    }

    #[test]
    fn never_overwrites_a_sign_in_rotated_during_the_request() {
        let reads = Cell::new(0);
        let token = renew_locked(
            "access-old",
            false,
            at(10),
            || {
                reads.set(reads.get() + 1);
                Some(if reads.get() == 1 {
                    stored("access-old", "refresh-old", at(0))
                } else {
                    stored("access-claude", "refresh-claude", at(28_800))
                })
            },
            |_, _, now| Some(refreshed(now)),
            |_, _| panic!("must not clobber Claude Code's newer sign-in"),
        )
        .unwrap();
        assert_eq!(token.access_token, "access-claude");
    }

    #[test]
    fn retries_a_failed_write_and_reports_a_refused_refresh() {
        let saves = Cell::new(0);
        let token = renew_locked(
            "access-old",
            false,
            at(10),
            || Some(stored("access-old", "refresh-old", at(0))),
            |_, _, now| Some(refreshed(now)),
            |_, _| {
                saves.set(saves.get() + 1);
                false
            },
        );
        assert_eq!(token.unwrap().access_token, "access-new");
        assert_eq!(saves.get(), SAVE_ATTEMPTS);

        let refused = renew_locked(
            "access-old",
            false,
            at(10),
            || Some(stored("access-old", "refresh-old", at(0))),
            |_, _, _| None,
            |_, _| panic!("nothing to write"),
        );
        assert_eq!(refused, None);
    }

    #[test]
    fn parses_the_token_endpoint_answer() {
        let token = parse_refresh(
            r#"{"access_token":"a","refresh_token":"r","expires_in":28800,"refresh_token_expires_in":2592000,"scope":"user:inference user:profile"}"#,
            "posted",
            at(0),
        )
        .unwrap();
        assert_eq!(token.expires_at, at(28_800));
        assert_eq!(token.refresh_token_expires_at, Some(at(2_592_000)));
        assert_eq!(
            token.scopes.as_deref(),
            Some(&["user:inference".to_string(), "user:profile".to_string()][..])
        );

        let kept =
            parse_refresh(r#"{"access_token":"a","expires_in":60}"#, "posted", at(0)).unwrap();
        assert_eq!(kept.refresh_token, "posted");
        assert_eq!(kept.scopes, None);
        assert!(parse_refresh(r#"{"error":"invalid_grant"}"#, "posted", at(0)).is_none());
    }

    #[test]
    fn waits_for_a_held_lock_and_takes_over_a_stale_one() {
        let dir = std::env::temp_dir().join(format!("agent-burn-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let lock = dir.join(".oauth_refresh.lock");

        fs::create_dir(&lock).unwrap();
        assert!(!lock_dir(&lock, Instant::now()));

        let old = SystemTime::now() - Duration::from_secs(120);
        open_dir_for_times(&lock).set_modified(old).unwrap();
        assert!(lock_dir(&lock, Instant::now()));
        assert!(!is_stale(&lock));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(not(windows))]
    fn open_dir_for_times(path: &Path) -> fs::File {
        fs::File::open(path).unwrap()
    }

    /// Windows opens a directory handle only with `FILE_FLAG_BACKUP_SEMANTICS`,
    /// and changing its times needs `FILE_WRITE_ATTRIBUTES`.
    #[cfg(windows)]
    fn open_dir_for_times(path: &Path) -> fs::File {
        use std::os::windows::fs::OpenOptionsExt;

        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    }
}
