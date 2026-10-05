//! Sending email. The log mailer prints the message, which is what self-hosters without an email
//! server use (spec section 8: "without an email server, login links are printed to the logs");
//! the SMTP mailer sends through `SMTP_URL`; tests capture messages.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lettre::message::{Mailbox, MultiPart};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    pub to: String,
    pub subject: String,
    pub text: String,
    /// When set, the message goes out as multipart/alternative with `text` as the plain part.
    pub html: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    /// `SMTP_URL` or `MAIL_FROM` is unusable; raised at startup.
    #[error("{0}")]
    Config(String),
    /// A recipient address that can't be parsed.
    #[error("invalid email address {0:?}")]
    Address(String),
    #[error("could not build the message: {0}")]
    Build(String),
    #[error("could not send the message: {0}")]
    Send(String),
}

/// How long one SMTP connect or command may take before the send fails (and the job retries).
/// lettre's own default is 60 s, long enough to hold the single job runner on a hung server.
pub const SMTP_TIMEOUT: Duration = Duration::from_secs(20);

/// A configured SMTP connection.
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    timeout: Duration,
}

#[derive(Clone, Default)]
pub enum Mailer {
    /// Prints every message to the log.
    #[default]
    Log,
    /// Keeps every message in memory, for tests.
    Capture(Arc<Mutex<Vec<Email>>>),
    /// Sends through an SMTP server.
    Smtp(Arc<SmtpMailer>),
}

impl Mailer {
    pub fn capture() -> (Mailer, Arc<Mutex<Vec<Email>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (Mailer::Capture(sent.clone()), sent)
    }

    /// The mailer for `SMTP_URL` (`smtps://user:pass@host:465`, `smtp://host:587?tls=required`);
    /// no URL means the log mailer. `from` is the `MAIL_FROM` sender, e.g.
    /// `CodoSEO <hello@codoseo.com>`.
    pub fn from_config(smtp_url: Option<&str>, from: &str) -> Result<Mailer, MailError> {
        Mailer::from_config_with_timeout(smtp_url, from, SMTP_TIMEOUT)
    }

    /// [`from_config`](Self::from_config) with an explicit connect/command timeout.
    fn from_config_with_timeout(
        smtp_url: Option<&str>,
        from: &str,
        timeout: Duration,
    ) -> Result<Mailer, MailError> {
        let from: Mailbox = from
            .parse()
            .map_err(|e| MailError::Config(format!("MAIL_FROM is invalid: {e}")))?;
        let Some(url) = smtp_url else {
            return Ok(Mailer::Log);
        };
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .map_err(|e| MailError::Config(format!("SMTP_URL is invalid: {e}")))?
            .timeout(Some(timeout))
            .build();
        Ok(Mailer::Smtp(Arc::new(SmtpMailer {
            transport,
            from,
            timeout,
        })))
    }

    pub async fn send(&self, email: Email) -> Result<(), MailError> {
        match self {
            Mailer::Log => {
                tracing::info!(to = %email.to, subject = %email.subject, "email (no SMTP configured)");
                // The login link must be findable even when tracing isn't initialised.
                println!(
                    "\n── email to {} ──\n{}\n{}\n",
                    email.to, email.subject, email.text
                );
                Ok(())
            }
            Mailer::Capture(sent) => {
                sent.lock().expect("mailer lock").push(email);
                Ok(())
            }
            Mailer::Smtp(smtp) => {
                let message = build_message(&smtp.from, &email)?;
                // lettre's timeout bounds only the TCP connect; a server that accepts and then
                // goes quiet would hold the job runner, so the whole send gets the same limit.
                match tokio::time::timeout(smtp.timeout, smtp.transport.send(message)).await {
                    Ok(sent) => sent.map(|_| ()).map_err(|e| MailError::Send(e.to_string())),
                    Err(_) => Err(MailError::Send(format!(
                        "the SMTP server did not answer within {} seconds",
                        smtp.timeout.as_secs()
                    ))),
                }
            }
        }
    }
}

