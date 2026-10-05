//! T7.2: notification channels: targets encrypted at rest, the default email channel, failure
//! bookkeeping.

mod support;

use codoseo_notify::{ChannelKey, ChannelKind, ChannelTarget};
use codoseo_store::channels::{self, DeleteOutcome};
use sqlx::{PgPool, Row};
use support::TestDb;
use uuid::Uuid;

async fn account(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO accounts (email, email_canonical) VALUES ($1, $1) RETURNING id")
        .bind(email)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn webhook() -> ChannelTarget {
    ChannelTarget::Webhook {
        url: "https://alerts.example.com/hooks/abc123".parse().unwrap(),
        secret: "whsec_topsecret".into(),
    }
}

#[tokio::test]
async fn a_channel_round_trips_with_its_target_encrypted_at_rest() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("secret");
    let acct = account(&db.pool, "a@example.com").await;

    let id = channels::create(&db.pool, &key, acct, &webhook(), Some("Pager"), false)
        .await
        .unwrap();

    // The column holds nonce ‖ ciphertext, never the plaintext.
    let raw: Vec<u8> =
        sqlx::query_scalar("SELECT target_encrypted FROM alert_channels WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    for needle in ["alerts.example.com", "abc123", "whsec_topsecret"] {
        assert!(
            !raw.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "{needle} found in the stored bytes"
        );
    }

    assert_eq!(
        channels::get_target(&db.pool, &key, id).await.unwrap(),
        Some(webhook())
    );
    assert_eq!(
        channels::get_target(&db.pool, &key, Uuid::new_v4())
            .await
            .unwrap(),
        None
    );
    // Another key can't read it.
    assert!(
        channels::get_target(&db.pool, &ChannelKey::derive("other"), id)
            .await
            .is_err()
    );

    let list = channels::list_for_account(&db.pool, &key, acct)
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    let c = &list[0];
    assert_eq!((c.id, c.kind), (id, ChannelKind::Webhook));
    assert_eq!(c.name.as_deref(), Some("Pager"));
    // Host only: neither the path nor the secret is ever shown.
    assert_eq!(c.target, "alerts.example.com");
    assert!(c.enabled && !c.muted && !c.is_default);
    assert_eq!(c.consecutive_failures, 0);

    assert_eq!(
        channels::delete(&db.pool, acct, id).await.unwrap(),
        DeleteOutcome::Deleted
    );
    assert!(
        channels::list_for_account(&db.pool, &key, acct)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        channels::delete(&db.pool, acct, id).await.unwrap(),
        DeleteOutcome::NotFound
    );
}

#[tokio::test]
async fn one_account_cannot_touch_anothers_channels() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("secret");
    let (a, b) = (
        account(&db.pool, "a@example.com").await,
        account(&db.pool, "b@example.com").await,
    );
    let id = channels::create(&db.pool, &key, a, &webhook(), None, false)
        .await
        .unwrap();
    assert_eq!(
        channels::delete(&db.pool, b, id).await.unwrap(),
        DeleteOutcome::NotFound
    );
    assert!(!channels::set_muted(&db.pool, b, id, true).await.unwrap());
    assert!(
        channels::list_for_account(&db.pool, &key, b)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        channels::list_for_account(&db.pool, &key, a)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn the_default_email_channel_is_the_account_address_and_cannot_be_deleted() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("secret");
    let acct = account(&db.pool, "owner@example.com").await;

    let id = channels::ensure_default_email(&db.pool, &key, acct)
        .await
        .unwrap();
    assert_eq!(
        channels::get_target(&db.pool, &key, id).await.unwrap(),
        Some(ChannelTarget::Email {
            to: "owner@example.com".into()
        })
    );
    let list = channels::list_for_account(&db.pool, &key, acct)
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].is_default);
    assert_eq!(list[0].kind, ChannelKind::Email);
    assert_eq!(list[0].target, "owner@example.com");

    assert_eq!(
        channels::delete(&db.pool, acct, id).await.unwrap(),
        DeleteOutcome::DefaultChannel
    );
    assert_eq!(
        channels::list_for_account(&db.pool, &key, acct)
            .await
            .unwrap()
            .len(),
        1
    );

    // It can be muted and unmuted.
    assert!(channels::set_muted(&db.pool, acct, id, true).await.unwrap());
    assert!(
        channels::list_for_account(&db.pool, &key, acct)
            .await
            .unwrap()[0]
            .muted
    );
    assert!(
        channels::set_muted(&db.pool, acct, id, false)
            .await
            .unwrap()
    );
    assert!(
        !channels::list_for_account(&db.pool, &key, acct)
            .await
            .unwrap()[0]
            .muted
    );
}

