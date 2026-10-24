use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::Rank;
use crate::store::Item;

pub const DEFAULT_PROMPT: &str = "You filter a personal news feed for a homelab and infrastructure engineer. \
Given items as JSON, score each 0 to 10 for whether it is worth reading: \
score high for self-hosting, Kubernetes, Rust, networking, storage, hardware, and practical \
tutorials; score low for AI hype, funding news, listicles, and opinion pieces without substance. \
Return JSON only: {\"items\":[{\"id\":<int>,\"score\":<int>,\"reason\":\"<under 12 words>\"}]}";

pub fn prefilter(items: Vec<Item>, rank: &Rank) -> Vec<Item> {
    items
        .into_iter()
        .filter(|item| {
            let haystack = format!("{} {}", item.title, item.summary).to_lowercase();
            if rank
                .exclude
                .iter()
                .any(|word| haystack.contains(&word.to_lowercase()))
            {
                return false;
            }
            rank.require_any.is_empty()
                || rank
                    .require_any
                    .iter()
                    .any(|word| haystack.contains(&word.to_lowercase()))
        })
        .collect()
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: String,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatBody,
}

#[derive(Deserialize)]
struct ChatBody {
    content: String,
}

#[derive(Deserialize)]
struct Verdicts {
    items: Vec<Verdict>,
}

#[derive(Deserialize)]
pub struct Verdict {
    pub id: i64,
    pub score: i64,
    pub reason: String,
}

pub async fn score(
    client: &Client,
    base_url: &str,
    model: &str,
    key: &str,
    prompt: &str,
    items: &[Item],
) -> Result<Vec<Verdict>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let payload: Vec<String> = items
        .iter()
        .map(|item| {
            format!(
                "{{\"id\":{},\"source\":{:?},\"title\":{:?},\"summary\":{:?}}}",
                item.id,
                item.source,
                item.title,
                item.summary.chars().take(300).collect::<String>()
            )
        })
        .collect();

    let request = ChatRequest {
        model,
        messages: vec![
            Message {
                role: "system",
                content: prompt.to_string(),
            },
            Message {
                role: "user",
                content: format!("[{}]", payload.join(",")),
            },
        ],
        temperature: 0.2,
        response_format: ResponseFormat {
            kind: "json_object",
        },
    };

    let response = client
        .post(format!(
            "{}/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(key)
        .json(&request)
        .send()
        .await
        .context("calling the model")?
        .error_for_status()?
        .json::<ChatResponse>()
        .await
        .context("decoding the model response")?;

    let content = response
        .choices
        .first()
        .map(|choice| choice.message.content.clone())
        .unwrap_or_default();
    let verdicts: Verdicts = serde_json::from_str(content.trim())
        .with_context(|| format!("model did not return the agreed JSON: {content}"))?;
    Ok(verdicts.items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, summary: &str) -> Item {
        Item {
            id: 1,
            source: "s".into(),
            title: title.into(),
            link: "https://x/1".into(),
            summary: summary.into(),
            weight: 5,
            reason: None,
            feed: "news".into(),
            extra: String::new(),
        }
    }

    #[test]
    fn prefilter_honours_exclude_and_require_any() {
        let rank = Rank {
            exclude: vec!["giveaway".into()],
            require_any: vec!["kubernetes".into()],
            prompt: None,
        };
        let kept = prefilter(
            vec![
                item("Free giveaway", "kubernetes stuff"),
                item("Kubernetes 1.31", "release notes"),
                item("Baking bread", "flour"),
            ],
            &rank,
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].title, "Kubernetes 1.31");
    }

    #[test]
    fn prefilter_without_require_any_keeps_everything_but_excluded() {
        let rank = Rank {
            exclude: vec!["hype".into()],
            require_any: vec![],
            prompt: None,
        };
        let kept = prefilter(vec![item("AI hype", ""), item("Wireguard mesh", "")], &rank);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].title, "Wireguard mesh");
    }
}
