//! Async chat-completion client for the cockpit. Posts a turn to the live
//! llama-server (`/v1/chat/completions`), returns the assistant's content.
//!
//! Non-streaming for the first cut — the request goes out, a single reply
//! comes back, gets pushed into the chat pane. Streaming + tool-call display
//! are follow-up chunks.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    temperature: f32,
    max_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

/// One pending chat exchange — sender half lives in the spawned task,
/// receiver lives on the main UI loop and is polled non-blockingly per tick.
pub type ChatRx = mpsc::Receiver<Result<String, String>>;

pub fn spawn_chat_request(
    client: Arc<reqwest::Client>,
    base_url: String,
    model: String,
    history: Vec<ChatMessage>,
) -> ChatRx {
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let result = do_request(&client, &base_url, &model, &history)
            .await
            .map_err(|e| format!("{e}"));
        let _ = tx.send(result).await;
    });
    rx
}

async fn do_request(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    history: &[ChatMessage],
) -> Result<String> {
    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let body = ChatRequest {
        model,
        messages: history,
        temperature: 0.7,
        max_tokens: 1024,
    };
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("POST failed")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        // char-aware truncation so we don't panic on multibyte UTF-8 boundaries
        let snippet: String = text.chars().take(300).collect();
        return Err(anyhow!("server returned {status}: {}", snippet));
    }
    let parsed: ChatResponse = resp.json().await.context("response not valid JSON")?;
    let content = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("response had no choices"))?
        .message
        .content;
    Ok(content)
}

/// Build a sharable reqwest client with a moderate timeout. Re-use across
/// requests; `reqwest::Client` is internally an Arc'd connection pool.
pub fn build_client() -> Arc<reqwest::Client> {
    Arc::new(
        reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("build reqwest client"),
    )
}
