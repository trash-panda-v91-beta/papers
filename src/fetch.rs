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
    let mut items = match &source.kind {
        SourceKind::Feed { url } => fetch_feed(client, url).await?,
        SourceKind::Html {
            url,
            item,
            title,
            link,
            summary,
        } => fetch_html(client, url, item, title, link, summary.as_deref()).await?,
        SourceKind::Seerr { url } => fetch_seerr(client, url).await?,
        SourceKind::Jellyfin { url, libraries } => fetch_jellyfin(client, url, libraries).await?,
    };
    if let Some(label) = &source.label {
        for item in &mut items {
            item.title = format!("{label}: {}", item.title);
        }
    }
    let cap = source.max_per_fetch.unwrap_or(usize::MAX);
    Ok(items.into_iter().take(cap).collect())
}

/// public host the feed and its /add links are served from
pub fn base_url() -> String {
    let url = env::var("PAPERS_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    url.trim_end_matches('/').to_string()
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
    #[serde(rename = "voteAverage", default)]
    vote_average: Option<f64>,
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
    let add_base = base_url();
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
    let mut extra = String::new();
    if let Some(rating) = media.vote_average.filter(|rating| *rating > 0.0) {
        extra.push_str(&format!(
            "<p>{rating:.1} / 10 - {}</p>\n",
            escape(&media.media_type)
        ));
    }
    // a youtube trailer plays inline in reeder; the plain link stays as the fallback
    if let Some(key) = &trailer {
        extra.push_str(&format!(
            "<p><iframe width=\"100%\" height=\"315\" \
             src=\"https://www.youtube.com/embed/{key}\" frameborder=\"0\" \
             allowfullscreen=\"true\"></iframe></p>\n"
        ));
    }
    let mut anchors = Vec::new();
    if let Some(key) = &trailer {
        anchors.push(format!(
            "<a href=\"https://www.youtube.com/watch?v={key}\">trailer</a>"
        ));
    }
    anchors.push(format!("<a href=\"{}\">add</a>", escape(&add)));
    extra.push_str(&format!("<p>{}</p>", anchors.join(" - ")));
    Fetched {
        title: format!("{title}{year}"),
        link: format!(
            "https://www.themoviedb.org/{}/{}",
            media.media_type, media.id
        ),
        summary: truncate(media.overview.as_deref().unwrap_or_default()),
        published: None,
        extra,
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
    video.key.clone()
}

#[derive(Deserialize)]
struct JellyfinItems {
    #[serde(default, rename = "Items")]
    items: Vec<JellyfinItem>,
}

#[derive(Deserialize)]
struct JellyfinItem {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
    /// Movie or Series; collections and folders slip into the query anyway
    #[serde(rename = "Type")]
    kind: String,
    #[serde(rename = "ParentIndexNumber")]
    parent_index_number: Option<i64>,
    #[serde(rename = "IndexNumber")]
    index_number: Option<i64>,
    #[serde(rename = "LocationType")]
    location_type: Option<String>,
    #[serde(rename = "ProductionYear")]
    production_year: Option<i64>,
    #[serde(rename = "Overview")]
    overview: Option<String>,
    #[serde(rename = "CommunityRating")]
    community_rating: Option<f64>,
    #[serde(rename = "OfficialRating")]
    official_rating: Option<String>,
    #[serde(rename = "RunTimeTicks")]
    run_time_ticks: Option<i64>,
    #[serde(default, rename = "Genres")]
    genres: Vec<String>,
    /// Tmdb / Imdb ids, which is how an entry links somewhere readable
    #[serde(default, rename = "ProviderIds")]
    provider_ids: std::collections::HashMap<String, String>,
    #[serde(rename = "DateCreated")]
    date_created: Option<String>,
    #[serde(default, rename = "RemoteTrailers")]
    remote_trailers: Vec<JellyfinTrailer>,
}

#[derive(Deserialize)]
struct JellyfinTrailer {
    #[serde(rename = "Url")]
    url: Option<String>,
}

#[derive(Deserialize)]
struct JellyfinLibrary {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "ItemId")]
    item_id: String,
}

