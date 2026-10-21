mod body;
mod http;
pub mod key_value;
mod method;
mod request;
mod response;
mod variables;

pub use body::{Body, MultipartPart, PartValue};
pub use http::{ClientOptions, HttpClient, SendError};
pub use key_value::KeyValue;
pub use method::{InvalidMethod, Method};
pub use request::{Request, Resolved};
pub use response::Response;
pub use variables::{Interpolated, Segment, Variables, segments};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
