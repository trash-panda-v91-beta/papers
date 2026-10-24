use chrono::{DateTime, SecondsFormat, Utc};

use crate::store::Item;

pub const FEED_TITLE: &str = "papers";

pub fn atom(
    feed_id: &str,
    feed_url: &str,
    digests: &[(String, String, String)],
    updated: DateTime<Utc>,
) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<feed xmlns=\"http://www.w3.org/2005/Atom\">\n");
    out.push_str(&format!("  <title>{FEED_TITLE} - {feed_id}</title>\n"));
    out.push_str(&format!("  <id>tag:papers,2026:{feed_id}</id>\n"));
    out.push_str(&format!("  <link href=\"{feed_url}\" rel=\"self\"/>\n"));
    out.push_str("  <author><name>papers</name></author>\n");
    out.push_str(&format!("  <updated>{}</updated>\n", stamp(updated)));
    for (day, title, body) in digests {
        out.push_str("  <entry>\n");
        out.push_str(&format!("    <title>{}</title>\n", escape(title)));
        out.push_str(&format!("    <id>tag:papers,{day}:daily</id>\n"));
        out.push_str(&format!(
            "    <updated>{}T06:00:00Z</updated>\n",
            escape(day)
        ));
        out.push_str(&format!(
            "    <link href=\"{feed_url}#{day}\" rel=\"alternate\"/>\n"
        ));
        out.push_str(&format!(
            "    <content type=\"html\">{}</content>\n",
            escape(body)
        ));
        out.push_str("  </entry>\n");
    }
    out.push_str("</feed>\n");
    out
}

pub fn paper_entry(day: &str, feed: &str, items: &[Item]) -> (String, String) {
    let title = match feed {
        "news" => format!("{day} - {} worth reading", items.len()),
        "media" => format!("{day} - {} to watch", items.len()),
        other => format!("{day} - {} {other} picks", items.len()),
    };
    let mut body = String::from("<ul>");
    for item in items {
        let reason = item.reason.as_deref().unwrap_or("");
        body.push_str(&format!(
            "<li><a href=\"{}\">{}</a> {} <em>{}</em> <small>{}</small></li>",
            escape(&item.link),
            escape(&item.title),
            item.extra,
            escape(reason),
            escape(&item.source)
        ));
    }
    body.push_str("</ul>");
    (title, body)
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
            summary: String::new(),
            weight: 5,
            reason: Some("kernel change".into()),
            extra: String::new(),
        }
    }

    #[test]
    fn renders_valid_atom_that_parses_back() {
        let (title, body) = paper_entry("2026-10-20", "news", &[item("Rust 2.0 & friends")]);
        let xml = atom(
            "news",
            "https://papers.example/news.xml",
            &[("2026-10-20".to_string(), title, body)],
            Utc::now(),
        );
        let feed = feed_rs::parser::parse(xml.as_bytes()).expect("atom must parse");
        assert_eq!(
            feed.title.map(|t| t.content).as_deref(),
            Some("papers - news")
        );
        assert_eq!(feed.entries.len(), 1);
        let entry = &feed.entries[0];
        assert!(entry.title.as_ref().unwrap().content.contains("2026-10-20"));
        let content = entry
            .content
            .as_ref()
            .unwrap()
            .body
            .as_deref()
            .unwrap_or_default();
        assert!(
            content.contains("a=1&amp;b=2"),
            "ampersand must be escaped: {content}"
        );
        assert!(content.contains("Rust 2.0 &amp; friends"));
    }

    #[test]
    fn entry_keeps_extra_anchors_verbatim() {
        let mut with_extra = item("Dune: Part Two (2024)");
        with_extra.extra =
            "<a href=\"https://x/watch?v=a&amp;b\">trailer</a> - <a href=\"https://p/add\">add</a>"
                .into();
        let (_, body) = paper_entry("2026-10-20", "news", &[with_extra]);
        assert!(body.contains(
            "<a href=\"https://x/watch?v=a&amp;b\">trailer</a> - <a href=\"https://p/add\">add</a>"
        ));
    }
}
