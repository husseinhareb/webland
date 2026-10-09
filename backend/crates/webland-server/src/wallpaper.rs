//! Server-side wallpaper persistence and HTTP endpoints.
//!
//! Stores the desktop background image in `$XDG_CONFIG_HOME/webland/wallpaper`,
//! and serves it via `/api/wallpaper` with HTTP caching. When updated or
//! cleared, broadcasts the change to all connected browser sessions.

use std::path::PathBuf;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use webland_protocol::ServerMessage;

use crate::transport::FrameSink;

/// Get the configuration directory for Webland.
#[must_use]
pub fn config_dir() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map_or_else(
            |_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| String::from("."));
                PathBuf::from(home).join(".config")
            },
            PathBuf::from,
        )
        .join("webland")
}

fn wallpaper_file_in(dir: &std::path::Path) -> PathBuf {
    dir.join("wallpaper")
}

fn mime_file_in(dir: &std::path::Path) -> PathBuf {
    dir.join("wallpaper.mime")
}

/// The URL for the current wallpaper, if one exists on disk.
#[must_use]
pub fn load_url() -> Option<String> {
    load_url_in(&config_dir())
}

#[must_use]
pub fn load_url_in(dir: &std::path::Path) -> Option<String> {
    let path = wallpaper_file_in(dir);
    let metadata = std::fs::metadata(path).ok()?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(format!("/api/wallpaper?v={mtime}"))
}

/// Read the wallpaper bytes and MIME type from disk.
#[must_use]
pub fn read() -> Option<(String, Vec<u8>)> {
    read_from(&config_dir())
}

#[must_use]
pub fn read_from(dir: &std::path::Path) -> Option<(String, Vec<u8>)> {
    let bytes = std::fs::read(wallpaper_file_in(dir)).ok()?;
    let mime = std::fs::read_to_string(mime_file_in(dir))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| detect_mime(&bytes).to_string());
    Some((mime, bytes))
}

/// Save wallpaper bytes and MIME type to disk atomically.
///
/// # Errors
/// Returns [`std::io::Error`] if creating directories or writing fails.
pub fn save(bytes: &[u8], mime: Option<&str>) -> std::io::Result<String> {
    save_in(&config_dir(), bytes, mime)
}

/// Save wallpaper bytes and MIME type into `dir` atomically.
///
/// # Errors
/// Returns [`std::io::Error`] if creating directories or writing fails.
pub fn save_in(dir: &std::path::Path, bytes: &[u8], mime: Option<&str>) -> std::io::Result<String> {
    std::fs::create_dir_all(dir)?;
    let mime_type = mime
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "application/octet-stream")
        .unwrap_or_else(|| detect_mime(bytes));

    let temp_file = dir.join(format!("wallpaper.tmp.{}", std::process::id()));
    std::fs::write(&temp_file, bytes)?;
    std::fs::rename(&temp_file, wallpaper_file_in(dir))?;
    let _ = std::fs::write(mime_file_in(dir), mime_type);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Ok(format!("/api/wallpaper?v={now}"))
}

/// Remove the wallpaper and its MIME file from disk.
pub fn clear() {
    clear_in(&config_dir());
}

pub fn clear_in(dir: &std::path::Path) {
    let _ = std::fs::remove_file(wallpaper_file_in(dir));
    let _ = std::fs::remove_file(mime_file_in(dir));
}

fn detect_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<svg") {
        "image/svg+xml"
    } else {
        "image/jpeg"
    }
}

/// HTTP handler: `GET /api/wallpaper`
pub async fn get_wallpaper() -> Response {
    tokio::task::spawn_blocking(read)
        .await
        .ok()
        .flatten()
        .map_or_else(
            || StatusCode::NOT_FOUND.into_response(),
            |(mime, bytes)| {
                (
                    StatusCode::OK,
                    [
                        (CONTENT_TYPE, mime),
                        (CACHE_CONTROL, "public, max-age=31536000".to_string()),
                    ],
                    bytes,
                )
                    .into_response()
            },
        )
}

/// HTTP handler: `POST /api/wallpaper`
pub async fn post_wallpaper(
    State(sink): State<FrameSink>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty image payload".to_string()));
    }
    let mime = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let url = tokio::task::spawn_blocking(move || save(&body, mime.as_deref()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sink.emit(ServerMessage::Wallpaper { url: Some(url) });
    Ok(StatusCode::OK)
}

/// HTTP handler: `DELETE /api/wallpaper`
pub async fn delete_wallpaper(State(sink): State<FrameSink>) -> StatusCode {
    let _ = tokio::task::spawn_blocking(clear).await;
    sink.emit(ServerMessage::Wallpaper { url: None });
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_detection_recognizes_formats() {
        assert_eq!(detect_mime(b"\x89PNG\r\n\x1a\n..."), "image/png");
        assert_eq!(detect_mime(b"\xff\xd8\xff..."), "image/jpeg");
        assert_eq!(detect_mime(b"RIFF\0\0\0\0WEBP..."), "image/webp");
        assert_eq!(detect_mime(b"GIF89a..."), "image/gif");
        assert_eq!(detect_mime(b"<svg xmlns=...>"), "image/svg+xml");
    }

    #[test]
    fn wallpaper_save_read_and_clear_cycle() {
        let scratch_dir = std::env::temp_dir().join(format!("webland-test-{}", std::process::id()));

        let dummy_png = b"\x89PNG\r\n\x1a\nfake-pixels";
        let url = save_in(&scratch_dir, dummy_png, Some("image/png")).expect("save should succeed");
        assert!(url.starts_with("/api/wallpaper?v="));

        let loaded_url = load_url_in(&scratch_dir);
        assert!(loaded_url.is_some());

        let (mime, bytes) = read_from(&scratch_dir).expect("read should find saved wallpaper");
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, dummy_png);

        clear_in(&scratch_dir);
        assert!(load_url_in(&scratch_dir).is_none());
        assert!(read_from(&scratch_dir).is_none());

        let _ = std::fs::remove_dir_all(&scratch_dir);
    }
}
