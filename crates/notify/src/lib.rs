//! Outgoing messages: email today, with Slack, Discord and signed webhooks to follow, and the
//! encryption that keeps channel targets (webhook URLs and secrets) safe at rest.

pub mod crypto;
pub mod email;

pub use crypto::{ChannelKey, CryptoError};
pub use email::{Email, MailError, Mailer};
