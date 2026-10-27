mod config;
mod fetch;
mod rank;
mod render;
mod store;

use std::collections::HashMap;
use std::env;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;
use tracing::{info, warn};

use crate::config::Config;
use crate::store::{Item, NewItem, Store};

const DEFAULT_LLM_URL: &str = "http://litellm.ai.svc.cluster.local:4000/v1";
const DEFAULT_LLM_MODEL: &str = "deepseek-v4.1-flash";
/// atoms readers scroll through before the old ones fall out
const FEED_ENTRIES: usize = 100;
/// seerr trending is curated, so picks sort above anything the model scores
const SEERR_SCORE: i64 = 10;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(!args.iter().any(|arg| arg == "--no-llm")).await,
        Some("serve") => serve().await,
        Some("sources") => {
            let config = Config::load(&Config::path())?;
            for source in &config.sources {
                println!("{}", source.name);
            }
            Ok(())
        }
        _ => {
            eprintln!("usage: papers run [--no-llm] | papers serve | papers sources");
            Ok(())
        }
    }
}

fn db_path() -> String {
    env::var("PAPERS_DB").unwrap_or_else(|_| "/data/papers.db".to_string())
}

fn out_dir() -> String {
    env::var("PAPERS_OUT").unwrap_or_else(|_| "/data/out".to_string())
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("papers/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?)
}

async fn run(use_model: bool) -> Result<()> {
    let store = Store::open(&db_path())?;
    let config = effective_config(&store)?;
    let http = http_client()?;
    let cutoff = cutoff(config.max_age_days);

    let mut added = 0usize;
    for source in &config.sources {
        match fetch::fetch(&http, source).await {
            Ok(items) => {
                for item in items {
                    if !fresh(item.published, cutoff) {
                        continue;
                    }
                    let new_item = NewItem {
                        source: source.name.clone(),
                        title: item.title,
                        link: item.link,
                        summary: item.summary,
                        published: item.published,
                        extra: item.extra,
                        image: item.image,
                    };
                    if store.insert(&new_item)? {
                        added += 1;
                    }
                }
            }
            Err(error) => warn!(source = %source.name, %error, "fetch failed"),
        }
    }

    let candidates = rank::prefilter(store.unseen(200)?, &config.rank);
    info!(fetched = added, candidates = candidates.len(), "collected");

    // seerr picks are not news; the reading scorer would fail them every time
    let seerr_names: Vec<&str> = config
        .sources
        .iter()
        .filter(|source| matches!(source.kind, config::SourceKind::Seerr { .. }))
        .map(|source| source.name.as_str())
        .collect();
    let (media, news): (Vec<Item>, Vec<Item>) = candidates
        .into_iter()
        .partition(|item| seerr_names.contains(&item.source.as_str()));

    if use_model {
        let prompt = config
            .rank
            .prompt
            .clone()
            .unwrap_or_else(|| rank::DEFAULT_PROMPT.to_string());
        match score_with_model(&http, &prompt, &news).await {
            Ok(verdicts) => {
                for verdict in verdicts {
                    store.set_score(verdict.id, verdict.score, &verdict.reason)?;
                }
            }
            Err(error) => {
                warn!(%error, "model scoring failed, publishing unscored");
                score_at_floor(
                    &news,
                    &store,
                    config.rank.min_score,
                    "rule score (model unavailable)",
                )?;
            }
        }
    } else {
        score_at_floor(&news, &store, config.rank.min_score, "rule score")?;
    }
    score_at_floor(&media, &store, SEERR_SCORE, "trending pick")?;

    // each run publishes what cleared the bar, but never the whole backlog at once
    let top = store.top(config.max_per_run, config.rank.min_score)?;
    if top.is_empty() {
        info!("nothing cleared the bar in this run");
        return Ok(());
    }
    let shown_ids: Vec<i64> = top.iter().map(|item| item.id).collect();
    store.mark_shown(&shown_ids)?;
    write_feed(&store)?;
    info!(kept = shown_ids.len(), "paper printed");
    Ok(())
}

/// feeds hand over their whole archive; anything older than the window is noise
fn cutoff(days: i64) -> DateTime<Utc> {
    if days <= 0 {
        DateTime::<Utc>::MIN_UTC
    } else {
        Utc::now() - TimeDelta::days(days)
    }
}

/// undated items are seerr picks, which are current by definition
fn fresh(published: Option<DateTime<Utc>>, cutoff: DateTime<Utc>) -> bool {
    published.is_none_or(|published| published >= cutoff)
}

fn score_at_floor(items: &[Item], store: &Store, score: i64, reason: &str) -> Result<()> {
    for item in items {
        store.set_score(item.id, score, reason)?;
    }
    Ok(())
}

async fn score_with_model(
    http: &reqwest::Client,
    prompt: &str,
    candidates: &[Item],
) -> Result<Vec<rank::Verdict>> {
    let base = env::var("PAPERS_LLM_URL").unwrap_or_else(|_| DEFAULT_LLM_URL.to_string());
    let model = env::var("PAPERS_LLM_MODEL").unwrap_or_else(|_| DEFAULT_LLM_MODEL.to_string());
    let key = env::var("PAPERS_LLM_KEY").unwrap_or_default();
    rank::score(http, &base, &model, &key, prompt, candidates).await
}

/// stored config wins over the seeded file; the file only bootstraps an empty
/// store, so Flux cannot revert live ranking edits
fn effective_config(store: &Store) -> Result<Config> {
    let raw = match store.load_config()? {
        Some(raw) => raw,
        None => {
            let raw = std::fs::read_to_string(Config::path())
                .with_context(|| format!("reading {}", Config::path().display()))?;
            store.save_config(&raw)?;
            info!("seeded live config from {}", Config::path().display());
            raw
        }
    };
    Config::parse(&raw)
}

fn write_feed(store: &Store) -> Result<()> {
    let entries = store.entries(FEED_ENTRIES)?;
    let Some(updated) = entries.first().map(|item| item.shown_at) else {
        return Ok(());
    };
    let dir = out_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
    let xml = render::atom(&fetch::base_url(), &entries, updated);
    std::fs::write(format!("{dir}/feed.xml"), xml)
        .with_context(|| format!("writing {dir}/feed.xml"))?;
    Ok(())
}

async fn serve() -> Result<()> {
    let store = Store::open(&db_path())?;
    write_feed(&store)?;
    tokio::spawn(async {
        loop {
            if let Err(error) = run(true).await {
                warn!(%error, "scheduled run failed");
            }
            sleep_until_next_run().await;
        }
    });

    let port: u16 = env::var("PAPERS_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8080);
    let seerr = effective_config(&store).ok().and_then(|config| {
        config
            .sources
            .into_iter()
            .find_map(|source| match source.kind {
                config::SourceKind::Seerr { url } => Some(url),
                _ => None,
            })
    });
    let app = app(AppState { seerr });
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    info!(port, "serving the paper");
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Clone)]
struct AppState {
    seerr: Option<String>,
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/icon.png", get(icon))
        .route("/feed.xml", get(serve_feed))
        .route("/add", get(add_media))
        .route("/admin/config", get(admin_get_config).put(admin_put_config))
        .route("/admin/run", post(admin_run))
        .route("/admin/preview", get(admin_preview))
        .with_state(state)
}

/// feed icon, referenced as <icon> and <logo> so readers show it next to the feed
const ICON: &[u8] = include_bytes!("../assets/icon.png");

async fn icon() -> Response {
    ([(header::CONTENT_TYPE, "image/png")], ICON).into_response()
}

async fn serve_feed() -> Response {
    match tokio::fs::read_to_string(format!("{}/feed.xml", out_dir())).await {
        Ok(body) => (
            [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
            body,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no paper yet").into_response(),
    }
}

#[derive(Deserialize)]
struct AddParams {
    media: i64,
    media_type: String,
}

// GET mutates on purpose: the feed only offers plain anchors, and this is a
// LAN-only service nothing crawls
async fn add_media(State(state): State<AppState>, Query(params): Query<AddParams>) -> Response {
    let Some(url) = state.seerr else {
        return (StatusCode::NOT_FOUND, "no seerr source configured").into_response();
    };
    let Ok(key) = env::var("SEERR_API_KEY") else {
        return (StatusCode::NOT_FOUND, "SEERR_API_KEY not set").into_response();
    };
    if params.media_type != "movie" && params.media_type != "tv" {
        return (StatusCode::BAD_REQUEST, "media_type must be movie or tv").into_response();
    }
    let client = match http_client() {
        Ok(client) => client,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    let response = client
        .post(format!("{}/api/v1/request", url.trim_end_matches('/')))
        .header("X-Api-Key", key)
        .json(&serde_json::json!({
            "mediaId": params.media,
            "mediaType": params.media_type,
        }))
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => Redirect::temporary(&format!(
            "https://www.themoviedb.org/{}/{}",
            params.media_type, params.media
        ))
        .into_response(),
        Ok(response) => (
            StatusCode::BAD_GATEWAY,
            format!("seerr said: {}", response.status()),
        )
            .into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, error.to_string()).into_response(),
    }
}

async fn sleep_until_next_run() {
    // hourly by default: fetch, score and publish whatever cleared the bar
    let minutes: u64 = env::var("PAPERS_INTERVAL_MINUTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(60);
    info!(minutes, "sleeping until the next run");
    tokio::time::sleep(Duration::from_secs(minutes * 60)).await;
}

fn admin_guard(headers: &HeaderMap) -> Response {
    let Ok(expected) = env::var("PAPERS_ADMIN_TOKEN") else {
        return (StatusCode::NOT_FOUND, "admin api is disabled").into_response();
    };
    if expected.is_empty() {
        return (StatusCode::NOT_FOUND, "admin api is disabled").into_response();
    }
    let ok = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| {
            // constant-time: the token gates live ranking edits
            subtle::ConstantTimeEq::ct_eq(expected.as_bytes(), token.as_bytes()).into()
        });
    if ok {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "bearer token required").into_response()
    }
}

async fn admin_get_config(State(_seerr): State<AppState>, headers: HeaderMap) -> Response {
    let guard = admin_guard(&headers);
    if !guard.status().is_success() {
        return guard;
    }
    let store = match Store::open(&db_path()) {
        Ok(store) => store,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    match store.load_config() {
        Ok(Some(sources)) => {
            let updated = store.config_updated_at().ok().flatten();
            let body = serde_json::json!({ "sources": sources, "updated_at": updated });
            (
                [(header::CONTENT_TYPE, "application/json")],
                body.to_string(),
            )
                .into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "no config stored yet").into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn admin_put_config(
    State(_seerr): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let guard = admin_guard(&headers);
    if !guard.status().is_success() {
        return guard;
    }
    if let Err(error) = Config::parse(&body) {
        return (StatusCode::BAD_REQUEST, error.to_string()).into_response();
    }
    let store = match Store::open(&db_path()) {
        Ok(store) => store,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    match store.save_config(&body) {
        Ok(()) => (StatusCode::OK, "saved; takes effect on the next run").into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn admin_run(headers: HeaderMap) -> Response {
    let guard = admin_guard(&headers);
    if !guard.status().is_success() {
        return guard;
    }
    tokio::spawn(async {
        if let Err(error) = run(true).await {
            warn!(%error, "admin-triggered run failed");
        }
    });
    (StatusCode::ACCEPTED, "run started").into_response()
}

async fn admin_preview(State(_seerr): State<AppState>, headers: HeaderMap) -> Response {
    let guard = admin_guard(&headers);
    if !guard.status().is_success() {
        return guard;
    }
    let store = match Store::open(&db_path()) {
        Ok(store) => store,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    let config = match effective_config(&store) {
        Ok(config) => config,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    let candidates = match store.unseen(200) {
        Ok(unseen) => rank::prefilter(unseen, &config.rank),
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    if candidates.is_empty() {
        return (
            [(header::CONTENT_TYPE, "application/json")],
            "{\"items\":[]}".to_string(),
        )
            .into_response();
    }
    let http = match http_client() {
        Ok(http) => http,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    let prompt = config
        .rank
        .prompt
        .clone()
        .unwrap_or_else(|| rank::DEFAULT_PROMPT.to_string());
    let verdicts = match score_with_model(&http, &prompt, &candidates).await {
        Ok(verdicts) => verdicts,
        Err(error) => return (StatusCode::BAD_GATEWAY, error.to_string()).into_response(),
    };
    let by_id: HashMap<i64, &store::Item> = candidates.iter().map(|item| (item.id, item)).collect();
    let mut rows: Vec<(String, i64, String)> = verdicts
        .into_iter()
        .filter_map(|verdict| {
            by_id
                .get(&verdict.id)
                .map(|item| (item.title.clone(), verdict.score, verdict.reason.clone()))
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.1));
    let body = serde_json::json!({ "items": rows
        .iter()
        .map(|(title, score, reason)| serde_json::json!({
            "title": title,
            "score": score,
            "reason": reason,
        }))
        .collect::<Vec<_>>() });
    (
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

#[cfg(test)]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn age_window_drops_a_source_backlog_but_keeps_undated_picks() {
        // sources hand over their archive; only the recent tail may publish
        let now = Utc::now();
        let window = cutoff(3);
        assert!(fresh(Some(now - TimeDelta::hours(2)), window));
        assert!(!fresh(Some(now - TimeDelta::days(30)), window));
        assert!(fresh(None, window), "undated seerr picks are current");
        assert!(
            fresh(Some(now - TimeDelta::days(400)), cutoff(0)),
            "0 disables the window"
        );
    }

    #[tokio::test]
    async fn router_builds_and_serves_feeds() {
        let _env = ENV_LOCK.lock().unwrap();
        // the /{feed} route syntax panicked at startup once; build the real
        // router and hit it so route mistakes fail here, not in the cluster
        let dir = std::env::temp_dir().join(format!("papers-route-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // only this test touches PAPERS_OUT, so the raw set_var is race-free here
        unsafe {
            std::env::set_var("PAPERS_OUT", dir.to_str().unwrap());
        }
        std::fs::write(
            dir.join("feed.xml"),
            "<feed/>\
        ",
        )
        .unwrap();

        let app = app(AppState { seerr: None });
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();
        let ok = client
            .get(format!("http://{addr}/healthz"))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.text().await.unwrap(), "ok");

        let feed = client
            .get(format!("http://{addr}/feed.xml"))
            .send()
            .await
            .unwrap();
        assert_eq!(feed.status(), 200);
        assert_eq!(feed.text().await.unwrap(), "<feed/>");

        for dead in [
            "nope.xml",
            "news.xml",
            "media.xml",
            "daily.xml",
            "../etc.xml",
        ] {
            let missing = client
                .get(format!("http://{addr}/{dead}"))
                .send()
                .await
                .unwrap();
            assert_eq!(missing.status(), 404, "{dead} must not serve a feed");
        }
        let admin = client
            .get(format!("http://{addr}/admin/config"))
            .send()
            .await
            .unwrap();
        assert_eq!(admin.status(), 404, "admin is dark without a token");

        let icon = client
            .get(format!("http://{addr}/icon.png"))
            .send()
            .await
            .unwrap();
        assert_eq!(icon.status(), 200, "the feed icon must be reachable");
        assert_eq!(
            icon.headers().get("content-type").unwrap(),
            "image/png",
            "readers sniff the icon type"
        );
        assert_eq!(icon.bytes().await.unwrap().as_ref(), ICON);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn admin_guard_accepts_only_matching_bearer() {
        // only this test touches PAPERS_ADMIN_TOKEN, so the raw set/remove is
        // race-free here despite the unsafe marker on env mutation
        unsafe {
            env::set_var("PAPERS_ADMIN_TOKEN", "secret-token-123");
        }
        let mut headers = HeaderMap::new();
        assert_eq!(admin_guard(&headers).status(), StatusCode::UNAUTHORIZED);
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong"),
        );
        assert_eq!(admin_guard(&headers).status(), StatusCode::UNAUTHORIZED);
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token-123"),
        );
        assert_eq!(admin_guard(&headers).status(), StatusCode::OK);
        unsafe {
            env::remove_var("PAPERS_ADMIN_TOKEN");
        }
        assert_eq!(admin_guard(&headers).status(), StatusCode::NOT_FOUND);
    }
}

#[cfg(test)]
mod e2e {
    use super::*;
    use axum::Json;
    use axum::routing::{get as get_route, post as post_route};

    /// boots stub feed + seerr + litellm servers, runs the real pipeline
    /// against a temp db, and asserts the observable feed files
    #[tokio::test]
    async fn pipeline_publishes_one_feed_with_stub_servers() {
        let _env = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("papers-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("out")).unwrap();
        unsafe {
            env::set_var("PAPERS_DB", dir.join("papers.db").to_str().unwrap());
            env::set_var("PAPERS_OUT", dir.join("out").to_str().unwrap());
            env::set_var("SEERR_API_KEY", "stub-key");
            env::remove_var("PAPERS_LLM_MODEL");
        }

        // stub news feed: three articles
        let rss = r#"<?xml version="1.0"?><rss version="2.0"><channel>
            <item><title>News A</title><link>https://stub.test/a</link></item>
            <item><title>News B</title><link>https://stub.test/b</link></item>
            <item><title>News C</title><link>https://stub.test/c</link></item>
        </channel></rss>"#;
        let feed_app = Router::new().route(
            "/rss",
            get_route(move || async {
                (
                    [(header::CONTENT_TYPE, "application/rss+xml")],
                    rss.to_string(),
                )
            }),
        );
        let feed_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let feed_addr = feed_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(feed_listener, feed_app).await.unwrap();
        });

        // stub seerr: trending with one movie, detail with a youtube trailer
        let trending = serde_json::json!({
            "results": [{
                "id": 42, "mediaType": "movie", "title": "Stub Movie (2026)",
                "overview": "a stub", "releaseDate": "2026-01-01"
            }]
        });
        let detail = serde_json::json!({
            "relatedVideos": [{"key": "stubtrailer", "type": "Trailer", "site": "YouTube"}]
        });
        let seerr_app = Router::new()
            .route(
                "/api/v1/discover/trending",
                get_route(move || async move { Json(trending.clone()) }),
            )
            .route(
                "/api/v1/movie/42",
                get_route(move || async move { Json(detail.clone()) }),
            );
        let seerr_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let seerr_addr = seerr_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(seerr_listener, seerr_app).await.unwrap();
        });

        // stub litellm: score every news item 9, below the curated seerr pick
        let llm_app = Router::new().route(
            "/v1/chat/completions",
            post_route(|| async {
                Json(serde_json::json!({
                    "choices": [{"message": {"content": r#"{"items":[
                            {"id": 1, "score": 9, "reason": "stub"},
                            {"id": 2, "score": 9, "reason": "stub"},
                            {"id": 3, "score": 9, "reason": "stub"}
                        ]}"#}}]
                }))
            }),
        );
        let llm_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let llm_addr = llm_listener.local_addr().unwrap();
        unsafe {
            env::set_var("PAPERS_LLM_URL", format!("http://{llm_addr}/v1"));
        }
        tokio::spawn(async move {
            axum::serve(llm_listener, llm_app).await.unwrap();
        });

        // live config pointing at the stubs
        let store = Store::open(&db_path()).unwrap();
        let config = format!(
            "max_per_run = 10\n[[sources]]\nname = \"stub-news\"\nkind = \"feed\"\nurl = \"http://{feed_addr}/rss\"\n\n[[sources]]\nname = \"stub-seerr\"\nkind = \"seerr\"\nurl = \"http://{seerr_addr}\"\nlabel = \"New in Seerr\"\n"
        );
        store.save_config(&config).unwrap();

        run(true).await.unwrap();

        // one feed file carries both lanes, and the media item keeps its
        // trailer iframe even though every news item out-scored it
        let feed = std::fs::read_to_string(dir.join("out/feed.xml")).unwrap();
        assert!(feed.contains("News A"), "news items are published");
        assert!(
            feed.contains("&lt;p&gt;&lt;em&gt;stub&lt;/em&gt;"),
            "news items were scored by the stub LLM and escaped for readers"
        );
        assert!(
            feed.contains("New in Seerr: Stub Movie (2026)"),
            "labels prefix the entry title: {feed}"
        );
        let parsed = feed_rs::parser::parse(feed.as_bytes()).expect("published feed parses");
        let seerr_entry = parsed
            .entries
            .iter()
            .find(|entry| {
                entry
                    .title
                    .as_ref()
                    .is_some_and(|title| title.content.starts_with("New in Seerr:"))
            })
            .expect("the media pick is in the paper");
        let body = seerr_entry
            .content
            .as_ref()
            .and_then(|content| content.body.clone())
            .unwrap_or_default();
        assert!(
            body.contains("youtube.com/embed/stubtrailer"),
            "trailer plays inline: {body}"
        );
        assert!(
            body.contains("/add?media=42&amp;media_type=movie"),
            "add link keeps its entity: {body}"
        );

        // second run with nothing new: the paper stays put
        run(true).await.unwrap();
        let again = std::fs::read_to_string(dir.join("out/feed.xml")).unwrap();
        assert_eq!(feed, again);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
