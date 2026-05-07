use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::Client;
use std::time::Duration;

use crate::error::{ApiError, ApiResult};

#[derive(Clone)]
pub struct FalconClient {
    inner: Client,
    base_url: String,
}

impl FalconClient {
    pub fn new(base_url: String) -> Self {
        let inner = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Some(Duration::from_secs(60)))
            .user_agent(concat!("maint-rust/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client build failed");
        Self { inner, base_url }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {token}")) {
            h.insert(AUTHORIZATION, v);
        }
        h
    }

    pub async fn get_json(&self, path: &str, token: &str) -> ApiResult<serde_json::Value> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .inner
            .get(&url)
            .headers(Self::auth_headers(token))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ApiError::UpstreamTimeout
                } else {
                    ApiError::Reqwest(e)
                }
            })?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::UpstreamAuth);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::UpstreamFalcon {
                status: status.as_u16(),
                body,
            });
        }
        let v = resp.json::<serde_json::Value>().await?;
        Ok(v)
    }

    pub async fn post_json(
        &self,
        path: &str,
        token: &str,
        body: &serde_json::Value,
    ) -> ApiResult<serde_json::Value> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .inner
            .post(&url)
            .headers(Self::auth_headers(token))
            .json(body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ApiError::UpstreamTimeout
                } else {
                    ApiError::Reqwest(e)
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::UpstreamFalcon {
                status: status.as_u16(),
                body,
            });
        }
        let v = resp
            .json::<serde_json::Value>()
            .await
            .unwrap_or(serde_json::json!({}));
        Ok(v)
    }
}