/// newest additions first, with the fields an entry needs. Kept as one string
/// because reqwest 0.13 ships no query builder
const JELLYFIN_QUERY: &str = "Recursive=true&IncludeItemTypes=Movie,Series\
     &SortBy=DateCreated&SortOrder=Descending&Limit=30\
     &Fields=Overview,ProductionYear,CommunityRating,RemoteTrailers,DateCreated,Genres,ProviderIds\
     &EnableImageTypes=Primary";

/// what recently landed in the library, newest first. Without `libraries` the
/// server's whole catalogue answers, which on a box with a DVR or a YouTube
/// archive library is mostly noise.
async fn fetch_jellyfin(client: &Client, url: &str, libraries: &[String]) -> Result<Vec<Fetched>> {
    let key = env::var("JELLYFIN_API_KEY").context("JELLYFIN_API_KEY not set")?;
    let base = url.trim_end_matches('/');
    let mut fetched = Vec::new();
    for parent in jellyfin_parents(client, base, &key, libraries).await? {
        let scope = parent
            .map(|id| format!("&ParentId={id}"))
            .unwrap_or_default();
        let response: JellyfinItems = client
            .get(format!("{base}/Items?{JELLYFIN_QUERY}{scope}"))
            // the ingress rejects X-Emby-Token, so use the documented header
            .header("Authorization", format!("MediaBrowser Token=\"{key}\""))
            .send()
            .await
            .with_context(|| format!("GET {base}/Items"))?
            .error_for_status()?
            .json()
            .await
            .context("decoding jellyfin items")?;
        for item in response
            .items
            .iter()
            .filter(|item| matches!(item.kind.as_str(), "Movie" | "Series"))
        {
            let episode = match item.kind.as_str() {
                "Series" => latest_episode(client, base, &key, &item.id).await,
                _ => None,
            };
            fetched.push(jellyfin_item(item, base, episode.as_deref()));
        }
    }
    // each library answers newest first, so merge on the date: otherwise the
    // first library fills `max_per_fetch` and the rest never publish
    fetched.sort_by_key(|item| std::cmp::Reverse(item.published));
    Ok(fetched)
}

