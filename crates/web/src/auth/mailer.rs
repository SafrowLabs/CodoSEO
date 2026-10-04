//! Sending login emails. M5 has no SMTP yet (M7's notify crate adds it): the log mailer prints
//! the message, which is what self-hosters without an email server use anyway (spec section 8:
//! "without an email server, login links are printed to the logs"). Tests capture messages.

use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    pub to: String,
    pub subject: String,
    pub text: String,
}

#[derive(Clone, Default)]
pub enum Mailer {
    /// Prints every message to the log.
    #[default]
    Log,
    /// Keeps every message in memory, for tests.
    Capture(Arc<Mutex<Vec<Email>>>),
}

impl Mailer {
    pub fn capture() -> (Mailer, Arc<Mutex<Vec<Email>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (Mailer::Capture(sent.clone()), sent)
    }

    pub async fn send(&self, email: Email) {
        match self {
            Mailer::Log => {
                tracing::info!(to = %email.to, subject = %email.subject, "email (no SMTP configured)");
                // The login link must be findable even when tracing isn't initialised.
                println!(
                    "\n── email to {} ──\n{}\n{}\n",
                    email.to, email.subject, email.text
                );
            }
            Mailer::Capture(sent) => sent.lock().expect("mailer lock").push(email),
        }
    }
}
