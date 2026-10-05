-- M7 final review: the jobs table grows with every alert, digest and email, and the scheduler
-- reads it on each tick.
--
-- The latest "Keep monitoring?" email job of an account, looked up per Free account on every
-- tick (`schedule::WARNING_JOB_STATUS`): the expression and predicate match that query.
CREATE INDEX jobs_keep_monitoring_idx ON jobs ((payload->>'keep_monitoring_for'), created_at DESC)
  WHERE kind = 'send_email';

-- Running jobs, scanned for ones whose worker died.
CREATE INDEX jobs_running_idx ON jobs (claimed_at) WHERE status = 'running';
