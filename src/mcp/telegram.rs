use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct TelegramClient {
    http: Client,
    token: String,
    chat_id: String,
    base_url: String,
}

impl TelegramClient {
    pub(crate) fn from_env() -> Result<Self> {
        let token = std::env::var("TELEGRAM_BOT_TOKEN").context("не задан TELEGRAM_BOT_TOKEN")?;
        let chat_id = std::env::var("TELEGRAM_CHAT_ID").context("не задан TELEGRAM_CHAT_ID")?;
        anyhow::ensure!(
            !token.trim().is_empty() && !chat_id.trim().is_empty(),
            "настройки Telegram пусты"
        );
        Ok(Self {
            http: Client::builder()
                .user_agent("fox-llm-telegram/0.1")
                .build()?,
            token,
            chat_id,
            base_url: "https://api.telegram.org".into(),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(base_url: String) -> Self {
        Self {
            http: Client::new(),
            token: "test-token".into(),
            chat_id: "test-chat".into(),
            base_url,
        }
    }

    pub(crate) async fn send(&self, text: &str) -> Result<TelegramOutcome> {
        let url = format!("{}/bot{}/sendMessage", self.base_url, self.token);
        let response = match self
            .http
            .post(url)
            .json(&serde_json::json!({"chat_id": self.chat_id, "text": text}))
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return Ok(TelegramOutcome::Unknown("ошибка сети Telegram".into())),
        };
        let status = response.status();
        let body: Value = response.json().await.unwrap_or_default();
        if status.is_success() && body.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(TelegramOutcome::Delivered(
                body.pointer("/result/message_id").and_then(Value::as_i64),
            ))
        } else {
            Ok(TelegramOutcome::Failed(
                body.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("Telegram отклонил сообщение")
                    .to_owned(),
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TelegramOutcome {
    Delivered(Option<i64>),
    Failed(String),
    Unknown(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Json, Router};
    use serde_json::json;
    use tokio::net::TcpListener;

    async fn with_response(
        status: axum::http::StatusCode,
        body: Value,
    ) -> Option<(TelegramClient, tokio::task::JoinHandle<()>)> {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("loopback Telegram test skipped: {error}");
                return None;
            }
        };
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/bottest-token/sendMessage",
            post(move || {
                let body = body.clone();
                async move { (status, Json(body)) }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Some((TelegramClient::for_test(format!("http://{address}")), task))
    }

    #[tokio::test]
    async fn telegram_classifies_delivered_rejected_and_unknown() {
        let Some((client, task)) = with_response(
            axum::http::StatusCode::OK,
            json!({"ok": true, "result": {"message_id": 42}}),
        )
        .await
        else {
            return;
        };
        assert_eq!(
            client.send("hello").await.unwrap(),
            TelegramOutcome::Delivered(Some(42))
        );
        task.abort();

        let Some((client, task)) = with_response(
            axum::http::StatusCode::BAD_REQUEST,
            json!({"ok": false, "description": "rejected"}),
        )
        .await
        else {
            return;
        };
        assert_eq!(
            client.send("hello").await.unwrap(),
            TelegramOutcome::Failed("rejected".into())
        );
        task.abort();

        let client = TelegramClient::for_test("http://127.0.0.1:9".into());
        assert!(matches!(
            client.send("hello").await.unwrap(),
            TelegramOutcome::Unknown(_)
        ));
    }
}
