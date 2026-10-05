//! Sending login emails. The mailers live in `codoseo-notify` (log, capture for tests, SMTP);
//! this module re-exports them so the web crate and its tests keep one import path.

pub use codoseo_notify::{Email, MailError, Mailer};