/// library ids to query, or one unscoped pass when no libraries are named
async fn jellyfin_parents(
    client: &Client,
    base: &str,
    key: &str,
    libraries: &[String],
) -> Result<Vec<Option<String>>> {
    if libraries.is_empty() {
        return Ok(vec![None]);
    }
    let known: Vec<JellyfinLibrary> = client
        .get(format!("{base}/Library/VirtualFolders"))
        .header("Authorization", format!("MediaBrowser Token=\"{key}\""))
        .send()
        .await
        .with_context(|| format!("GET {base}/Library/VirtualFolders"))?
        .error_for_status()?
        .json()
        .await
        .context("decoding jellyfin libraries")?;
    libraries
        .iter()
        .map(|wanted| {
            known
                .iter()
                .find(|library| library.name.eq_ignore_ascii_case(wanted))
                .map(|library| Some(library.item_id.clone()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "jellyfin has no library named {wanted:?}, has: {}",
                        known
                            .iter()
                            .map(|library| library.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
        })
        .collect()
}

fn jellyfin_item(item: &JellyfinItem, base: &str, episode: Option<&str>) -> Fetched {
    let year = item
        .production_year
        .map(|year| format!(" ({year})"))
        .unwrap_or_default();
    let mut extra = String::new();
    let mut meta = vec![match item.kind.as_str() {
        "Series" => "<strong>TV Show</strong>".to_string(),
        _ => "<strong>Movie</strong>".to_string(),
    }];
    if let Some(rating) = item.community_rating.filter(|rating| *rating > 0.0) {
        meta.push(format!("\u{1f345} {rating:.1} / 10"));
    }
    if let Some(rated) = item
        .official_rating
        .as_deref()
        .filter(|rated| !rated.is_empty())
    {
        meta.push(escape(rated));
    }
    if let Some(minutes) = item.run_time_ticks.and_then(runtime_minutes) {
        meta.push(minutes);
    }
    if !item.genres.is_empty() {
        meta.push(escape(&item.genres.join(", ")));
    }
    extra.push_str(&format!("<p>{}</p>\n", meta.join(" | ")));
    if let Some(episode) = episode {
        extra.push_str(&format!("<p>{episode}</p>\n"));
    }
    let trailer = item
        .remote_trailers
        .iter()
        .find_map(|trailer| trailer.url.as_deref())
        .and_then(youtube_key);
    if let Some(key) = &trailer {
        extra.push_str(&format!(
            "<p><iframe width=\"100%\" height=\"315\" \
             src=\"https://www.youtube.com/embed/{key}\" frameborder=\"0\" \
             allowfullscreen=\"true\"></iframe></p>\n"
        ));
    }
    let watch = format!("{base}/web/#/details?id={}", item.id);
    let mut anchors = vec![format!("<a href=\"{}\">watch</a>", escape(&watch))];
    if let Some(key) = &trailer {
        anchors.push(format!(
            "<a href=\"https://www.youtube.com/watch?v={key}\">trailer</a>"
        ));
    }
    extra.push_str(&format!("<p>{}</p>", anchors.join(" | ")));
    Fetched {
        title: format!("{}{year}", item.name),
        // a readable page, so a reader can offer reader view; the app link
        // stays in the body
        link: readable_link(item).unwrap_or(watch),
        summary: truncate(item.overview.as_deref().unwrap_or_default()),
        published: item
            .date_created
            .as_deref()
            .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
            .map(|at| at.with_timezone(&Utc)),
        extra,
        // the image endpoint needs no token, so a reader can fetch it directly
        image: format!("{base}/Items/{}/Images/Primary?maxHeight=450", item.id),
    }
}

/// jellyfin itself is an app page, so an entry points at tmdb or imdb instead
/// and reader view has an article to extract
fn readable_link(item: &JellyfinItem) -> Option<String> {
    if let Some(id) = item.provider_ids.get("Tmdb") {
        let kind = match item.kind.as_str() {
            "Series" => "tv",
            _ => "movie",
        };
        return Some(format!("https://www.themoviedb.org/{kind}/{id}"));
    }
    item.provider_ids
        .get("Imdb")
        .map(|id| format!("https://www.imdb.com/title/{id}/"))
}

/// newest episode on disk, so a show that mattered enough to add says what
/// actually landed. Jellyfin only knows episodes it has, so no airing dates.
async fn latest_episode(client: &Client, base: &str, key: &str, series: &str) -> Option<String> {
    let episodes: JellyfinItems = client
        .get(format!(
            "{base}/Shows/{series}/Episodes?SortBy=DateCreated&SortOrder=Descending\
             &Limit=1&Fields=PremiereDate,LocationType"
        ))
        .header("Authorization", format!("MediaBrowser Token=\"{key}\""))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let episode = episodes.items.first()?;
    let number = match (episode.parent_index_number, episode.index_number) {
        (Some(season), Some(index)) => format!("S{season:02}E{index:02}"),
        _ => String::new(),
    };
    let available = match episode.location_type.as_deref() {
        Some("FileSystem") => " \u{00b7} available to watch",
        _ => "",
    };
    Some(format!(
        "<strong>{number}</strong> {}{available}",
        escape(&episode.name)
    ))
}

/// jellyfin counts 100ns ticks; readers want `1h 52m`
fn runtime_minutes(ticks: i64) -> Option<String> {
    let minutes = ticks / 10_000_000 / 60;
    match minutes {
        0 => None,
        minutes if minutes < 60 => Some(format!("{minutes}m")),
        minutes => Some(format!("{}h {}m", minutes / 60, minutes % 60)),
    }
}

/// jellyfin stores trailers as watch urls; readers want the embed key
fn youtube_key(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    if host != "youtu.be" && !host.ends_with("youtube.com") {
        return None;
    }
    if let Some((_, key)) = parsed.query_pairs().find(|(name, _)| name == "v") {
        return Some(key.into_owned());
    }
    let key = parsed
        .path_segments()?
        .rfind(|segment| !segment.is_empty())?;
    Some(key.to_string())
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
                    .map(|summary| truncate(&plain_text(&summary.content)))
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
        for content in &media.content {
            let Some(url) = &content.url else { continue };
            let typed_image = content
                .content_type
                .as_ref()
                .is_some_and(|kind| kind.to_string().starts_with("image/"));
            if typed_image || looks_like_image(url.as_str()) {
                return url.to_string();
            }
        }
    }
    for link in &entry.links {
        if link.rel.as_deref() == Some("enclosure")
            && link
                .media_type
                .as_deref()
                .is_some_and(|mime| mime.starts_with("image/") || looks_like_image(&link.href))
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

/// media:content often carries no mime type, only a url
fn looks_like_image(url: &str) -> bool {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".webp", ".gif", ".avif"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

fn first_img_src(html: &str) -> Option<String> {
    let doc = Html::parse_fragment(html);
    let sel = Selector::parse("img").ok()?;
    doc.select(&sel)
        .next()
        .and_then(|node| node.value().attr("src"))
        .map(str::to_string)
}

/// summaries arrive as html, sometimes double-escaped by the publisher
/// (`&amp;quot;` inside CDATA). Our own body escapes again, so normalise to
/// plain text - two passes because one only unwraps one level.
fn plain_text(markup: &str) -> String {
    let once = text_nodes(markup);
    let twice = text_nodes(&once);
    twice.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn text_nodes(markup: &str) -> String {
    Html::parse_fragment(markup)
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
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
    fn summary_plain_text_strips_tags_and_decodes_entities() {
        // feed-rs hands over xml-decoded text, so entities here are one level down
        assert_eq!(
            plain_text("&quot;responsible AI&quot; <b>now</b>"),
            "\"responsible AI\" now"
        );
        // publishers that escape inside CDATA send two levels
        assert_eq!(
            plain_text("&amp;quot;responsible AI&amp;quot;"),
            "\"responsible AI\""
        );
        assert_eq!(plain_text("plain words"), "plain words");
    }

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
            Some("abc"),
            "https://papers.internal",
        );
        assert_eq!(item.title, "Dune: Part Two (2024)");
        assert_eq!(item.link, "https://www.themoviedb.org/movie/693134");
        assert_eq!(item.summary, "space sequel");
        assert!(
            item.extra.contains(
                "<iframe width=\"100%\" height=\"315\" src=\"https://www.youtube.com/embed/abc\""
            ),
            "trailer plays inline: {}",
            item.extra
        );
        assert!(
            item.extra
                .contains("<a href=\"https://www.youtube.com/watch?v=abc\">trailer</a>")
        );
        assert!(item.extra.contains(
            "<a href=\"https://papers.internal/add?media=693134&amp;media_type=movie\">add</a>"
        ));
        assert!(
            !item.extra.contains("/ 10"),
            "no rating line when the payload has no voteAverage"
        );
    }

    #[test]
    fn seerr_item_shows_the_rating() {
        let mut rated = media("movie", "Dune", "2024-03-01");
        rated.vote_average = Some(7.94);
        let item = seerr_item(&rated, "Dune", None, "https://p");
        assert!(
            item.extra.contains("<p>7.9 / 10 - movie</p>"),
            "{}",
            item.extra
        );

        let mut unrated = media("movie", "Dune", "2024-03-01");
        unrated.vote_average = Some(0.0);
        let item = seerr_item(&unrated, "Dune", None, "https://p");
        assert!(!item.extra.contains("/ 10"), "zero votes is not a rating");
    }

    fn jellyfin(json: serde_json::Value) -> JellyfinItem {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn jellyfin_item_carries_poster_rating_year_and_trailer() {
        let entry = jellyfin(serde_json::json!({
            "Id": "0f8a",
            "Name": "Dune: Part Two",
            "Type": "Movie",
            "ProductionYear": 2024,
            "Overview": "Paul unites the Fremen.",
            "CommunityRating": 8.2,
            "OfficialRating": "PG-13",
            "RunTimeTicks": 99600000000i64,
            "Genres": ["Sci-Fi", "Adventure"],
            "DateCreated": "2026-11-01T18:04:09.1234567Z",
            "RemoteTrailers": [{"Url": "https://www.youtube.com/watch?v=Way9Dexny3w"}],
            "ProviderIds": {"Tmdb": "693134", "Imdb": "tt15239678"},
        }));
        let item = jellyfin_item(&entry, "http://jellyfin:8096", None);
        assert_eq!(item.title, "Dune: Part Two (2024)");
        assert_eq!(
            item.link, "https://www.themoviedb.org/movie/693134",
            "entry links to a readable page so readers offer reader view"
        );
        assert!(
            item.extra
                .contains("http://jellyfin:8096/web/#/details?id=0f8a"),
            "the app link stays in the body: {}",
            item.extra
        );
        assert_eq!(
            item.image, "http://jellyfin:8096/Items/0f8a/Images/Primary?maxHeight=450",
            "poster is served by jellyfin itself"
        );
        assert!(
            item.extra
                .contains("<p><strong>Movie</strong> | \u{1f345} 8.2 / 10 | PG-13 | 2h 46m | Sci-Fi, Adventure</p>"),
            "meta line reads at a glance: {}",
            item.extra
        );
        assert!(
            item.extra
                .contains("https://www.youtube.com/embed/Way9Dexny3w"),
            "trailer plays inline: {}",
            item.extra
        );
        assert_eq!(item.summary, "Paul unites the Fremen.");
        assert!(
            item.published.is_some(),
            "date created is the publication stamp"
        );
    }

    #[test]
    fn series_entry_names_the_newest_episode() {
        let entry = jellyfin(serde_json::json!({
            "Id": "1",
            "Name": "Dexter: Resurrection",
            "Type": "Series",
            "ProviderIds": {"Tmdb": "1234"},
        }));
        let item = jellyfin_item(
            &entry,
            "http://jellyfin:8096",
            Some("<strong>S02E01</strong> Risky Business \u{00b7} available to watch"),
        );
        assert_eq!(item.link, "https://www.themoviedb.org/tv/1234");
        assert!(
            item.extra
                .contains("<strong>S02E01</strong> Risky Business \u{00b7} available to watch"),
            "{}",
            item.extra
        );
    }

    #[test]
    fn an_item_without_provider_ids_falls_back_to_the_app_link() {
        let entry = jellyfin(serde_json::json!({"Id": "9", "Name": "Home Video", "Type": "Movie"}));
        let item = jellyfin_item(&entry, "http://jellyfin:8096", None);
        assert_eq!(item.link, "http://jellyfin:8096/web/#/details?id=9");
    }

    #[test]
    fn runtime_ticks_read_as_hours_and_minutes() {
        assert_eq!(
            runtime_minutes(65894399999).as_deref(),
            Some("1h 49m"),
            "jellyfin ticks are 100ns"
        );
        assert_eq!(
            runtime_minutes(30 * 60 * 10_000_000).as_deref(),
            Some("30m")
        );
        assert_eq!(runtime_minutes(0), None, "no runtime means no line");
    }

    #[test]
    fn jellyfin_item_without_rating_or_trailer_stays_plain() {
        let entry = jellyfin(serde_json::json!({
            "Id": "1",
            "Name": "Home Video",
            "Type": "Series",
            "DateCreated": "2026-11-01T18:04:09.1234567Z",
        }));
        let item = jellyfin_item(&entry, "http://jellyfin:8096", None);
        assert_eq!(item.title, "Home Video");
        assert!(
            item.extra.starts_with("<p><strong>TV Show</strong></p>"),
            "kind is all it knows: {}",
            item.extra
        );
        assert!(!item.extra.contains("iframe"), "no trailer to play");
        assert!(
            item.extra.contains(">watch</a>"),
            "there is always somewhere to watch it: {}",
            item.extra
        );
    }

    #[test]
    fn youtube_key_reads_the_urls_jellyfin_stores() {
        assert_eq!(
            youtube_key("https://www.youtube.com/watch?v=Way9Dexny3w").as_deref(),
            Some("Way9Dexny3w")
        );
        assert_eq!(
            youtube_key("https://youtu.be/Way9Dexny3w").as_deref(),
            Some("Way9Dexny3w")
        );
        assert_eq!(
            youtube_key("https://www.youtube.com/embed/Way9Dexny3w").as_deref(),
            Some("Way9Dexny3w")
        );
        assert_eq!(
            youtube_key("https://example.com/trailer.mp4"),
            None,
            "only youtube trailers get an inline player"
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
            "<p><a href=\"https://papers.internal/add?media=693134&amp;media_type=tv\">add</a></p>"
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
        assert_eq!(pick_trailer(&videos).as_deref(), Some("real1"));
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
    fn entry_image_accepts_untyped_media_content_by_extension() {
        // selfh.st publishes media:content with only a url and medium="image"
        let entry = parsed_entry(
            r#"<rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/"><channel><title>t</title>
               <item><title>a</title><link>https://x/1</link>
                 <media:content url="https://x/featured-image.png?width=600" medium="image"/>
               </item></channel></rss>"#,
        );
        assert_eq!(
            entry_image(&entry),
            "https://x/featured-image.png?width=600"
        );
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
