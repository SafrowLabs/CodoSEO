//! T7.4: alert rules (default instant kinds, the per-site grid) and the channel state the
//! settings page and the delivery job need.

mod support;

use codoseo_core::change::ChangeKind;
use codoseo_notify::{ChannelKey, ChannelKind, ChannelTarget};
use codoseo_store::alert_rules::{self, DEFAULT_INSTANT};
use codoseo_store::channels;
use sqlx::PgPool;
use support::TestDb;
use uuid::Uuid;

async fn account(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO accounts (email, email_canonical) VALUES ($1, $1) RETURNING id")
        .bind(email)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn site(pool: &PgPool, account_id: Uuid, domain: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(account_id)
    .bind(domain)
    .bind(format!("https://{domain}/"))
    .fetch_one(pool)
    .await
    .unwrap()
}

fn slack(token: &str) -> ChannelTarget {
    ChannelTarget::Slack {
        url: format!("https://hooks.slack.com/services/T0/B0/{token}")
            .parse()
            .unwrap(),
    }
}

#[tokio::test]
async fn defaults_are_the_five_instant_kinds_and_creating_them_twice_changes_nothing() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("k");
    let acct = account(&db.pool, "a@example.com").await;
    let s = site(&db.pool, acct, "example.com").await;
    let ch = channels::ensure_default_email(&db.pool, &key, acct)
        .await
        .unwrap();

    alert_rules::create_defaults(&db.pool, s, ch).await.unwrap();
    alert_rules::create_defaults(&db.pool, s, ch).await.unwrap();

    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM alert_rules WHERE site_id = $1")
        .bind(s)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 5);
    for kind in [
        ChangeKind::BecameNoindex,
        ChangeKind::ErrorSpike,
        ChangeKind::RobotsTxtChanged,
        ChangeKind::SitemapShrank,
        ChangeKind::SiteMoved,
    ] {
        assert!(DEFAULT_INSTANT.contains(&kind));
        assert_eq!(
            alert_rules::instant_channels_for(&db.pool, s, kind)
                .await
                .unwrap(),
            vec![ch],
            "{kind:?}"
        );
    }
    // Everything else goes to the digest.
    assert!(
        alert_rules::instant_channels_for(&db.pool, s, ChangeKind::TitleChanged)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_same_rule_cannot_exist_twice() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("k");
    let acct = account(&db.pool, "a@example.com").await;
    let s = site(&db.pool, acct, "example.com").await;
    let ch = channels::ensure_default_email(&db.pool, &key, acct)
        .await
        .unwrap();
    let insert = || {
        sqlx::query(
            "INSERT INTO alert_rules (site_id, change_kind, channel_id, mode) \
             VALUES ($1, 'error_spike', $2, 'instant')",
        )
        .bind(s)
        .bind(ch)
        .execute(&db.pool)
    };
    insert().await.unwrap();
    assert!(insert().await.is_err(), "unique (site, kind, channel)");
}

#[tokio::test]
async fn toggling_a_rule_persists_and_an_unchecked_default_is_not_revived() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("k");
    let acct = account(&db.pool, "a@example.com").await;
    let s = site(&db.pool, acct, "example.com").await;
    let ch = channels::ensure_default_email(&db.pool, &key, acct)
        .await
        .unwrap();
    alert_rules::create_defaults(&db.pool, s, ch).await.unwrap();

    // Turn one default off and one extra kind on.
    assert!(
        alert_rules::set(&db.pool, s, ChangeKind::ErrorSpike, ch, false)
            .await
            .unwrap()
    );
    assert!(
        alert_rules::set(&db.pool, s, ChangeKind::TitleChanged, ch, true)
            .await
            .unwrap()
    );
    // Setting twice is fine.
    assert!(
        alert_rules::set(&db.pool, s, ChangeKind::TitleChanged, ch, true)
            .await
            .unwrap()
    );
    // Re-running the defaults (a second channel added later calls it for other channels) keeps
    // what the user chose.
    alert_rules::create_defaults(&db.pool, s, ch).await.unwrap();

    assert!(
        alert_rules::instant_channels_for(&db.pool, s, ChangeKind::ErrorSpike)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        alert_rules::instant_channels_for(&db.pool, s, ChangeKind::TitleChanged)
            .await
            .unwrap(),
        vec![ch]
    );

    let grid = alert_rules::grid(&db.pool, acct).await.unwrap();
    let cell = |kind| {
        grid.iter()
            .find(|c| c.site_id == s && c.channel_id == ch && c.kind == kind)
            .map(|c| c.instant)
    };
    assert_eq!(cell(ChangeKind::ErrorSpike), Some(false));
    assert_eq!(cell(ChangeKind::TitleChanged), Some(true));
    assert_eq!(cell(ChangeKind::SiteMoved), Some(true));
    assert_eq!(cell(ChangeKind::NewUrl), None, "no rule at all");
}

