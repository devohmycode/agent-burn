//! Keeps secrets and account identifiers out of the messages the
//! application shows and saves: the engine's errors end up on screen and in
//! `quota-state.json`. Tokens, JWTs, `Bearer` values, sensitive `key=value`
//! pairs, e-mail addresses and identifiers are replaced, and a message is cut
//! to a few hundred characters.

/// Longest message kept, in characters.
const MAX_CHARS: usize = 400;
/// A run of token characters this long, with letters and digits, is a secret.
const LONG_SECRET: usize = 32;
const PREFIXES: [&str; 12] = [
    "sk-",
    "sk_",
    "rk_",
    "pk_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "AKIA",
];
const SENSITIVE_KEYS: [&str; 13] = [
    "token",
    "access_token",
    "refresh_token",
    "id_token",
    "session",
    "sessionid",
    "cookie",
    "password",
    "secret",
    "api_key",
    "apikey",
    "authorization",
    "auth",
];

fn token_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || "-_.=+/@:%".contains(character)
}

fn is_uuid(word: &str) -> bool {
    word.len() == 36
        && word.char_indices().all(|(index, character)| match index {
            8 | 13 | 18 | 23 => character == '-',
            _ => character.is_ascii_hexdigit(),
        })
}

fn is_email(word: &str) -> bool {
    let Some((name, domain)) = word.split_once('@') else {
        return false;
    };
    !name.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// What replaces `word`, if it is secret.
fn replacement(word: &str) -> Option<String> {
    let trimmed = word.trim_end_matches(['.', ':', ',']);
    if let Some((key, value)) = trimmed.split_once(['=', ':'])
        && !value.is_empty()
        && SENSITIVE_KEYS.contains(&key.to_ascii_lowercase().as_str())
    {
        return Some(format!("{key}=[redacted]"));
    }
    if is_email(trimmed) {
        return Some("[email]".into());
    }
    if is_uuid(trimmed) {
        return Some("[id]".into());
    }
    if trimmed.starts_with("eyJ") && trimmed.contains('.') && trimmed.len() >= 20 {
        return Some("[token]".into());
    }
    if PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix)) && trimmed.len() >= 12 {
        return Some("[token]".into());
    }
    let letters = trimmed.chars().any(|c| c.is_ascii_alphabetic());
    let digits = trimmed.chars().any(|c| c.is_ascii_digit());
    if trimmed.len() >= LONG_SECRET && letters && digits && !trimmed.contains(':') {
        return Some("[redacted]".into());
    }
    None
}

/// `text` without secrets or account identifiers, cut to `MAX_CHARS`.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let mut previous = String::new();
    let flush = |word: &mut String, out: &mut String, previous: &mut String| {
        if word.is_empty() {
            return;
        }
        let secret =
            if previous.eq_ignore_ascii_case("bearer") || previous.eq_ignore_ascii_case("basic") {
                Some("[token]".to_string())
            } else {
                replacement(word)
            };
        out.push_str(secret.as_deref().unwrap_or(word));
        *previous = std::mem::take(word);
    };
    for character in text.chars() {
        if token_char(character) {
            word.push(character);
        } else {
            flush(&mut word, &mut out, &mut previous);
            out.push(character);
        }
    }
    flush(&mut word, &mut out, &mut previous);
    if out.chars().count() > MAX_CHARS {
        let cut: String = out.chars().take(MAX_CHARS).collect();
        format!("{cut}…")
    } else {
        out
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Fake secrets of each kind, as an engine error could print them.
    pub const SECRETS: [&str; 7] = [
        "sk-ant-oat01-FAKEc0ffee1234567890abcdef",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJsZWEifQ.c2lnbmF0dXJl",
        "lea.martin@example.com",
        "3f2a9c1e-7b4d-4e8a-9f00-1234abcd5678",
        "ghp_FAKE0123456789abcdefghij",
        // spellchecker:off
        "c2Vzc2lvbi1zZWNyZXQtdmFsdWUtMTIzNDU2Nzg5MA",
        // spellchecker:on
        "cookie-value-42",
    ];

    pub fn leaked(text: &str) -> Vec<&'static str> {
        SECRETS
            .iter()
            .copied()
            .filter(|secret| text.contains(secret))
            .collect()
    }

    #[test]
    fn secrets_and_identifiers_are_replaced_and_the_rest_kept() {
        let message = format!(
            "401 Unauthorized for {} with Bearer {} (token={}, account {}, cookie={}); \
             refresh with {} at https://api.example.com/v1/usage; key {}",
            SECRETS[2], SECRETS[1], SECRETS[0], SECRETS[3], SECRETS[6], SECRETS[4], SECRETS[5]
        );

        let shown = redact(&message);

        assert!(leaked(&shown).is_empty(), "{shown}");
        assert!(
            shown.starts_with("401 Unauthorized for [email] with Bearer [token] (token=[redacted]"),
            "{shown}"
        );
        assert!(
            shown.contains("https://api.example.com/v1/usage"),
            "{shown}"
        );
    }

    #[test]
    fn ordinary_messages_and_paths_stay_as_they_are() {
        for message in [
            "engine not found: C:\\Program Files\\Agent Burn\\agent-burn.exe",
            "No valid Claude data directories found",
            "engine timed out",
            "The live quota could not be read (offline). The last reading is kept.",
        ] {
            assert_eq!(redact(message), message);
        }
    }

    #[test]
    fn long_messages_are_cut() {
        let long = "warning ".repeat(200);
        let shown = redact(&long);
        assert_eq!(shown.chars().count(), MAX_CHARS + 1);
        assert!(shown.ends_with('…'));
    }
}
