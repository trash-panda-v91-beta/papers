use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{Client, Url};
use scraper::{Html, Selector};
use serde::Deserialize;
use std::env;

use crate::config::{Source, SourceKind};
use crate::render::escape;

pub struct Fetched {
    pub title: String,
    pub link: String,
    pub summary: String,
    pub published: Option<DateTime<Utc>>,
    pub extra: String,
    pub image: String,
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
        SourceKind::Seerr { url } => fetch_seerr(client, url).await?,
    };
    let cap = source.max_per_fetch.unwrap_or(usize::MAX);
    Ok(items.into_iter().take(cap).collect())
}

pub fn public_base() -> String {
    // the feed URL doubles as the public base for feed files and /add links;
    // accept a bare host or a legacy .../daily.xml value
    let url =
        env::var("PAPERS_FEED_URL").unwrap_or_else(|_| "http://localhost:8080/daily.xml".into());
    match url.rsplit_once('/') {
        Some((base, last)) if last.ends_with(".xml") => base.to_string(),
        _ => url,
    }
}

#[derive(Deserialize)]
struct SeerrTrending {
    #[serde(default)]
    results: Vec<SeerrMedia>,
}

#[derive(Deserialize)]
struct SeerrMedia {
    id: i64,
    #[serde(rename = "mediaType")]
    media_type: String,
    title: Option<String>,
    name: Option<String>,
    overview: Option<String>,
    #[serde(rename = "releaseDate")]
    release_date: Option<String>,
    #[serde(rename = "firstAirDate")]
    first_air_date: Option<String>,
    #[serde(rename = "posterPath")]
    poster_path: Option<String>,
}

#[derive(Deserialize, Default)]
struct SeerrVideos {
    #[serde(default, rename = "relatedVideos")]
    related_videos: Vec<SeerrVideo>,
}

#[derive(Deserialize)]
struct SeerrVideo {
    key: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    site: Option<String>,
}

async fn fetch_seerr(client: &Client, url: &str) -> Result<Vec<Fetched>> {
    let key = std::env::var("SEERR_API_KEY").context("SEERR_API_KEY not set")?;
    let base = url.trim_end_matches('/');
    let trending: SeerrTrending = client
        .get(format!("{base}/api/v1/discover/trending"))
        .header("X-Api-Key", &key)
        .send()
        .await
        .with_context(|| format!("GET {url} trending"))?
        .error_for_status()?
        .json()
        .await
        .context("decoding seerr trending")?;
    let add_base = public_base();
    let mut items = Vec::new();
    for media in trending.results {
        let title = media
            .title
            .clone()
            .or(media.name.clone())
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        let trailer = trailer_for(client, &key, base, &media).await;
        items.push(seerr_item(&media, &title, trailer.as_deref(), &add_base));
    }
    Ok(items)
}

fn seerr_item(media: &SeerrMedia, title: &str, trailer: Option<&str>, add_base: &str) -> Fetched {
    let year = media
        .release_date
        .as_deref()
        .or(media.first_air_date.as_deref())
        .and_then(|date| date.get(0..4))
        .map(|year| format!(" ({year})"))
        .unwrap_or_default();
    let add = format!(
        "{add_base}/add?media={}&media_type={}",
        media.id, media.media_type
    );
    let mut anchors = Vec::new();
    if let Some(url) = trailer {
        anchors.push(format!("<a href=\"{}\">trailer</a>", escape(url)));
    }
    anchors.push(format!("<a href=\"{}\">add</a>", escape(&add)));
    Fetched {
        title: format!("{title}{year}"),
        link: format!(
            "https://www.themoviedb.org/{}/{}",
            media.media_type, media.id
        ),
        summary: truncate(media.overview.as_deref().unwrap_or_default()),
        published: None,
        extra: anchors.join(" - "),
        image: poster_url(media.poster_path.as_deref()),
    }
}

/// tmdb serves posters by size prefix; w500 is the sweet spot for readers
fn poster_url(path: Option<&str>) -> String {
    match path {
        Some(path) if !path.is_empty() => format!("https://image.tmdb.org/t/p/w500{path}"),
        _ => String::new(),
    }
}

async fn trailer_for(client: &Client, key: &str, base: &str, media: &SeerrMedia) -> Option<String> {
    let kind = &media.media_type;
    let response = client
        .get(format!("{base}/api/v1/{kind}/{}", media.id))
        .header("X-Api-Key", key)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    pick_trailer(&response.json::<SeerrVideos>().await.ok()?)
}

fn pick_trailer(videos: &SeerrVideos) -> Option<String> {
    let video = videos.related_videos.iter().find(|video| {
        video.site.as_deref() == Some("YouTube") && video.kind.as_deref() == Some("Trailer")
    })?;
    Some(format!(
        "https://www.youtube.com/watch?v={}",
        video.key.as_deref()?
    ))
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
            let image = entry_image(&entry);
            Some(Fetched {
                title: entry.title.map(|t| t.content).unwrap_or_default(),
                link,
                summary: entry
                    .summary
                    .map(|s| truncate(&s.content))
                    .unwrap_or_default(),
                published: entry.published.or(entry.updated),
                extra: String::new(),
                image,
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
                extra: String::new(),
                image: String::new(),
            })
        })
        .filter(|f| !f.title.is_empty())
        .collect())
}

fn selector(raw: &str) -> Result<Selector> {
    Selector::parse(raw).map_err(|e| anyhow::anyhow!("bad selector {raw:?}: {e:?}"))
}

