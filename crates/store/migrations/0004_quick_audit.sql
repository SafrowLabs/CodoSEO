-- M6: the no-signup audit. Per-IP counters and the 24 h same-domain reuse lookup only ever look
-- at quick crawls, so both indexes are partial. No new tables: `sites.claim_token_hash`,
-- `crawls.requester_ip_hash`, `crawls.source` and `events` come from 0001.
CREATE INDEX crawls_quick_ip_idx ON crawls(requester_ip_hash, created_at) WHERE trigger = 'quick';
CREATE INDEX crawls_quick_domain_idx ON crawls(domain, created_at DESC) WHERE trigger = 'quick';
