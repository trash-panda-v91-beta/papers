use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{Client, Url};
use scraper::{Html, Selector};

use crate::config::{Source, SourceKind};

pub struct Fetched {
    pub title: String,
    pub link: String,
    pub summary: String,
    pub published: Option<DateTime<Utc>>,
}

const SUMMARY_LIMIT: usize = 400;

pub async fn fetch(client: &Client, source: &Source) -> Result<Vec<Fetched>> {
    let items = match &source.kind {
        SourceKind::Feed { url } => fetch_feed(client, url).await?,
        SourceKind::Html {
            url,
            item,
            title,
            link,
            summary,
        } => fetch_html(client, url, item, title, link, summary.as_deref()).await?,
    };
    let cap = source.max_per_fetch.unwrap_or(usize::MAX);
    Ok(items.into_iter().take(cap).collect())
}

async fn fetch_feed(client: &Client, url: &str) -> Result<Vec<Fetched>> {
    let bytes = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?
        .bytes()
        .await?;
    let feed = feed_rs::parser::parse(&bytes[..]).with_context(|| format!("parsing feed {url}"))?;
    Ok(feed
        .entries
        .into_iter()
        .filter_map(|entry| {
            let link = entry.links.first()?.href.clone();
            Some(Fetched {
                title: entry.title.map(|t| t.content).unwrap_or_default(),
                link,
                summary: entry
                    .summary
                    .map(|s| truncate(&s.content))
                    .unwrap_or_default(),
                published: entry.published.or(entry.updated),
            })
        })
        .collect())
}

async fn fetch_html(
    client: &Client,
    url: &str,
    item: &str,
    title: &str,
    link: &str,
    summary: Option<&str>,
) -> Result<Vec<Fetched>> {
    let body = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?
        .text()
        .await?;
    let doc = Html::parse_document(&body);
    let base = Url::parse(url)?;
    let item_sel = selector(item)?;
    let title_sel = selector(title)?;
    let link_sel = selector(link)?;
    let summary_sel = summary.map(selector).transpose()?;

    Ok(doc
        .select(&item_sel)
        .filter_map(|node| {
            let href = node
                .select(&link_sel)
                .next()
                .and_then(|n| n.value().attr("href"))
                .or_else(|| node.value().attr("href"))?;
            let link = base.join(href).ok()?.to_string();
            let title = node
                .select(&title_sel)
                .next()
                .map(|n| n.text().collect::<String>())
                .unwrap_or_default()
                .trim()
                .to_string();
            let summary = summary_sel
                .as_ref()
                .and_then(|s| node.select(s).next())
                .map(|n| truncate(&n.text().collect::<String>()))
                .unwrap_or_default();
            Some(Fetched {
                title,
                link,
                summary,
                published: None,
            })
        })
        .filter(|f| !f.title.is_empty())
        .collect())
}

fn selector(raw: &str) -> Result<Selector> {
    Selector::parse(raw).map_err(|e| anyhow::anyhow!("bad selector {raw:?}: {e:?}"))
}

fn truncate(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(SUMMARY_LIMIT) {
        Some((idx, _)) => format!("{}...", &flat[..idx]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_collapses_whitespace_and_caps() {
        assert_eq!(truncate(" a\n b   c "), "a b c");
        let long = "x".repeat(SUMMARY_LIMIT + 10);
        assert!(truncate(&long).ends_with("..."));
        assert_eq!(truncate(&long).chars().count(), SUMMARY_LIMIT + 3);
    }

    #[test]
    fn html_items_resolve_relative_links() {
        let body = r#"<div class="t"><a href="/one">First</a></div><div class="t"><a href="https://x/two">Second</a></div>"#;
        let doc = Html::parse_document(body);
        let item_sel = selector("div.t").unwrap();
        let link_sel = selector("a").unwrap();
        let items = doc.select(&item_sel);
        let base = Url::parse("https://example.com/index").unwrap();
        let links: Vec<String> = items
            .filter_map(|n| n.select(&link_sel).next())
            .filter_map(|a| a.value().attr("href"))
            .map(|h| base.join(h).unwrap().to_string())
            .collect();
        assert_eq!(
            links,
            vec!["https://example.com/one", "https://x/two"]
        );
    }
}
