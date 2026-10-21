//! Core engine of cornu.
//!
//! This crate has no UI dependency: the GUI and the CLI are both built on top of it.

/// Version of the cornu engine.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert!(!VERSION.is_empty());
    }
}
