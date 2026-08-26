//! Shared types and errors for the Webland backend.
//!
//! Nothing is implemented yet; this crate exists to hold the vocabulary
//! (ids, geometry, errors) that the other backend crates will share.

/// Errors surfaced by the Webland backend.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Placeholder variant; replaced as real failure modes appear.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Convenience alias used across the backend crates.
pub type Result<T> = std::result::Result<T, Error>;
