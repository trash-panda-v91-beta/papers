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
                 extra text not null default ''
             );
             create table if not exists digests (
                 day text primary key,
                 title text not null,
                 body text not null,
                 created_at text not null
             );",
        )?;
        Ok(Self { conn })
    }

    pub fn insert(&self, item: &NewItem) -> Result<bool> {
        // sha2 0.11 output is a hybrid-array Array<u8, U32>, which has no LowerHex
        let hash = Sha256::digest(item.link.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let changed = self.conn.execute(
            "insert or ignore into items
                 (url_hash, source, title, link, summary, published, fetched_at, weight, extra)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn unseen(&self, limit: usize) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare(
            "select id, source, title, link, summary, weight, reason, extra
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

    pub fn top(&self, keep: usize) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare(
            "select id, source, title, link, summary, weight, reason, extra
             from items where shown = 0 and score >= 5
             order by score desc, weight desc, fetched_at desc limit ?1",
        )?;
        let rows = stmt.query_map([keep as i64], |row| {
            Ok(Item {
                id: row.get(0)?,
                source: row.get(1)?,
                title: row.get(2)?,
                link: row.get(3)?,
                summary: row.get(4)?,
                weight: row.get(5)?,
                reason: row.get(6)?,
                extra: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn mark_shown(&self, ids: &[i64]) -> Result<()> {
        for id in ids {
            self.conn
                .execute("update items set shown = 1 where id = ?1", params![id])?;
        }
        Ok(())
    }

    pub fn save_digest(&self, day: &str, title: &str, body: &str) -> Result<()> {
        self.conn.execute(
            "insert into digests (day, title, body, created_at) values (?1, ?2, ?3, ?4)
             on conflict(day) do update set title = excluded.title, body = excluded.body",
            params![day, title, body, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn digests(&self, keep: usize) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("select day, title, body from digests order by day desc limit ?1")?;
        let rows = stmt.query_map([keep as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
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
            store.top(10).unwrap().is_empty(),
            "score below 5 is not shown"
        );

        assert!(store.insert(&item("https://a/2")).unwrap());
        let next = store.unseen(10).unwrap();
        store.set_score(next[0].id, 9, "good").unwrap();
        let top = store.top(10).unwrap();
        assert_eq!(top.len(), 1);
        store.mark_shown(&[top[0].id]).unwrap();
        assert!(store.top(10).unwrap().is_empty(), "shown items drop out");

        store.save_digest("2026-10-20", "day", "<p>hi</p>").unwrap();
        assert_eq!(store.digests(5).unwrap().len(), 1);
    }

    #[test]
    fn extra_round_trips() {
        let store = Store::open(":memory:").unwrap();
        let mut new = item("https://a/1");
        new.extra = "<a href=\"https://x\">trailer</a>".into();
        store.insert(&new).unwrap();
        assert_eq!(store.unseen(10).unwrap()[0].extra, new.extra);
    }
}
