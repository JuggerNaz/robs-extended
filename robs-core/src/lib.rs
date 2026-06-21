pub mod types;
pub mod traits;
pub mod error;
pub mod pipeline;
pub mod event;
pub mod registry;
pub mod context;
pub mod scene;
pub mod annotation;

pub use types::*;
pub use traits::*;
pub use error::*;
pub use pipeline::*;
pub use event::*;
pub use registry::*;
pub use context::*;
pub use scene::*;
pub use annotation::*;

pub const ROBS_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ROBS_NAME: &str = "ROBS";