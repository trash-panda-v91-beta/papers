mod config;
mod fetch;
mod rank;
mod render;
mod store;

use std::env;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{Local, TimeDelta, Utc};
use tracing::{info, warn};

use crate::config::Config;
use crate::store::{Item, NewItem, Store};

const DEFAULT_LLM_URL: &str = "http://litellm.ai.svc.cluster.local:4000/v1";
const DEFAULT_LLM_MODEL: &str = "deepseek-v4.1-flash";

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
                println!("{} (weight {})", source.name, source.weight);
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
    let config = Config::load(&Config::path())?;
    let store = Store::open(&db_path())?;
    let http = http_client()?;

    let mut added = 0usize;
    for source in &config.sources {
        match fetch::fetch(&http, source).await {
            Ok(items) => {
                for item in items {
                    let new_item = NewItem {
                        source: source.name.clone(),
                        title: item.title,
                        link: item.link,
                        summary: item.summary,
                        published: item.published,
                        weight: source.weight,
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
    if candidates.is_empty() {
        return Ok(());
    }

    if use_model {
        match score_with_model(&http, &candidates).await {
            Ok(verdicts) => {
                for verdict in verdicts {
                    store.set_score(verdict.id, verdict.score, &verdict.reason)?;
                }
            }
            Err(error) => {
                warn!(%error, "model scoring failed, falling back to source weights");
                score_by_weight(&candidates, &store, "rule score (model unavailable)")?;
            }
        }
    } else {
        score_by_weight(&candidates, &store, "rule score")?;
    }

    let top = store.top(config.keep_per_day)?;
    if top.is_empty() {
        info!("nothing cleared the bar today");
        return Ok(());
    }
    let day = Local::now().format("%Y-%m-%d").to_string();
    let (title, body) = render::paper_entry(&day, &top);
    store.save_digest(&day, &title, &body)?;
    let ids: Vec<i64> = top.iter().map(|item| item.id).collect();
    store.mark_shown(&ids)?;
    write_feed(&store)?;
    info!(day = %day, kept = ids.len(), "paper printed");
    Ok(())
}

fn score_by_weight(items: &[Item], store: &Store, reason: &str) -> Result<()> {
    for item in items {
        store.set_score(item.id, item.weight, reason)?;
    }
    Ok(())
}

async fn score_with_model(
    http: &reqwest::Client,
    candidates: &[Item],
) -> Result<Vec<rank::Verdict>> {
    let base = env::var("PAPERS_LLM_URL").unwrap_or_else(|_| DEFAULT_LLM_URL.to_string());
    let model = env::var("PAPERS_LLM_MODEL").unwrap_or_else(|_| DEFAULT_LLM_MODEL.to_string());
    let key = env::var("PAPERS_LLM_KEY").unwrap_or_default();
    rank::score(http, &base, &model, &key, candidates).await
}

fn write_feed(store: &Store) -> Result<()> {
    let dir = out_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
    let url = env::var("PAPERS_FEED_URL")
        .unwrap_or_else(|_| "http://localhost:8080/daily.xml".to_string());
    let xml = render::atom(&url, &store.digests(30)?, Utc::now());
    std::fs::write(format!("{dir}/daily.xml"), xml)
        .with_context(|| format!("writing {dir}/daily.xml"))?;
    Ok(())
}

async fn serve() -> Result<()> {
    write_feed(&Store::open(&db_path())?)?;
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
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/daily.xml", get(serve_feed));
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    info!(port, "serving the paper");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn serve_feed() -> Response {
    match tokio::fs::read_to_string(format!("{}/daily.xml", out_dir())).await {
        Ok(body) => (
            [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
            body,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no paper printed yet").into_response(),
    }
}

async fn sleep_until_next_run() {
    let hour: u32 = env::var("PAPERS_HOUR")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(6);
    let now = Local::now();
    let mut next = now
        .date_naive()
        .and_hms_opt(hour, 0, 0)
        .and_then(|naive| naive.and_local_timezone(Local).single())
        .unwrap_or_else(|| now + TimeDelta::hours(1));
    if next <= now {
        next += TimeDelta::days(1);
    }
    let wait = (next - now).to_std().unwrap_or(Duration::from_secs(3600));
    info!(seconds = wait.as_secs(), "sleeping until the next run");
    tokio::time::sleep(wait).await;
}
