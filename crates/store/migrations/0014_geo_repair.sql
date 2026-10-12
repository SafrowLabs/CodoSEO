-- GEO Phase B3: repair and recheck.
--
-- An incident can now be accepted or snoozed by the owner, recover on its own before it is
-- confirmed, and be re-tested on demand or by a follow-up recheck.
ALTER TABLE ai_incidents
  ADD COLUMN before JSONB,
  ADD COLUMN accepted_at TIMESTAMPTZ,
  ADD COLUMN accepted_by UUID REFERENCES accounts(id) ON DELETE SET NULL,
  ADD COLUMN snoozed_until TIMESTAMPTZ,
  ADD COLUMN snooze_escalated BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN recovered_at TIMESTAMPTZ,
  ADD COLUMN verified_at TIMESTAMPTZ,
  ADD COLUMN resolved_by TEXT CHECK (resolved_by IN ('crawl', 'recheck', 'intent'));

COMMENT ON COLUMN ai_incidents.before IS
  'What was true before the incident opened, captured when it opens from the newest earlier '
  'report that judged its kind (codoseo_geo::incident::Before); NULL when there was none (a '
  'baseline incident) and for incidents an intent change opened.';
COMMENT ON COLUMN ai_incidents.accepted_at IS
  'When the owner accepted the finding as it is (an intent they chose); NULL if not accepted.';
COMMENT ON COLUMN ai_incidents.accepted_by IS
  'The account that accepted it. Set NULL if that account is deleted.';
COMMENT ON COLUMN ai_incidents.snoozed_until IS
  'The incident stays open but raises no reminders until this time; NULL if not snoozed.';
COMMENT ON COLUMN ai_incidents.snooze_escalated IS
  'It got worse while snoozed (that change was recorded already alerted): the first crawl '
  'that still finds it once the snooze is over announces it, then this is cleared.';
COMMENT ON COLUMN ai_incidents.recovered_at IS
  'When a check first saw the problem gone, before it was confirmed; NULL if it has not recovered.';
COMMENT ON COLUMN ai_incidents.verified_at IS
  'When a recheck or a later crawl confirmed the recovery; NULL until verified.';
COMMENT ON COLUMN ai_incidents.resolved_by IS
  'What resolved the incident: crawl | recheck | intent; NULL while open and for incidents '
  'resolved before this column existed.';

-- One row per recheck of an incident: a manual "check again" or the follow-up the system queues
-- after a repair. At most one is queued or running per incident.
CREATE TABLE ai_rechecks (
  id BIGSERIAL PRIMARY KEY,
  site_id UUID NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
  incident_id BIGINT NOT NULL REFERENCES ai_incidents(id) ON DELETE CASCADE,
  origin TEXT NOT NULL CHECK (origin IN ('manual', 'followup')),
  requested_by UUID REFERENCES accounts(id) ON DELETE SET NULL,
  status TEXT NOT NULL DEFAULT 'queued'
    CHECK (status IN ('queued', 'running', 'done', 'superseded', 'failed')),
  due_at TIMESTAMPTZ NOT NULL,
  started_at TIMESTAMPTZ,
  finished_at TIMESTAMPTZ,
  outcome TEXT CHECK (outcome IN ('clean', 'still_failing', 'inconclusive')),
  failure TEXT CHECK (failure IN ('site', 'ours')),
  observations JSONB,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX ai_rechecks_active_idx ON ai_rechecks (incident_id)
  WHERE status IN ('queued', 'running');
CREATE INDEX ai_rechecks_site_idx ON ai_rechecks (site_id, created_at);

COMMENT ON COLUMN ai_rechecks.origin IS 'manual (the owner asked) | followup (queued after a repair)';
COMMENT ON COLUMN ai_rechecks.requested_by IS 'The account that asked for a manual recheck; NULL for follow-ups.';
COMMENT ON COLUMN ai_rechecks.status IS 'queued | running | done | superseded | failed';
COMMENT ON COLUMN ai_rechecks.due_at IS 'When the recheck may run; later than created_at for a follow-up.';
COMMENT ON COLUMN ai_rechecks.outcome IS 'clean | still_failing | inconclusive; NULL until done.';
COMMENT ON COLUMN ai_rechecks.failure IS
  'Why a failed recheck failed: site (robots.txt got no answer; a manual one uses the daily '
  'allowance) | ours (it could not run on our side; free); NULL otherwise.';
COMMENT ON COLUMN ai_rechecks.observations IS 'What the recheck saw (the fresh probes and robots.txt), as JSON; NULL until done.';

-- The job that runs a recheck when it is due. Not used before the commit, as in 0012 and 0013.
ALTER TYPE job_kind ADD VALUE 'ai_recheck';
