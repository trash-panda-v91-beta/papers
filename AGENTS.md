# papers

Rust service that fetches declared sources, scores what is worth reading, and publishes one
daily Atom paper. Replaces the nginx feed host and the Python digest scripts in nebular-grid.

## Commands

```bash
mise run check    # hk check --all: fmt, clippy with -D warnings, actionlint, yaml, toml, markdown
mise run fix      # hk fix --all
mise run test     # cargo test, unit tests live inside each module
```

`mise run` is the command surface. No direct tool invocation needed beyond cargo.

## Layout

- `src/config.rs` - `sources.toml` parsing, source kinds `feed` and `html`
- `src/fetch.rs` - pulls one source into normalized items, resolves relative links
- `src/store.rs` - SQLite: `items` deduped by link hash, `digests` one row per day
- `src/rank.rs` - keyword prefilter, then model scoring over the survivors
- `src/render.rs` - Atom rendering, 30 days of entries
- `src/main.rs` - subcommands `run`, `serve`, `sources`, plus the in process scheduler

Each module carries its own `#[cfg(test)]` block; there is no separate integration suite.

## Conventions

- Hyphen-minus only in every artifact, never en or em dashes.
- Comments explain why, not what. A clear name beats a comment.
- Conventional Commits, one change per commit.
- Secrets arrive as env vars from an ExternalSecret, never in the repo.
- Ranking quality is the product. Plumbing changes need a reason; prompt changes are expected.

## Deploy

Image `ghcr.io/trash-panda-v91-beta/papers`, tag pinned by digest in nebular-grid. Runs as
`serve` with a PVC at `/data`. Nothing listens publicly: the HTTPRoute attaches to the
internal gateway and sits behind the Envoy basic auth policy.
