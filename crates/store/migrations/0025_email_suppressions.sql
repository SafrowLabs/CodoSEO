-- Addresses CodoSEO must not email. `notifications` stops alerts, the weekly digest and
-- reminders (the person unsubscribed); `all` stops every email, sign-in links included (the
-- person asked never to be emailed, or the address bounced or complained).
CREATE TYPE email_suppression_scope AS ENUM ('notifications', 'all');

CREATE TABLE email_suppressions (
  address TEXT PRIMARY KEY,
  scope email_suppression_scope NOT NULL,
  reason TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
