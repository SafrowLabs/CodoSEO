-- The cloud plans become Free, Lite ($9) and Pro ($19). The old $9 Pro is now Lite and the old
-- Agency is now Pro, so existing accounts keep what they pay for under the new name. The order
-- matters: 'pro' has to be free before 'agency' can take it. Renaming a label rewrites no rows,
-- and `accounts.plan` is the only place a plan slug is stored.
ALTER TYPE plan RENAME VALUE 'pro' TO 'lite';
ALTER TYPE plan RENAME VALUE 'agency' TO 'pro';

-- The new Pro allows 10 monitored sites where Agency allowed 25. As on any downgrade
-- (`plans::apply_plan_limits`), the oldest stay monitored and the rest stop; nothing is deleted
-- and the owner can choose others on /billing/sites. Lite keeps the old Pro's 5 sites.
UPDATE sites SET monitoring_active = false WHERE id IN (
  SELECT id FROM (
    SELECT s.id, row_number() OVER (PARTITION BY s.account_id ORDER BY s.created_at, s.id) AS n
    FROM sites s JOIN accounts a ON a.id = s.account_id
    WHERE a.plan = 'pro' AND s.monitoring_active
  ) ranked
  WHERE n > 10
);