/// best available image for an entry: media thumbnail, image media content,
/// image enclosure, then the first <img> in the entry html. Empty when none.
fn entry_image(entry: &feed_rs::model::Entry) -> String {
    for media in &entry.media {
        if let Some(thumb) = media.thumbnails.first() {
            return thumb.image.uri.clone();
        }
        if let Some(content) = media.content.iter().find(|c| {
            c.content_type
                .as_ref()
                .is_some_and(|t| t.to_string().starts_with("image/"))
        }) {
            if let Some(url) = &content.url {
                return url.to_string();
            }
        }
    }
    for link in &entry.links {
        if link.rel.as_deref() == Some("enclosure")
            && link
                .media_type
                .as_deref()
                .is_some_and(|mime| mime.starts_with("image/"))
        {
            return link.href.clone();
        }
    }
    let html = entry
        .content
        .as_ref()
        .and_then(|c| c.body.clone())
        .or_else(|| entry.summary.as_ref().map(|s| s.content.clone()));
    html.as_deref().and_then(first_img_src).unwrap_or_default()
}

fn first_img_src(html: &str) -> Option<String> {
    let doc = Html::parse_fragment(html);
    let sel = Selector::parse("img").ok()?;
    doc.select(&sel)
        .next()
        .and_then(|node| node.value().attr("src"))
        .map(str::to_string)
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
        assert_eq!(links, vec!["https://example.com/one", "https://x/two"]);
    }

    fn media(media_type: &str, title: &str, date: &str) -> SeerrMedia {
        serde_json::from_value(serde_json::json!({
            "id": 693134,
            "mediaType": media_type,
            "title": title,
            "name": title,
            "overview": "space sequel",
            "releaseDate": date,
            "firstAirDate": date,
            "posterPath": "/poster.jpg",
        }))
        .unwrap()
    }

    #[test]
    fn seerr_item_maps_title_year_links_and_anchors() {
        let item = seerr_item(
            &media("movie", "Dune: Part Two", "2024-03-01"),
            "Dune: Part Two",
            Some("https://www.youtube.com/watch?v=abc"),
            "https://papers.internal",
        );
        assert_eq!(item.title, "Dune: Part Two (2024)");
        assert_eq!(item.link, "https://www.themoviedb.org/movie/693134");
        assert_eq!(item.summary, "space sequel");
        assert_eq!(
            item.extra,
            "<a href=\"https://www.youtube.com/watch?v=abc\">trailer</a> - \
             <a href=\"https://papers.internal/add?media=693134&amp;media_type=movie\">add</a>"
        );
    }

    #[test]
    fn seerr_tv_uses_name_and_first_air_date() {
        let item = seerr_item(
            &media("tv", "Severance", "2022-02-18"),
            "Severance",
            None,
            "https://papers.internal",
        );
        assert_eq!(item.title, "Severance (2022)");
        assert_eq!(item.link, "https://www.themoviedb.org/tv/693134");
        assert_eq!(
            item.extra,
            "<a href=\"https://papers.internal/add?media=693134&amp;media_type=tv\">add</a>"
        );
    }

    #[test]
    fn pick_trailer_takes_first_youtube_trailer() {
        let videos: SeerrVideos = serde_json::from_str(
            r#"{"relatedVideos": [
                 {"key": "clip1", "type": "Clip", "site": "YouTube"},
                 {"key": "vimeo1", "type": "Trailer", "site": "Vimeo"},
                 {"key": "real1", "type": "Trailer", "site": "YouTube"}
               ]}"#,
        )
        .unwrap();
        assert_eq!(
            pick_trailer(&videos).unwrap(),
            "https://www.youtube.com/watch?v=real1"
        );
        assert!(pick_trailer(&SeerrVideos::default()).is_none());
    }

    fn parsed_entry(xml: &str) -> feed_rs::model::Entry {
        let feed = feed_rs::parser::parse(xml.as_bytes()).expect("feed parses");
        feed.entries.into_iter().next().expect("one entry")
    }

    #[test]
    fn seerr_poster_becomes_the_entry_image() {
        let item = seerr_item(
            &media("movie", "Dune", "2024-03-01"),
            "Dune",
            None,
            "https://p",
        );
        assert_eq!(item.image, "https://image.tmdb.org/t/p/w500/poster.jpg");
    }

    #[test]
    fn poster_url_is_empty_without_a_path() {
        assert_eq!(poster_url(None), "");
        assert_eq!(poster_url(Some("")), "");
    }

    #[test]
    fn entry_image_prefers_media_content_over_inline_html() {
        // feed-rs drops a lone media:thumbnail (its media object is only emitted
        // when media:content is present), so media:content is the reliable hook
        let entry = parsed_entry(
            r#"<rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/"><channel><title>t</title>
               <item><title>a</title><link>https://x/1</link>
                 <media:group><media:content url="https://x/media.jpg" type="image/jpeg"/></media:group>
                 <description>&lt;img src="https://x/inline.jpg"/&gt;</description>
               </item></channel></rss>"#,
        );
        assert_eq!(entry_image(&entry), "https://x/media.jpg");
    }

    #[test]
    fn entry_image_falls_back_to_an_inline_img() {
        let entry = parsed_entry(
            r#"<rss version="2.0"><channel><title>t</title>
               <item><title>a</title><link>https://x/1</link>
                 <description>&lt;img src="https://x/inline.jpg"/&gt; text</description>
               </item></channel></rss>"#,
        );
        assert_eq!(entry_image(&entry), "https://x/inline.jpg");
    }

    #[test]
    fn entry_without_images_yields_empty() {
        let entry = parsed_entry(
            r#"<rss version="2.0"><channel><title>t</title>
               <item><title>a</title><link>https://x/1</link></item></channel></rss>"#,
        );
        assert_eq!(entry_image(&entry), "");
    }
}
