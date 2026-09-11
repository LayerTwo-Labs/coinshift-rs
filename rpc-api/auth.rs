//! Cookie authentication for the JSON-RPC server, in the style of Bitcoin
//! Core's `.cookie` file.
//!
//! On start the server writes a random token to `rpc.cookie` in its data
//! directory (mode 0600) as `__cookie__:<token>`. A client reads that file
//! and sends the pair as HTTP basic auth (or the token alone as a bearer
//! token). Anything that can read the file is, by construction, running as
//! the user who owns the wallet; anything else gets `401`.

use std::path::Path;

use base64::Engine as _;
use http::HeaderValue;

/// File name of the cookie, relative to the data directory.
pub const COOKIE_FILE_NAME: &str = "rpc.cookie";

/// User name recorded in the cookie file.
pub const COOKIE_USER: &str = "__cookie__";

/// Parse a cookie file's contents into `(user, secret)`.
pub fn parse_cookie(contents: &str) -> Option<(String, String)> {
    let (user, secret) = contents.trim().split_once(':')?;
    if user.is_empty() || secret.is_empty() {
        return None;
    }
    Some((user.to_owned(), secret.to_owned()))
}

/// Read `(user, secret)` from a cookie file.
pub fn read_cookie(path: &Path) -> std::io::Result<(String, String)> {
    let contents = std::fs::read_to_string(path)?;
    parse_cookie(&contents).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: expected `user:secret` on one line", path.display()),
        )
    })
}

/// `Authorization` header value for HTTP basic auth.
pub fn basic_auth_header(user: &str, secret: &str) -> HeaderValue {
    let encoded = base64::engine::general_purpose::STANDARD
        .encode(format!("{user}:{secret}"));
    let mut value = HeaderValue::from_str(&format!("Basic {encoded}"))
        .expect("base64 output is always a valid header value");
    value.set_sensitive(true);
    value
}

/// Constant-time byte comparison, so a wrong token cannot be found one byte
/// at a time from response timing.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Whether an `Authorization` header value proves possession of the cookie.
///
/// Accepts `Basic base64(user:secret)` with the cookie's user and secret, or
/// `Bearer <secret>`.
pub fn authorizes(
    header: Option<&HeaderValue>,
    user: &str,
    secret: &str,
) -> bool {
    let Some(header) = header.and_then(|h| h.to_str().ok()) else {
        return false;
    };
    if let Some(token) = header.strip_prefix("Bearer ") {
        return ct_eq(token.trim().as_bytes(), secret.as_bytes());
    }
    if let Some(encoded) = header.strip_prefix("Basic ") {
        let Ok(decoded) =
            base64::engine::general_purpose::STANDARD.decode(encoded.trim())
        else {
            return false;
        };
        let expected = format!("{user}:{secret}");
        return ct_eq(&decoded, expected.as_bytes());
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_and_bearer_forms_are_accepted() {
        let header = basic_auth_header(COOKIE_USER, "s3cret");
        assert!(authorizes(Some(&header), COOKIE_USER, "s3cret"));
        let bearer = HeaderValue::from_static("Bearer s3cret");
        assert!(authorizes(Some(&bearer), COOKIE_USER, "s3cret"));
    }

    #[test]
    fn wrong_or_missing_credentials_are_rejected() {
        assert!(!authorizes(None, COOKIE_USER, "s3cret"));
        let wrong = basic_auth_header(COOKIE_USER, "s3cre");
        assert!(!authorizes(Some(&wrong), COOKIE_USER, "s3cret"));
        let wrong_user = basic_auth_header("alice", "s3cret");
        assert!(!authorizes(Some(&wrong_user), COOKIE_USER, "s3cret"));
        let garbage = HeaderValue::from_static("Basic not-base64!");
        assert!(!authorizes(Some(&garbage), COOKIE_USER, "s3cret"));
        let bearer = HeaderValue::from_static("Bearer other");
        assert!(!authorizes(Some(&bearer), COOKIE_USER, "s3cret"));
    }

    #[test]
    fn cookie_round_trip() {
        let dir = temp_dir::TempDir::new().unwrap();
        let path = dir.path().join(COOKIE_FILE_NAME);
        std::fs::write(&path, "__cookie__:abc123\n").unwrap();
        assert_eq!(
            read_cookie(&path).unwrap(),
            (COOKIE_USER.to_owned(), "abc123".to_owned())
        );
        std::fs::write(&path, "garbage").unwrap();
        assert!(read_cookie(&path).is_err());
    }
}