#[tokio::test]
async fn ensure_default_email_is_idempotent_even_when_called_at_once() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("secret");
    let acct = account(&db.pool, "owner@example.com").await;
    let other = account(&db.pool, "other@example.com").await;

    let ids = futures_util::future::join_all(
        (0..6).map(|_| channels::ensure_default_email(&db.pool, &key, acct)),
    )
    .await;
    let first = *ids[0].as_ref().unwrap();
    assert!(ids.iter().all(|r| *r.as_ref().unwrap() == first));
    assert_eq!(
        channels::ensure_default_email(&db.pool, &key, acct)
            .await
            .unwrap(),
        first
    );

    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM alert_channels WHERE account_id = $1")
        .bind(acct)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    // Accounts don't share a default.
    assert_ne!(
        channels::ensure_default_email(&db.pool, &key, other)
            .await
            .unwrap(),
        first
    );
}

#[tokio::test]
async fn an_account_has_at_most_one_default_channel() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("secret");
    let acct = account(&db.pool, "owner@example.com").await;
    let mail = ChannelTarget::Email {
        to: "owner@example.com".into(),
    };
    channels::create(&db.pool, &key, acct, &mail, None, true)
        .await
        .unwrap();
    assert!(
        channels::create(&db.pool, &key, acct, &mail, None, true)
            .await
            .is_err()
    );
    // Non-default channels are unlimited.
    channels::create(&db.pool, &key, acct, &mail, Some("Also me"), false)
        .await
        .unwrap();
    channels::create(&db.pool, &key, acct, &mail, Some("And me"), false)
        .await
        .unwrap();
}

#[tokio::test]
async fn failures_count_up_success_resets_and_disable_switches_the_channel_off() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("secret");
    let acct = account(&db.pool, "a@example.com").await;
    let id = channels::create(&db.pool, &key, acct, &webhook(), None, false)
        .await
        .unwrap();

    let state = |pool: PgPool| async move {
        let r = sqlx::query(
            "SELECT enabled, consecutive_failures, last_error, last_failure_at IS NOT NULL AS failed \
             FROM alert_channels WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        (
            r.get::<bool, _>("enabled"),
            r.get::<i16, _>("consecutive_failures"),
            r.get::<Option<String>, _>("last_error"),
            r.get::<bool, _>("failed"),
        )
    };

    channels::record_failure(&db.pool, id, "the server answered 500")
        .await
        .unwrap();
    channels::record_failure(&db.pool, id, "the server answered 502")
        .await
        .unwrap();
    assert_eq!(
        state(db.pool.clone()).await,
        (true, 2, Some("the server answered 502".into()), true)
    );

    channels::record_success(&db.pool, id).await.unwrap();
    let (enabled, failures, err, _) = state(db.pool.clone()).await;
    assert_eq!((enabled, failures, err), (true, 0, None));

    channels::disable(&db.pool, id, "gave up after 5 attempts")
        .await
        .unwrap();
    assert_eq!(
        state(db.pool.clone()).await,
        (false, 0, Some("gave up after 5 attempts".into()), true)
    );
    let list = channels::list_for_account(&db.pool, &key, acct)
        .await
        .unwrap();
    assert!(!list[0].enabled);
    assert_eq!(
        list[0].last_error.as_deref(),
        Some("gave up after 5 attempts")
    );
}
