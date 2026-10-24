use chrono::{DateTime, SecondsFormat, Utc};

use crate::store::Item;

pub const FEED_TITLE: &str = "papers";

/// one atom entry per article - readers see every item on its own
pub fn atom(feed_id: &str, feed_url: &str, items: &[Item], updated: DateTime<Utc>) -> String {
    let mut out = String::with_capacity(8192);
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<feed xmlns=\"http://www.w3.org/2005/Atom\">\n");
    out.push_str(&format!("  <title>{FEED_TITLE} - {feed_id}</title>\n"));
    out.push_str(&format!("  <id>tag:papers,2026:{feed_id}</id>\n"));
    out.push_str(&format!("  <link href=\"{feed_url}\" rel=\"self\"/>\n"));
    out.push_str("  <author><name>papers</name></author>\n");
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
            "    <link href=\"{}\" rel=\"alternate\"/>\n",
            escape(&item.link)
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

/// per-article body: summary, scoring reason, and source-specific anchors
fn entry_body(item: &Item, reason: &str) -> String {
    let mut body = String::new();
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