fn build_message(from: &Mailbox, email: &Email) -> Result<Message, MailError> {
    let to: Mailbox = email
        .to
        .parse()
        .map_err(|_| MailError::Address(email.to.clone()))?;
    let builder = Message::builder()
        .from(from.clone())
        .to(to)
        .subject(email.subject.clone());
    let built = match &email.html {
        Some(html) => builder.multipart(MultiPart::alternative_plain_html(
            email.text.clone(),
            html.clone(),
        )),
        None => builder.body(email.text.clone()),
    };
    built.map_err(|e| MailError::Build(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(html: Option<&str>) -> Email {
        Email {
            to: "a@example.com".into(),
            subject: "Hi".into(),
            text: "plain body".into(),
            html: html.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn a_server_that_never_answers_fails_the_send_within_the_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accepts and then says nothing, like a hung server.
        let _server = tokio::spawn(async move {
            let _held = listener.accept().await;
            std::future::pending::<()>().await;
        });
        let mailer = Mailer::from_config_with_timeout(
            Some(&format!("smtp://127.0.0.1:{port}")),
            "CodoSEO <hello@codoseo.com>",
            Duration::from_millis(300),
        )
        .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), mailer.send(email(None))).await;
        assert!(
            matches!(outcome, Ok(Err(MailError::Send(_)))),
            "{outcome:?}"
        );
    }

    #[test]
    fn the_default_timeout_is_twenty_seconds() {
        assert_eq!(SMTP_TIMEOUT, Duration::from_secs(20));
    }

    fn from() -> Mailbox {
        "CodoSEO <hello@codoseo.com>".parse().unwrap()
    }

    #[test]
    fn html_makes_a_multipart_alternative() {
        let raw = build_message(&from(), &email(Some("<p>html body</p>")))
            .unwrap()
            .formatted();
        let raw = String::from_utf8_lossy(&raw).into_owned();
        assert!(raw.contains("multipart/alternative"), "{raw}");
        assert!(
            raw.contains("text/plain") && raw.contains("text/html"),
            "{raw}"
        );
        assert!(raw.contains("hello@codoseo.com"), "{raw}");
    }

    #[test]
    fn plain_text_stays_single_part() {
        let raw = build_message(&from(), &email(None)).unwrap().formatted();
        let raw = String::from_utf8_lossy(&raw).into_owned();
        assert!(!raw.contains("multipart"), "{raw}");
        assert!(raw.contains("plain body"), "{raw}");
    }

    #[test]
    fn a_bad_recipient_is_an_address_error() {
        let mut e = email(None);
        e.to = "not an address".into();
        assert!(matches!(
            build_message(&from(), &e),
            Err(MailError::Address(_))
        ));
    }

    #[test]
    fn no_smtp_url_means_the_log_mailer() {
        let m = Mailer::from_config(None, "CodoSEO <hello@codoseo.com>").unwrap();
        assert!(matches!(m, Mailer::Log));
    }

    #[tokio::test]
    async fn a_good_smtp_url_builds_and_a_bad_one_is_a_config_error() {
        let ok = Mailer::from_config(
            Some("smtps://user:pw@mail.example.com:465"),
            "CodoSEO <hello@codoseo.com>",
        );
        assert!(matches!(ok, Ok(Mailer::Smtp(_))));
        let bad = Mailer::from_config(Some("not a url"), "CodoSEO <hello@codoseo.com>");
        assert!(matches!(bad, Err(MailError::Config(_))));
        let bad_from = Mailer::from_config(None, "nonsense");
        assert!(matches!(bad_from, Err(MailError::Config(_))));
    }

    #[tokio::test]
    async fn log_and_capture_send_ok() {
        assert!(Mailer::Log.send(email(None)).await.is_ok());
        let (m, sent) = Mailer::capture();
        m.send(email(Some("<p>x</p>"))).await.unwrap();
        assert_eq!(sent.lock().unwrap().len(), 1);
    }
}
