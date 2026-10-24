use chrono::{DateTime, SecondsFormat, Utc};

use crate::store::Item;

pub const FEED_TITLE: &str = "papers";
pub const FEED_SUBTITLE: &str = "what is worth reading today";

/// one atom entry per article - readers see every item on its own
pub fn atom(feed_id: &str, feed_url: &str, items: &[Item], updated: DateTime<Utc>) -> String {
    let mut out = String::with_capacity(8192);
    let base = feed_base(feed_url);
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<feed xmlns=\"http://www.w3.org/2005/Atom\" xmlns:media=\"http://search.yahoo.com/mrss/\">\n");
    out.push_str(&format!("  <title>{FEED_TITLE} - {feed_id}</title>\n"));
    out.push_str(&format!("  <subtitle>{FEED_SUBTITLE}</subtitle>\n"));
    out.push_str(&format!("  <id>tag:papers,2026:{feed_id}</id>\n"));
    out.push_str(&format!("  <link href=\"{feed_url}\" rel=\"self\"/>\n"));
    out.push_str(&format!("  <link href=\"{base}\" rel=\"alternate\"/>\n"));
    out.push_str(&format!("  <icon>{base}/icon.png</icon>\n"));
    out.push_str(&format!("  <logo>{base}/icon.png</logo>\n"));
    out.push_str("  <author><name>papers</name></author>\n");
    out.push_str(
        "  <generator uri=\"https://github.com/trash-panda-v91-beta/papers\">papers</generator>\n",
    );
    out.push_str(&format!("  <updated>{}</updated>\n", stamp(updated)));
    for item in items {
        let reason = item.reason.as_deref().unwrap_or("");
        out.push_str("  <entry>\n");
        out.push_str(&format!("    <title>{}</title>\n", escape(&item.title)));
        out.push_str(&format!(
            "    <id>tag:papers,2026:{feed_id}:{}</id>\n",
            item.id
        ));
        out.push_str(&format!(
            "    <updated>{}</updated>\n",
            stamp(item.shown_at)
        ));
        out.push_str(&format!(
            "    <link href=\"{}\" rel=\"alternate\"/>",
            escape(&item.link)
        ));
        out.push('\n');
        if !item.image.is_empty() {
            out.push_str(&format!(
                "    <link href=\"{}\" rel=\"enclosure\" type=\"image/jpeg\" length=\"0\"/>\n",
                escape(&item.image)
            ));
            out.push_str(&format!(
                "    <media:thumbnail url=\"{}\"/>\n",
                escape(&item.image)
            ));
        }
        out.push_str(&format!(
            "    <author><name>{}</name></author>\n",
            escape(&item.source)
        ));
        out.push_str(&format!(
            "    <category term=\"{}\"/>\n",
            escape(&item.source)
        ));
        out.push_str(&format!(
            "    <content type=\"html\">{}</content>\n",
            entry_body(item, reason)
        ));
        out.push_str("  </entry>\n");
    }
    out.push_str("</feed>\n");
    out
}

/// the feed file's directory doubles as the public base for the icon
fn feed_base(feed_url: &str) -> String {
    match feed_url.rsplit_once('/') {
        Some((base, _)) => base.to_string(),
        None => feed_url.to_string(),
    }
}

/// per-article body: image, summary, scoring reason, and source-specific anchors
fn entry_body(item: &Item, reason: &str) -> String {
    let mut body = String::new();
    if !item.image.is_empty() {
        body.push_str(&format!(
            "<p><img src=\"{}\" alt=\"{}\"/></p>",
            escape(&item.image),
            escape(&item.title)
        ));
    }
    if !item.summary.is_empty() {
        body.push_str(&format!("<p>{}</p>", escape(&item.summary)));
    }
    if !item.extra.is_empty() {
        body.push_str(&format!("<p>{}</p>", item.extra));
    }
    body.push_str(&format!(
        "<p><em>{}</em> <small>{}</small></p>",
        escape(reason),
        escape(&item.source)
    ));
    body
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str) -> Item {
        Item {
            id: 1,
            source: "lwn".into(),
            title: title.into(),
            link: "https://lwn.net/1?a=1&b=2".into(),
            summary: "a summary".into(),
            weight: 5,
            reason: Some("kernel change".into()),
            extra: String::new(),
            image: String::new(),
            shown_at: Utc::now(),
        }
    }

    #[test]
    fn renders_one_entry_per_article_that_parses_back() {
        let xml = atom(
            "news",
            "https://papers.example/news.xml",
            &[item("Rust 2.0 & friends"), item("Second post")],
            Utc::now(),
        );
        let feed = feed_rs::parser::parse(xml.as_bytes()).expect("atom must parse");
        assert_eq!(
            feed.title.map(|t| t.content).as_deref(),
            Some("papers - news")
        );
        assert_eq!(feed.entries.len(), 2, "each article is its own entry");

        let entry = &feed.entries[0];
        assert_eq!(entry.title.as_ref().unwrap().content, "Rust 2.0 & friends");
        assert!(
            xml.contains("tag:papers,2026:news:1"),
            "entry ids are stable per item"
        );
        assert!(
            xml.contains("a=1&amp;b=2"),
            "href ampersand must be xml-escaped in the raw feed: {xml}"
        );
        let content = entry
            .content
            .as_ref()
            .unwrap()
            .body
            .as_deref()
            .unwrap_or_default();
        assert!(content.contains("a summary"), "summary in the body");
        assert!(content.contains("kernel change"), "reason in the body");
    }

    #[test]
    fn feed_carries_icon_and_rich_entries() {
        let mut rich = item("Dune: Part Two (2024)");
        rich.source = "seerr".into();
        rich.image = "https://image.tmdb.org/t/p/w500/poster.jpg".into();
        let xml = atom(
            "media",
            "https://papers.example/media.xml",
            &[rich],
            Utc::now(),
        );
        assert!(
            xml.contains("<icon>https://papers.example/icon.png</icon>"),
            "readers need a feed icon: {xml}"
        );
        assert!(xml.contains("<logo>https://papers.example/icon.png</logo>"));
        assert!(xml.contains("xmlns:media=\"http://search.yahoo.com/mrss/\""));
        assert!(
            xml.contains("<media:thumbnail url=\"https://image.tmdb.org/t/p/w500/poster.jpg\"/>")
        );
        assert!(xml.contains("rel=\"enclosure\""));
        assert!(xml.contains("<img src=\"https://image.tmdb.org/t/p/w500/poster.jpg\""));
        assert!(xml.contains("<author><name>seerr</name></author>"));
        assert!(xml.contains("<category term=\"seerr\"/>"));

        let feed = feed_rs::parser::parse(xml.as_bytes()).expect("rich atom must parse");
        let entry = &feed.entries[0];
        assert!(
            entry
                .links
                .iter()
                .any(|l| l.rel.as_deref() == Some("enclosure")),
            "poster is exposed as an enclosure link too"
        );
    }

    #[test]
    fn entry_keeps_extra_anchors_verbatim() {
        let mut with_extra = item("Dune: Part Two (2024)");
        with_extra.extra =
            "<a href=\"https://x/watch?v=a&amp;b\">trailer</a> - <a href=\"https://p/add\">add</a>"
                .into();
        let xml = atom(
            "media",
            "https://papers.example/media.xml",
            &[with_extra],
            Utc::now(),
        );
        assert!(xml.contains(
            "<a href=\"https://x/watch?v=a&amp;b\">trailer</a> - <a href=\"https://p/add\">add</a>"
        ));
    }
}
