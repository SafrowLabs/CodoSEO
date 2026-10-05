//! Outgoing messages: email, Slack, Discord and signed webhooks, plus the encryption that keeps
//! channel targets (webhook URLs and secrets) safe at rest.

pub mod crypto;
pub mod deliver;
pub mod discord;
pub mod email;
pub mod message;
pub mod slack;
pub mod webhook;

pub use crypto::{ChannelKey, CryptoError};
pub use deliver::{
    ChannelKind, ChannelTarget, DeliveryError, TargetError, deliver, guarded_client,
    validate_target,
};
pub use email::{Email, MailError, Mailer};
pub use message::{AlertItem, AlertKind, AlertMessage};