#[tokio::test]
async fn a_rule_cannot_join_a_site_to_another_accounts_channel() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("k");
    let a = account(&db.pool, "a@example.com").await;
    let b = account(&db.pool, "b@example.com").await;
    let site_a = site(&db.pool, a, "a.example.com").await;
    let channel_b = channels::ensure_default_email(&db.pool, &key, b)
        .await
        .unwrap();

    assert!(
        !alert_rules::set(&db.pool, site_a, ChangeKind::ErrorSpike, channel_b, true)
            .await
            .unwrap()
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM alert_rules")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert!(
        alert_rules::grid(&db.pool, a).await.unwrap().is_empty()
            && alert_rules::grid(&db.pool, b).await.unwrap().is_empty()
    );
}

#[tokio::test]
async fn a_new_channel_gets_default_rules_on_every_site_of_the_account() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("k");
    let acct = account(&db.pool, "a@example.com").await;
    let other = account(&db.pool, "b@example.com").await;
    let s1 = site(&db.pool, acct, "one.example.com").await;
    let s2 = site(&db.pool, acct, "two.example.com").await;
    let foreign = site(&db.pool, other, "three.example.com").await;
    let ch = channels::create(&db.pool, &key, acct, &slack("x"), None, false)
        .await
        .unwrap();

    alert_rules::create_defaults_for_account(&db.pool, acct, ch)
        .await
        .unwrap();

    for s in [s1, s2] {
        assert_eq!(
            alert_rules::instant_channels_for(&db.pool, s, ChangeKind::SiteMoved)
                .await
                .unwrap(),
            vec![ch]
        );
    }
    assert!(
        alert_rules::instant_channels_for(&db.pool, foreign, ChangeKind::SiteMoved)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn deleting_a_channel_removes_its_rules() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("k");
    let acct = account(&db.pool, "a@example.com").await;
    let s = site(&db.pool, acct, "example.com").await;
    let ch = channels::create(&db.pool, &key, acct, &slack("x"), None, false)
        .await
        .unwrap();
    alert_rules::create_defaults(&db.pool, s, ch).await.unwrap();
    channels::delete(&db.pool, acct, ch).await.unwrap();
    assert!(alert_rules::grid(&db.pool, acct).await.unwrap().is_empty());
}

#[tokio::test]
async fn reenabling_a_channel_clears_its_failures_and_error() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("k");
    let acct = account(&db.pool, "a@example.com").await;
    let ch = channels::create(&db.pool, &key, acct, &slack("x"), None, false)
        .await
        .unwrap();
    for _ in 0..3 {
        channels::record_failure(&db.pool, ch, "HTTP 500")
            .await
            .unwrap();
    }
    channels::disable(&db.pool, ch, "HTTP 500").await.unwrap();

    let state = channels::state(&db.pool, ch).await.unwrap().unwrap();
    assert_eq!(state.account_id, acct);
    assert_eq!(state.kind, ChannelKind::Slack);
    assert!(!state.enabled && !state.muted);

    // Not someone else's.
    let stranger = account(&db.pool, "z@example.com").await;
    assert!(!channels::reenable(&db.pool, stranger, ch).await.unwrap());
    assert!(
        !channels::state(&db.pool, ch)
            .await
            .unwrap()
            .unwrap()
            .enabled
    );

    assert!(channels::reenable(&db.pool, acct, ch).await.unwrap());
    let state = channels::state(&db.pool, ch).await.unwrap().unwrap();
    assert!(state.enabled);
    let listed = channels::list_for_account(&db.pool, &key, acct)
        .await
        .unwrap();
    assert_eq!(listed[0].consecutive_failures, 0);
    assert_eq!(listed[0].last_error, None);
    assert!(
        channels::state(&db.pool, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn only_enabled_unmuted_channels_are_deliverable() {
    let db = TestDb::new().await;
    let key = ChannelKey::derive("k");
    let acct = account(&db.pool, "a@example.com").await;
    let email = channels::ensure_default_email(&db.pool, &key, acct)
        .await
        .unwrap();
    let active = channels::create(&db.pool, &key, acct, &slack("a"), None, false)
        .await
        .unwrap();
    let muted = channels::create(&db.pool, &key, acct, &slack("b"), None, false)
        .await
        .unwrap();
    let off = channels::create(&db.pool, &key, acct, &slack("c"), None, false)
        .await
        .unwrap();
    channels::set_muted(&db.pool, acct, muted, true)
        .await
        .unwrap();
    channels::disable(&db.pool, off, "boom").await.unwrap();

    let mut got = channels::deliverable(&db.pool, acct).await.unwrap();
    got.sort_by_key(|c| c.0);
    let mut want = vec![(email, ChannelKind::Email), (active, ChannelKind::Slack)];
    want.sort_by_key(|c| c.0);
    assert_eq!(got, want);
}
