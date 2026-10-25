//! Masking credentials in everything this crate persists.
//!
//! A QD template usually keeps its session in the query string
//! (`?token=...&sign=...`). The step rows, the run log and the failure trace are
//! all written to the database, then rendered in the run table, included in an
//! exported backup and pasted into issue reports -- so the one place a
//! deployment's own credentials never need to appear is exactly where they used
//! to sit in plain text.
//!
//! Redaction happens on the way *into* the database, not on the way out. The
//! executor keeps the real values, so a template that reads a token back still
//! gets the token; only the copy at rest is masked. Masking at render time
//! instead would leave the plain text in every backup and in every SQL dump.
//!
//! The rules are deliberately narrow, because over-masking a query string costs
//! nothing but under-masking costs a session:
//!
//! - the userinfo of a URL (`https://user:pass@host`) is masked entirely;
//! - a query parameter whose name looks like a credential is masked;
//! - a `Bearer` credential quoted in an error message is masked.
//!
//! Request headers and bodies are not persisted anywhere in this project, which
//! is why there is no rule for them here.

/// Words a query parameter name is matched against, after all separators are
/// stripped (`access_token`, `access-token` and `accessToken` all normalise to
/// `accesstoken`).
const SECRET_WORDS: [&str; 18] = [
    "token",
    "key",
    "sign",
    "signature",
    "sig",
    "secret",
    "password",
    "passwd",
    "pwd",
    "session",
    "sessionid",
    "cookie",
    "auth",
    "authorization",
    "credential",
    "ticket",
    "captcha",
    "code",
];

/// The value substituted for a masked secret. Chosen so that a second pass over
/// already-masked text changes nothing.
pub const MASK: &str = "***";

/// Mask credentials in `text`, leaving everything else byte-for-byte intact.
///
/// Idempotent: masking an already-masked string returns it unchanged, so a
/// value can be redacted both where it is formatted and again where it is
/// stored without the second pass doing damage.
pub fn redact(text: &str) -> String {
    let url_safe = redact_urls(text);
    redact_bearer(&url_safe)
}

/// Walk the text for `://` and rewrite each URL body that follows.
///
/// A byte scan is safe on UTF-8: every delimiter this looks for is ASCII, and
/// no byte of a multi-byte character can equal one.
fn redact_urls(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == b':' && bytes[index + 1] == b'/' && bytes[index + 2] == b'/' {
            let start = index + 3;
            // Copy through the scheme separator, then rewrite only the body.
            out.push_str(&text[copied..start]);
            let mut end = start;
            while end < bytes.len() && !ends_url(bytes[end]) {
                end += 1;
            }
            out.push_str(&redact_url_body(&text[start..end]));
            index = end;
            copied = end;
            continue;
        }
        index += 1;
    }
    out.push_str(&text[copied..]);
    out
}

/// Bytes that end a URL inside a free-form sentence or log line.
fn ends_url(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'"' | b'\'' | b'<' | b'>' | b'`' | b'\\' | b'|' | b',' | b';'
        )
}

/// Mask one URL body, i.e. everything after `scheme://`.
fn redact_url_body(body: &str) -> String {
    // Authority ends at the first path, query or fragment delimiter.
    let authority_end = body.find(['/', '?', '#']).unwrap_or(body.len());
    let (authority, rest) = body.split_at(authority_end);
    // `user:password@host` -- keep the host, drop the credentials.
    let authority = match authority.rfind('@') {
        Some(at) => format!("{MASK}{}", &authority[at..]),
        None => authority.to_string(),
    };
    // Split off the query without disturbing the fragment.
    let (path, fragment) = match rest.find('#') {
        Some(hash) => rest.split_at(hash),
        None => (rest, ""),
    };
    let (path, query) = match path.find('?') {
        Some(mark) => path.split_at(mark),
        None => (path, ""),
    };
    format!(
        "{authority}{path}{}{fragment}",
        redact_query(query.trim_start_matches('?'))
    )
}

fn redact_query(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }
    let masked: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) if is_secret_name(name) => format!("{name}={MASK}"),
            _ => pair.to_string(),
        })
        .collect();
    format!("?{}", masked.join("&"))
}

/// Whether a query parameter name is one whose value must not be stored.
///
/// Matching is by suffix as well as equality, because `access_token`,
/// `refresh_token` and `csrf_token` are the shapes that actually appear in
/// templates. The cost is that an unusual name such as `monkey` is masked too;
/// a masked value nobody reads is a much smaller problem than a leaked one.
fn is_secret_name(name: &str) -> bool {
    let normalised: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if normalised.is_empty() {
        return false;
    }
    SECRET_WORDS
        .iter()
        .any(|word| normalised == *word || normalised.ends_with(word))
}

/// Mask the credential in `Bearer <token>` wherever it is quoted.
fn redact_bearer(text: &str) -> String {
    const MARKER: &str = "Bearer ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = rest.find(MARKER) {
        let after = found + MARKER.len();
        out.push_str(&rest[..after]);
        let tail = &rest[after..];
        let token_len = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | ')' | '}'))
            .unwrap_or(tail.len());
        // `Bearer ***` is already masked; copying it keeps this idempotent.
        if token_len == 0 || &tail[..token_len] == MASK {
            out.push_str(&tail[..token_len]);
        } else {
            out.push_str(MASK);
        }
        rest = &tail[token_len..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_credential_query_parameters() {
        assert_eq!(
            redact("GET https://example.com/checkin?token=abc123&sign=deadbeef&page=2"),
            "GET https://example.com/checkin?token=***&sign=***&page=2"
        );
    }

    #[test]
    fn matches_names_by_suffix_and_case() {
        assert_eq!(
            redact("https://x.test/p?access_token=a&Access-Token=b&refresh_token=c&cursor=9"),
            "https://x.test/p?access_token=***&Access-Token=***&refresh_token=***&cursor=9"
        );
    }

    #[test]
    fn masks_url_userinfo() {
        assert_eq!(
            redact("https://user:pa55@example.com/hook"),
            "https://***@example.com/hook"
        );
    }

    #[test]
    fn keeps_the_fragment_and_the_rest_of_the_sentence() {
        assert_eq!(
            redact("see https://x.test/a?key=1#section for details"),
            "see https://x.test/a?key=***#section for details"
        );
    }

    #[test]
    fn masks_bearer_credentials_in_error_text() {
        assert_eq!(
            redact("upstream said: unauthorized (Authorization: Bearer abc.def-ghi)"),
            "upstream said: unauthorized (Authorization: Bearer ***)"
        );
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        let text = "step 1  GET https://example.com/path?page=2\n        HTTP 200 · 12 bytes";
        assert_eq!(redact(text), text);
        assert_eq!(redact("connection refused"), "connection refused");
    }

    /// A backup can be redacted twice (once where it is formatted, once where it
    /// is stored). That must not compound into `******` or re-mask the host.
    #[test]
    fn is_idempotent() {
        for original in [
            "https://user:pa55@example.com/hook?token=abc&page=1",
            "Authorization: Bearer abc.def",
            "https://x.test/a?key=***",
        ] {
            let once = redact(original);
            assert_eq!(redact(&once), once, "second pass changed: {once}");
        }
    }

    /// The trace is plain text copied out of a dialog, so the scan must survive
    /// a URL that is the very last thing in the string, with no terminator.
    #[test]
    fn handles_url_at_end_of_input() {
        assert_eq!(
            redact("https://x.test/a?sign=1"),
            "https://x.test/a?sign=***"
        );
        assert_eq!(redact("https://x.test"), "https://x.test");
    }
}
