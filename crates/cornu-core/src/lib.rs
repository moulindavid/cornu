//! Core engine of cornu.
//!
//! This crate has no UI dependency: the GUI and the CLI are both built on top of it.

mod body;
pub mod key_value;
mod method;
mod request;
mod response;

pub use body::{Body, MultipartPart, PartValue};
pub use key_value::KeyValue;
pub use method::{InvalidMethod, Method};
pub use request::Request;
pub use response::Response;

/// Version of the cornu engine.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
