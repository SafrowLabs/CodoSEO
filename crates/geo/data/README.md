# CodoSEO AI bot registry

`ai-bots.json` lists the AI crawlers and agents that the operators themselves document, with the
robots.txt product token, what the bot is for, and whether it honours robots.txt. It is published
at <https://codoseo.com/ai-bots.json> and described by `ai-bots.schema.json`.

Licence: CC0-1.0 (see `LICENSE`). Use it for anything, no attribution needed. The rest of this
crate is AGPL-3.0-only.

## Fields

- `token`: the robots.txt product token exactly as the operator writes it.
- `operator`, `product`: who runs it and what it feeds.
- `purpose`: `search` (builds a search or answer index), `user_fetch` (fetches a page because a
  user asked), `agent` (an autonomous agent acting for a user), `training` (model training data),
  `ads` (ad review or ad targeting).
- `honours_robots`: `yes`, `partial` (the operator says rules may not apply), `no` (the operator
  says it generally ignores them), `unknown` (not stated).
- `crawls`: false for control tokens such as Google-Extended that never make requests.
- `user_agent_contains`: a substring to match in the User-Agent header, or null.
- `ip_ranges_url`: a JSON file of `prefixes` (`ipv4Prefix` / `ipv6Prefix`), or null when the
  operator publishes none or only an HTML page.
- `reverse_dns`: host suffixes for reverse-DNS verification.
- `signature_agent`: the Web Bot Auth `Signature-Agent` identity, or null.
- `source_url`: the operator page the entry was checked against.
- `last_reviewed`: the date (YYYY-MM-DD) the entry was last checked.
- `notes`: one sentence of context.

## Proposing a change

Open a pull request against <https://github.com/SafrowLabs/CodoSEO> editing `ai-bots.json`. Cite
an operator-owned page in `source_url`, never a third-party directory, and update `last_reviewed`
and the top-level `updated`. `cargo test -p codoseo-geo` lints the file.
