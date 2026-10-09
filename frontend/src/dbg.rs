//! Debug logging sink for Webland frontend.
//!
//! High-frequency logging is disabled by default to eliminate GC churn and DOM stalls.

#[inline]
#[allow(dead_code)]
pub fn log(_s: impl AsRef<str>) {}
