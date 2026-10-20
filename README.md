# papers

Fetches sources, keeps what is worth reading, publishes one daily paper as an Atom feed.

One binary, one container, one SQLite file. Sources are declared in TOML, ranking is rules
first and a model second, and the output is a normal feed any reader can subscribe to.

## How it works

1. `fetch` pulls every source in `sources.toml` and stores new items, deduped by link hash
2. `rank` drops excluded items by keyword, then asks the model to score the rest 0 to 10
3. the top `keep_per_day` items become one entry for the day, older days stay in the feed
4. `serve` publishes the feed over HTTP and runs the daily job in process

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

`kind = "api"` for Jellyseerr and GitHub comes next, see the task note in the vault.

## Configuration

| variable | default | meaning |
|---|---|---|
| `PAPERS_CONFIG` | `/data/sources.toml` | sources file |
| `PAPERS_DB` | `/data/papers.db` | SQLite path |
| `PAPERS_OUT` | `/data/out` | rendered feed directory |
| `PAPERS_PORT` | `8080` | HTTP port for `serve` |
| `PAPERS_HOUR` | `6` | local hour of the daily run |
| `PAPERS_FEED_URL` | `http://localhost:8080/daily.xml` | self link in the feed |
| `PAPERS_LLM_URL` | `http://litellm.ai.svc.cluster.local:4000/v1` | OpenAI compatible endpoint |
| `PAPERS_LLM_MODEL` | `deepseek-v4.1-flash` | model name |
| `PAPERS_LLM_KEY` | empty | bearer token for the endpoint |

## Commands

```bash
papers run              # fetch, rank, print today's paper
papers run --no-llm     # same, scoring by source weight only
papers serve            # publish /daily.xml and run the daily job
papers sources          # print the configured sources
```

## Development

```bash
mise run check          # fmt, clippy, tests
cargo test
```
