# papers

Fetches sources, keeps what is worth reading, publishes one Atom feed.

One binary, one container, one SQLite file, one feed at `/feed.xml`. Sources are declared in
TOML, ranking is rules first and a model second, and the output is a normal feed any reader can
subscribe to.

## How it works

1. `fetch` pulls every source in `sources.toml` and stores new items, deduped by link hash
2. `rank` drops excluded items by keyword, then asks the model to score the rest 0 to 10
3. everything scoring 5 or more is published, up to `max_per_run` per run
4. `serve` publishes the feed over HTTP and runs the job hourly in process

Each article is its own entry with its image, summary, source extras (poster, rating, inline
trailer) and the scoring reason, so a reader shows a stream of articles rather than a digest.

A model outage is not fatal: scoring falls back to the per source weight and the paper still prints.

## Source kinds

```toml
[[sources]]
name = "lwn"
kind = "feed"
url = "https://lwn.net/headlines/rss"
weight = 8

[[sources]]
name = "example-blog"
kind = "html"
url = "https://example.com/blog/"
item = "article"
title = "h2"
link = "a"
summary = "p"
weight = 5
max_per_fetch = 10
```

`label` prefixes entry titles, which is how a source announces itself in the feed:

```toml
[[sources]]
name = "seerr-trending"
kind = "seerr"
url = "http://seerr.media.svc.cluster.local"
label = "New in Seerr"
weight = 7
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
papers run --no-llm     # same, scoring by source weight only
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
