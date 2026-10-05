//! The SMTP mailer against a tiny in-process SMTP server (no internet in tests).

use codoseo_notify::{Email, MailError, Mailer};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Accepts one connection, speaks just enough SMTP, and returns the DATA section it received.
async fn fake_smtp(listener: TcpListener) -> String {
    let (stream, _) = listener.accept().await.expect("accept");
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    write.write_all(b"220 fake ESMTP\r\n").await.unwrap();
    let mut data = String::new();
    let mut in_data = false;
    let mut line = String::new();
    loop {
        line.clear();
        if read.read_line(&mut line).await.unwrap() == 0 {
            return data;
        }
        if in_data {
            if line == ".\r\n" {
                in_data = false;
                write.write_all(b"250 queued\r\n").await.unwrap();
            } else {
                data.push_str(&line);
            }
            continue;
        }
        let upper = line.to_ascii_uppercase();
        let reply: &[u8] = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            b"250 fake\r\n"
        } else if upper.starts_with("DATA") {
            in_data = true;
            b"354 go ahead\r\n"
        } else if upper.starts_with("QUIT") {
            write.write_all(b"221 bye\r\n").await.unwrap();
            return data;
        } else {
            b"250 ok\r\n"
        };
        write.write_all(reply).await.unwrap();
    }
}

#[tokio::test]
async fn sends_a_multipart_message_through_smtp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(fake_smtp(listener));

    let mailer = Mailer::from_config(
        Some(&format!("smtp://127.0.0.1:{port}")),
        "CodoSEO <hello@codoseo.com>",
    )
    .unwrap();
    mailer
        .send(Email {
            to: "to@example.com".into(),
            subject: "Weekly digest".into(),
            text: "plain part".into(),
            html: Some("<p>html part</p>".into()),
        })
        .await
        .expect("sent");

    let data = server.await.unwrap();
    assert!(data.contains("Subject: Weekly digest"), "{data}");
    assert!(data.contains("multipart/alternative"), "{data}");
    assert!(
        data.contains("plain part") && data.contains("html part"),
        "{data}"
    );
}

#[tokio::test]
async fn a_refused_connection_is_a_send_error() {
    // A port nothing listens on.
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let mailer = Mailer::from_config(
        Some(&format!("smtp://127.0.0.1:{port}")),
        "CodoSEO <hello@codoseo.com>",
    )
    .unwrap();
    let err = mailer
        .send(Email {
            to: "to@example.com".into(),
            subject: "x".into(),
            text: "y".into(),
            html: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, MailError::Send(_)), "{err}");
}
