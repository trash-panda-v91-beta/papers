use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

pub struct Store {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct NewItem {
    pub source: String,
    pub title: String,
    pub link: String,
    pub summary: String,
    pub published: Option<DateTime<Utc>>,
    pub weight: i64,
    pub extra: String,
    pub image: String,
    pub feed: String,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: i64,
    pub source: String,
    pub title: String,
    pub link: String,
    pub summary: String,
    pub weight: i64,
    pub reason: Option<String>,
    pub extra: String,
    /// poster or thumbnail url, empty when the source has none
    pub image: String,
    /// when the item was published to a feed; defaults to epoch if unshown
    pub shown_at: DateTime<Utc>,
}

impl Item {
    pub fn with_shown_at(mut self, at: DateTime<Utc>) -> Self {
        self.shown_at = at;
        self
    }
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening db {path}"))?;
        conn.execute_batch(
            "create table if not exists items (
                 id integer primary key,
                 url_hash text not null unique,
                 source text not null,
                 title text not null,
                 link text not null,
                 summary text not null default '',
                 published text,
                 fetched_at text not null,
                 weight integer not null,
                 score integer,
                 reason text,
                 shown integer not null default 0,
                 extra text not null default '',
                 image text not null default ''
             );
             create table if not exists config (
                 id integer primary key check (id = 1),
                 sources text not null,
                 updated_at text not null
             );",
        )?;
        Self::migrate(&conn)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("setting busy timeout")?;
        Ok(Self { conn })
    }

    /// older databases: items without feed/extra/shown_at columns - widen
    /// them in place, carrying existing rows into the news feed
    fn migrate(conn: &Connection) -> Result<()> {
        let has_column = |table: &str, column: &str| -> Result<bool> {
            let mut stmt = conn.prepare(&format!("pragma table_info({table})"))?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                if row.get::<_, String>(1)? == column {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        if !has_column("items", "shown_at")? {
            conn.execute_batch("alter table items add column shown_at text;")?;
        }
        if !has_column("items", "extra")? {
            conn.execute_batch("alter table items add column extra text not null default '';")?;
        }
        if !has_column("items", "image")? {
            conn.execute_batch("alter table items add column image text not null default '';")?;
        }
        if !has_column("items", "feed")? {
            conn.execute_batch(
                "alter table items add column feed text not null default 'news';
                 update items set feed = 'news';",
            )?;
        }
        Ok(())
    }

    /// live config overrides the seeded file; stored once and then owned by
    /// the admin API, so git only ever provides the bootstrap
    pub fn load_config(&self) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("select sources from config where id = 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn save_config(&self, sources: &str) -> Result<()> {
        self.conn.execute(
            "insert into config (id, sources, updated_at) values (1, ?1, ?2)
             on conflict(id) do update set sources = excluded.sources,
                 updated_at = excluded.updated_at",
            params![sources, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn config_updated_at(&self) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("select updated_at from config where id = 1")?;
        let mut rows = stmt.query([])?;
        Ok(rows.next()?.map(|row| row.get(0).unwrap_or_default()))
    }

    pub fn insert(&self, item: &NewItem) -> Result<bool> {
        // sha2 0.11 output is a hybrid-array Array<u8, U32>, which has no LowerHex
        let hash = Sha256::digest(item.link.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let changed = self.conn.execute(
            "insert or ignore into items
                 (url_hash, source, title, link, summary, published, fetched_at, weight, extra, feed, image)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                hash,
                item.source,
                item.title,
                item.link,
                item.summary,
                item.published.map(|d| d.to_rfc3339()),
                Utc::now().to_rfc3339(),
                item.weight,
                item.extra,
                item.feed,
                item.image,
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn unseen(&self, limit: usize) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare(
            "select id, source, title, link, summary, weight, reason, extra, image
             from items where shown = 0 and score is null
             order by weight desc, fetched_at desc limit ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(Item {
                id: row.get(0)?,
                source: row.get(1)?,
                title: row.get(2)?,
                link: row.get(3)?,
                summary: row.get(4)?,
                weight: row.get(5)?,
                reason: row.get(6)?,
                extra: row.get(7)?,
                image: row.get(8)?,
                shown_at: Utc::now(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_score(&self, id: i64, score: i64, reason: &str) -> Result<()> {
        self.conn.execute(
            "update items set score = ?1, reason = ?2 where id = ?3",
            params![score, reason, id],
        )?;
        Ok(())
    }

    /// feeds that have scored, unpublished items waiting for a digest
    pub fn pending_feeds(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "select distinct feed from items where shown = 0 and score >= 5 order by feed",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn top(&self, feed: &str, keep: usize) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare(
            "select id, source, title, link, summary, weight, reason, extra, image
             from items where shown = 0 and score >= 5 and feed = ?1
             order by score desc, weight desc, fetched_at desc limit ?2",
        )?;
        let rows = stmt.query_map(params![feed, keep as i64], |row| {
            Ok(Item {
                id: row.get(0)?,
                source: row.get(1)?,
                title: row.get(2)?,
                link: row.get(3)?,
                summary: row.get(4)?,
                weight: row.get(5)?,
                reason: row.get(6)?,
                extra: row.get(7)?,
                image: row.get(8)?,
                shown_at: Utc::now(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn mark_shown(&self, ids: &[i64]) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        for id in ids {
            self.conn.execute(
                "update items set shown = 1, shown_at = ?1 where id = ?2",
                params![now, id],
            )?;
        }
        Ok(())
    }

    /// published items for a feed, newest first - one atom entry per item
    pub fn feed_entries(&self, feed: &str, keep: usize) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare(
            "select id, source, title, link, summary, weight, reason, extra, image,
                    coalesce(shown_at, fetched_at) as stamp
             from items where shown = 1 and feed = ?1
             order by stamp desc limit ?2",
        )?;
        let rows = stmt.query_map(params![feed, keep as i64], |row| {
            Ok((
                Item {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    summary: row.get(4)?,
                    weight: row.get(5)?,
                    reason: row.get(6)?,
                    extra: row.get(7)?,
                    image: row.get(8)?,
                    shown_at: Utc::now(),
                },
                row.get::<_, String>(9)?,
            ))
        })?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(item, stamp)| {
                item.with_shown_at(
                    DateTime::parse_from_rfc3339(&stamp)
                        .map(|t| t.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                )
            })
            .collect())
    }

    pub fn feeds(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("select distinct feed from items where shown = 1 order by feed")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(link: &str) -> NewItem {
        NewItem {
            source: "test".into(),
            title: "t".into(),
            link: link.into(),
            summary: String::new(),
            published: None,
            weight: 5,
            extra: String::new(),
            image: String::new(),
            feed: "news".into(),
        }
    }

    #[test]
    fn dedupes_by_link_and_respects_score_floor() {
        let store = Store::open(":memory:").unwrap();
        assert!(store.insert(&item("https://a/1")).unwrap());
        assert!(
            !store.insert(&item("https://a/1")).unwrap(),
            "same link must not insert twice"
        );

        let unseen = store.unseen(10).unwrap();
        assert_eq!(unseen.len(), 1);

        store.set_score(unseen[0].id, 3, "meh").unwrap();
        assert!(
            store.top("news", 10).unwrap().is_empty(),
            "score below 5 is not shown"
        );

        assert!(store.insert(&item("https://a/2")).unwrap());
        let next = store.unseen(10).unwrap();
        store.set_score(next[0].id, 9, "good").unwrap();
        let top = store.top("news", 10).unwrap();
        assert_eq!(top.len(), 1);
        store.mark_shown(&[top[0].id]).unwrap();
        assert!(
            store.top("news", 10).unwrap().is_empty(),
            "shown items drop out"
        );
    }

    #[test]
    fn feed_entries_come_from_shown_items_per_feed() {
        let store = Store::open(":memory:").unwrap();
        assert!(store.insert(&item("https://a/1")).unwrap());
        assert!(store.insert(&item("https://a/2")).unwrap());
        let unseen = store.unseen(10).unwrap();
        store.set_score(unseen[0].id, 9, "good").unwrap();
        store.mark_shown(&[unseen[0].id]).unwrap();

        assert_eq!(store.feeds().unwrap(), vec!["news"]);
        let entries = store.feed_entries("news", 30).unwrap();
        assert_eq!(entries.len(), 1, "only published items are feed entries");
    }

    #[test]
    fn migration_carries_old_schema_into_news_feed() {
        // build a v0.2-style database by hand, then let Store::open migrate it
        let path = std::env::temp_dir().join(format!("papers-mig-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table items (
                 id integer primary key,
                 url_hash text not null unique,
                 source text not null,
                 title text not null,
                 link text not null,
                 summary text not null default '',
                 published text,
                 fetched_at text not null,
                 weight integer not null,
                 score integer,
                 reason text,
                 shown integer not null default 0
             );
             insert into items (url_hash, source, title, link, fetched_at, weight)
                 values ('h', 'lwn', 't', 'https://a/1', '2026-10-20', 5);",
        )
        .unwrap();
        drop(conn);

        let store = Store::open(path.to_str().unwrap()).unwrap();
        // migrated rows survive and new links still dedupe
        assert_eq!(store.unseen(10).unwrap().len(), 1);
        assert!(store.insert(&item("https://a/2")).unwrap());
        assert_eq!(store.unseen(10).unwrap().len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn extra_round_trips() {
        let store = Store::open(":memory:").unwrap();
        let mut new = item("https://a/1");
        new.extra = "<a href=\"https://x\">trailer</a>".into();
        store.insert(&new).unwrap();
        assert_eq!(store.unseen(10).unwrap()[0].extra, new.extra);
    }

    #[test]
    fn live_config_seeds_once_then_overrides() {
        let store = Store::open(":memory:").unwrap();
        assert!(store.load_config().unwrap().is_none());
        store.save_config("keep_per_day = 3").unwrap();
        assert_eq!(store.load_config().unwrap().unwrap(), "keep_per_day = 3");
        assert!(store.config_updated_at().unwrap().is_some());
        store.save_config("keep_per_day = 5").unwrap();
        assert_eq!(store.load_config().unwrap().unwrap(), "keep_per_day = 5");
    }
}
