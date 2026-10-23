mod auth;
mod body;
mod environment;
mod http;
pub mod key_value;
mod method;
mod request;
mod response;
mod variables;

pub use auth::{ApiKeyLocation, Auth};
pub use body::{Body, MultipartPart, PartValue};
pub use environment::Environment;
pub use http::{ClientOptions, HttpClient, SendError};
pub use key_value::KeyValue;
pub use method::{InvalidMethod, Method};
pub use request::{Request, Resolved};
pub use response::Response;
pub use variables::{DYNAMIC_VARIABLES, Interpolated, Segment, Variables, is_dynamic, segments};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
