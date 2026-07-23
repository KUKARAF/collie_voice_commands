use thiserror::Error;

use crate::collie::percent_encode;

#[derive(Debug, Error)]
pub enum KvManagerError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("kv_manager error: {0}")]
    Api(String),
}

pub type Result<T> = std::result::Result<T, KvManagerError>;

pub struct KvManagerClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl KvManagerClient {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        }
    }

    /// Fetches one KV entry's plaintext value. Requires an API key whose `allowed_keys`
    /// scope includes this entry name — see kv_manager's middleware/api_key.rs.
    pub async fn get_entry(&self, key: &str) -> Result<String> {
        let url = format!("{}/kv/{}", self.base_url, percent_encode(key));
        let resp = self
            .http
            .get(url)
            .header("X-Api-Key", &self.api_key)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(KvManagerError::Api(format!("{status}: {body}")));
        }
        Ok(resp.text().await?)
    }
}
