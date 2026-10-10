//! Who may use the desktop: whoever holds the session token.
//!
//! Binding to localhost was never the whole answer. A browser opens a
//! WebSocket to `127.0.0.1` from any page on the internet, and the handshake is
//! not subject to CORS, so any site the user visited could have driven the
//! desktop. Every request, the socket upgrade included, now has to carry the
//! token as a cookie.
//!
//! The page sets that cookie from `#token=` in its own URL. A fragment is never
//! sent in a request or written to a log.
//!
//! The cookie is `SameSite=Strict`, which keeps other sites from having it
//! attached, but a site is not an origin: cookies ignore the port, so any other
//! web app on `127.0.0.1` is the same site and would get it. So `Origin`, which
//! a browser always sends on a socket upgrade and a page cannot forge, is
//! checked too: a request that has one must come from the host it was
//! addressed to.
//!
//! One token per user, kept in the config directory so a bookmarked link
//! survives a restart. Deleting the file revokes it; the next start makes a
//! new one.

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::{COOKIE, HOST, ORIGIN};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The cookie the frontend stores the token in. Kept in step with
/// `frontend/src/desktop/connect.rs` by hand: it is one string.
pub const COOKIE_NAME: &str = "webland_token";

/// The token, read from the config directory or made and saved there.
///
/// # Errors
/// When the file can be neither read nor written, or the system has no
/// randomness to give.
pub fn load() -> std::io::Result<String> {
    load_in(&crate::wallpaper::config_dir())
}

/// [`load`], against a directory of the caller's choosing.
///
/// # Errors
/// As [`load`].
pub fn load_in(dir: &Path) -> std::io::Result<String> {
    let path = dir.join("token");
    if let Ok(saved) = std::fs::read_to_string(&path) {
        let saved = saved.trim();
        if !saved.is_empty() {
            return Ok(saved.to_owned());
        }
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let token = bytes.iter().fold(String::new(), |mut hex, byte| {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    std::fs::create_dir_all(dir)?;
    // Owner-only from the moment it exists: the token is the desktop.
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?
        .write_all(token.as_bytes())?;
    Ok(token)
}

/// Middleware: pass the request on if it carries the token, refuse it if not.
pub async fn require(State(token): State<Arc<str>>, request: Request, next: Next) -> Response {
    if !same_origin(request.headers()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if presented(&request).is_some_and(|given| same(given.as_bytes(), token.as_bytes())) {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Whether the page that made the request is on the host it was sent to.
///
/// No `Origin` is not a browser page, and the token alone decides. Behind a
/// proxy the host the browser addressed is `X-Forwarded-Host` (trunk's proxy
/// rewrites `Host`); a page cannot set either on a socket upgrade.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) else {
        return true;
    };
    let addressed = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(HOST))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim);
    // `null` (a sandboxed or opaque origin) has no host, and never matches.
    let from = origin.split_once("://").map(|(_, host)| host);
    from.is_some() && from == addressed
}

/// The token in the request's cookies, if there is one.
fn presented(request: &Request) -> Option<&str> {
    request
        .headers()
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(COOKIE_NAME)?.strip_prefix('='))
}

/// Equal, in time that does not depend on where the first difference is, so
/// the token cannot be guessed a byte at a time by timing refusals.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::{load_in, same, same_origin};
    use axum::http::HeaderMap;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn origin_must_be_the_addressed_host() {
        // The page itself, through trunk's proxy and straight to the backend.
        assert!(same_origin(&headers(&[
            ("origin", "http://127.0.0.1:3030"),
            ("host", "127.0.0.1"),
            ("x-forwarded-host", "127.0.0.1:3030"),
        ])));
        assert!(same_origin(&headers(&[
            ("origin", "http://127.0.0.1:9001"),
            ("host", "127.0.0.1:9001"),
        ])));
        // Not a browser page: the token decides.
        assert!(same_origin(&headers(&[("host", "127.0.0.1:9001")])));
        // Another site, including another name for the same machine.
        assert!(!same_origin(&headers(&[
            ("origin", "http://localhost:3030"),
            ("x-forwarded-host", "127.0.0.1:3030"),
        ])));
        assert!(!same_origin(&headers(&[
            ("origin", "https://evil.example"),
            ("host", "127.0.0.1:9001"),
        ])));
        assert!(!same_origin(&headers(&[
            ("origin", "null"),
            ("host", "127.0.0.1:9001"),
        ])));
    }

    #[test]
    fn token_is_made_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("webland-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = load_in(&dir).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(load_in(&dir).unwrap(), first);
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(dir.join("token")).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn comparison_is_exact() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"abcd"));
        assert!(!same(b"", b"a"));
    }
}
