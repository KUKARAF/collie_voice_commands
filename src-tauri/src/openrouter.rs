use serde_json::Value;
use thiserror::Error;

const CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const TTS_URL: &str = "https://openrouter.ai/api/v1/audio/speech";

#[derive(Debug, Error)]
pub enum OpenRouterError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("openrouter error: {0}")]
    Api(String),
    #[error("unexpected response shape: {0}")]
    Shape(String),
}

pub type Result<T> = std::result::Result<T, OpenRouterError>;

pub struct OpenRouterClient {
    http: reqwest::Client,
    api_key: String,
}

impl OpenRouterClient {
    pub fn new(api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
        }
    }

    async fn ensure_success(resp: reqwest::Response) -> Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(OpenRouterError::Api(format!("{status}: {body}")))
    }

    async fn chat(&self, body: Value) -> Result<Value> {
        let resp = self
            .http
            .post(CHAT_URL)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;
        let resp = Self::ensure_success(resp).await?;
        Ok(resp.json::<Value>().await?)
    }

    /// Structured-output chat completion: the model's reply is constrained to `schema`
    /// (OpenRouter's `response_format: json_schema` with `strict: true`) and parsed back into
    /// a `Value` for the caller to deserialize into a concrete type.
    pub async fn chat_json(
        &self,
        model: &str,
        system: &str,
        user: &str,
        schema_name: &str,
        schema: Value,
    ) -> Result<Value> {
        let body = serde_json::json!({
            "model": model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": schema_name,
                    "strict": true,
                    "schema": schema,
                }
            }
        });
        let response = self.chat(body).await?;
        let content = extract_message_content(&response)?;
        serde_json::from_str(&content)
            .map_err(|e| OpenRouterError::Shape(format!("content wasn't valid JSON: {e}")))
    }

    /// Plain-text chat completion — used for summarizing pane output, no schema constraint.
    pub async fn chat_text(&self, model: &str, system: &str, user: &str) -> Result<String> {
        let body = serde_json::json!({
            "model": model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
        });
        let response = self.chat(body).await?;
        extract_message_content(&response)
    }

    /// Raw audio bytes from OpenRouter's dedicated TTS endpoint — this is NOT the chat
    /// completions endpoint, and the response body is audio, not JSON.
    pub async fn tts(
        &self,
        model: &str,
        voice: &str,
        input: &str,
        format: &str,
    ) -> Result<Vec<u8>> {
        let resp = self
            .http
            .post(TTS_URL)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": model,
                "input": input,
                "voice": voice,
                "response_format": format,
            }))
            .send()
            .await?;
        let resp = Self::ensure_success(resp).await?;
        Ok(resp.bytes().await?.to_vec())
    }
}

fn extract_message_content(response: &Value) -> Result<String> {
    response["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            OpenRouterError::Shape(format!(
                "no choices[0].message.content string in response: {response}"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_message_content_from_chat_response() {
        let fixture = serde_json::json!({
            "choices": [{ "message": { "content": "hello there" } }]
        });
        assert_eq!(extract_message_content(&fixture).unwrap(), "hello there");
    }

    #[test]
    fn missing_content_is_a_shape_error() {
        let fixture = serde_json::json!({ "choices": [] });
        assert!(extract_message_content(&fixture).is_err());
    }
}
