pub mod file;
pub mod slack;
pub mod utils;

pub use utils::*;

pub const SLACK_TOKEN_ENV: &str = "SLACK_BOT_TOKEN";
