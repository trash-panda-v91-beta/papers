use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    /// items published per run, so a backlog cannot dump the whole pool at once
    #[serde(default = "default_max_per_run")]
    pub max_per_run: usize,
    /// sources hand over their whole back catalogue; only recent items count.
    /// 0 disables the cutoff.
    #[serde(default = "default_max_age_days")]
    pub max_age_days: i64,
    #[serde(default)]
    pub rank: Rank,
    pub sources: Vec<Source>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Rank {
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub require_any: Vec<String>,
    /// items scoring below this are dropped. 0 publishes everything, raise it
    /// to tighten the paper as the noise shows up.
    #[serde(default)]
    pub min_score: i64,
    /// overrides the built-in scoring prompt; editable live via /admin/config
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Source {
    pub name: String,
    #[serde(flatten)]
    pub kind: SourceKind,
    #[serde(default)]
    pub max_per_fetch: Option<usize>,
    /// prefixes entry titles, e.g. "New in Seerr"
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SourceKind {
    Feed {
        url: String,
    },
    Html {
        url: String,
        item: String,
        title: String,
        link: String,
        #[serde(default)]
        summary: Option<String>,
    },
    Seerr {
        url: String,
    },
}

fn default_max_per_run() -> usize {
    8
}

fn default_max_age_days() -> i64 {
    7
}

impl Config {
    pub fn parse(raw: &str) -> Result<Self> {
        let config: Config = toml::from_str(raw).context("parsing sources config")?;
        if config.sources.is_empty() {
            anyhow::bail!("config has no sources");
        }
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&raw)
    }

    pub fn path() -> PathBuf {
        std::env::var("PAPERS_CONFIG")
            .map_or_else(|_| PathBuf::from("/data/sources.toml"), PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_source_kinds() {
        let raw = r#"
max_per_run = 8
[rank]
exclude = ["giveaway"]
require_any = []

[[sources]]
name = "hn"
kind = "feed"
url = "https://hnrss.org/frontpage"

[[sources]]
name = "lwn"
kind = "html"
url = "https://lwn.net/"
item = "div.ArticleTitle a"
title = "a"
link = "a"

[[sources]]
name = "seerr"
kind = "seerr"
url = "http://seerr:80"
"#;
        let dir = std::env::temp_dir().join("ng-feeds-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sources.toml");
        std::fs::write(&path, raw).unwrap();

        let config = Config::load(&path).unwrap();
        assert_eq!(config.max_per_run, 8);
        assert_eq!(config.max_age_days, default_max_age_days());
        assert_eq!(config.sources.len(), 3);
        assert_eq!(config.rank.exclude, vec!["giveaway".to_string()]);
        assert_eq!(
            config.rank.min_score, 0,
            "everything publishes until the bar is raised"
        );
        assert!(matches!(
            config.sources[2].kind,
            SourceKind::Seerr { ref url } if url == "http://seerr:80"
        ));
    }

    #[test]
    fn parses_rank_prompt_override() {
        let config = Config::parse(
            "[rank]\nprompt = \"score for cats\"\n\n[[sources]]\nname = \"hn\"\nkind = \"feed\"\nurl = \"https://x\"\n",
        )
        .unwrap();
        assert_eq!(config.rank.prompt.as_deref(), Some("score for cats"));
        let config =
            Config::parse("[[sources]]\nname = \"hn\"\nkind = \"feed\"\nurl = \"https://x\"\n")
                .unwrap();
        assert_eq!(config.rank.prompt, None);
    }
}
