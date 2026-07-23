use serde_json::Value;
use thiserror::Error;

const CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const TTS_URL: &str = "https://openrouter.ai/api/v1/audio/speech";
const KEYS_URL: &str = "https://openrouter.ai/api/v1/keys/";

/// A conservative default cap for a key that's going to live cached on a phone — a safety net,
/// not a tuned policy. Resets monthly so a forgotten install doesn't accumulate an unbounded bill.
const PROVISIONED_KEY_LIMIT_USD: f64 = 20.0;

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

/// Mints a new, spend-capped OpenRouter API key using a *management* key (from
/// openrouter.ai/settings/management-keys — a key that can only manage other keys, never make
/// inference calls itself). Not a method on `OpenRouterClient`: it authenticates with a
/// different key than the one that struct wraps.
pub async fn provision_scoped_key(
    http: &reqwest::Client,
    management_key: &str,
    name: &str,
) -> Result<String> {
    let resp = http
        .post(KEYS_URL)
        .bearer_auth(management_key)
        .json(&serde_json::json!({
            "name": name,
            "limit": PROVISIONED_KEY_LIMIT_USD,
            "limit_reset": "monthly",
        }))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(OpenRouterError::Api(format!("{status}: {body}")));
    }
    let value = resp.json::<Value>().await?;
    extract_created_key(&value)
}

/// The create-key response puts the one-time-visible secret at the top level as `key` — e.g.
/// `{"data": {...metadata...}, "key": "sk-or-v1-..."}` — not nested under `data`.
fn extract_created_key(response: &Value) -> Result<String> {
    response["key"].as_str().map(str::to_string).ok_or_else(|| {
        OpenRouterError::Shape(format!(
            "no top-level \"key\" string in response: {response}"
        ))
    })
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

    #[test]
    fn extracts_top_level_key_from_create_key_response() {
        // Deliberately not a plausible-looking key format — GitHub's push-protection secret
        // scanner flags anything matching OpenRouter's real sk-or-v1-<hex> shape, even in tests.
        let fixture = serde_json::json!({
            "data": { "label": "fixture-truncated-label", "hash": "abc", "name": "test" },
            "key": "test-fixture-not-a-real-openrouter-key"
        });
        assert_eq!(
            extract_created_key(&fixture).unwrap(),
            "test-fixture-not-a-real-openrouter-key"
        );
    }

    #[test]
    fn missing_top_level_key_is_a_shape_error() {
        let fixture = serde_json::json!({ "data": {} });
        assert!(extract_created_key(&fixture).is_err());
    }
}
