# papers

Fetches sources, keeps what is worth reading, publishes one Atom feed.

One binary, one container, one SQLite file, one feed at `/feed.xml`. Sources are declared in
TOML, ranking is rules first and a model second, and the output is a normal feed any reader can
subscribe to.

## How it works

1. `fetch` pulls every source in `sources.toml` and stores new items, deduped by link hash.
   Anything older than `max_age_days` is skipped, so a source's back catalogue never
   floods the feed
2. `rank` drops excluded items by keyword, then asks the model to score the rest 0 to 10
3. everything is published, up to `max_per_run` per run, best score first
4. `serve` publishes the feed over HTTP and runs the job hourly in process

Each article is its own entry with its image, summary, source extras (poster, rating, inline
trailer) and the scoring reason, so a reader shows a stream of articles rather than a digest.

A model outage is not fatal: everything is published unscored and the paper still prints.

## Ranking

Everything a source publishes lands in the feed. Ranking only decides the order and, if you ask
for it, what to drop:

```toml
[rank]
exclude = ["giveaway", "sponsored", "webinar"]
require_any = []
min_score = 0
prompt = "..."
```

- `prompt` is the steering wheel: tell the model what you want more or less of. Live editable
  through `POST /admin/config`, no deploy needed
- `min_score` is the only gate. `0` publishes everything the model saw; raise it as noise shows up
- `exclude` and `require_any` are hard keyword filters, applied before the model sees anything

Seerr picks skip the model: they are curated trending, so they score at the top and always publish.

## Source kinds

```toml
[[sources]]
name = "lwn"
kind = "feed"
url = "https://lwn.net/headlines/rss"

[[sources]]
name = "example-blog"
kind = "html"
url = "https://example.com/blog/"
item = "article"
title = "h2"
link = "a"
summary = "p"
max_per_fetch = 10
```

`label` prefixes entry titles, which is how a source announces itself in the feed:

```toml
[[sources]]
name = "seerr-trending"
kind = "seerr"
url = "http://seerr.media.svc.cluster.local"
label = "New in Seerr"
```

## Configuration

| variable | default | meaning |
|---|---|---|
| `PAPERS_CONFIG` | `/data/sources.toml` | sources file |
| `PAPERS_DB` | `/data/papers.db` | SQLite path |
| `PAPERS_OUT` | `/data/out` | rendered feed directory |
| `PAPERS_PORT` | `8080` | HTTP port for `serve` |
| `PAPERS_BASE_URL` | `http://localhost:8080` | public host for the feed self link, icon and `/add` links |
| `PAPERS_INTERVAL_MINUTES` | `60` | minutes between runs in `serve` |
| `PAPERS_LLM_URL` | `http://litellm.ai.svc.cluster.local:4000/v1` | OpenAI compatible endpoint |
| `PAPERS_LLM_MODEL` | `deepseek-v4.1-flash` | model name |
| `PAPERS_LLM_KEY` | empty | bearer token for the endpoint |

## Commands

```bash
papers run              # fetch, rank, publish feed.xml
papers run --no-llm     # same, no model call: everything publishes unscored
papers serve            # publish /feed.xml and run the hourly job
papers sources          # print the configured sources
```

## Development

```bash
mise run check          # fmt, clippy, tests
cargo test
scripts/dev-run.sh      # fetch dev/sources.toml, publish, serve for Reeder
```

`scripts/dev-run.sh` publishes to `dev/out` and serves on `http://<lan ip>:8199/feed.xml`, so a
reader on the same network can subscribe to the real rendering while you iterate.
