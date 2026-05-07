use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::ApiResult;

pub const CACHE_PREFIX: &str = "maint:";

#[derive(Clone)]
pub struct CacheManager {
    inner: Option<ConnectionManager>,
}

impl CacheManager {
    pub async fn new(redis_url: Option<&str>) -> Self {
        match redis_url {
            Some(url) => match redis::Client::open(url) {
                Ok(client) => match ConnectionManager::new(client).await {
                    Ok(mgr) => {
                        tracing::info!("redis connection manager ready");
                        Self { inner: Some(mgr) }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to init redis ConnectionManager; running without cache");
                        Self { inner: None }
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "failed to open redis client; running without cache");
                    Self { inner: None }
                }
            },
            None => {
                tracing::info!("redis url not configured; running without cache");
                Self { inner: None }
            }
        }
    }

    fn key(suffix: &str) -> String {
        format!("{CACHE_PREFIX}{suffix}")
    }

    pub async fn get_json<T: DeserializeOwned>(&self, suffix: &str) -> ApiResult<Option<T>> {
        let mut mgr = match self.inner.clone() {
            Some(m) => m,
            None => return Ok(None),
        };
        let raw: Option<String> = mgr.get(Self::key(suffix)).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "redis get failed; treating as miss");
            None
        });
        match raw {
            Some(s) => match serde_json::from_str::<T>(&s) {
                Ok(v) => Ok(Some(v)),
                Err(e) => {
                    tracing::warn!(error = %e, "cache JSON decode failed; treating as miss");
                    Ok(None)
                }
            },
            None => Ok(None),
        }
    }

    pub async fn set_json<T: Serialize>(
        &self,
        suffix: &str,
        value: &T,
        ttl_seconds: u64,
    ) -> ApiResult<()> {
        let mut mgr = match self.inner.clone() {
            Some(m) => m,
            None => return Ok(()),
        };
        let s = serde_json::to_string(value).unwrap_or_default();
        let _: Result<(), _> = mgr.set_ex(Self::key(suffix), s, ttl_seconds).await;
        Ok(())
    }

    pub async fn delete(&self, suffix: &str) -> ApiResult<()> {
        let mut mgr = match self.inner.clone() {
            Some(m) => m,
            None => return Ok(()),
        };
        let _: Result<(), _> = mgr.del(Self::key(suffix)).await;
        Ok(())
    }

    /// Delete all keys matching `maint:<pattern>`. Used for invalidation.
    pub async fn delete_pattern(&self, pattern_suffix: &str) -> ApiResult<()> {
        let mut mgr = match self.inner.clone() {
            Some(m) => m,
            None => return Ok(()),
        };
        let pattern = format!("{CACHE_PREFIX}{pattern_suffix}");
        let mut iter = match mgr.scan_match::<_, String>(pattern).await {
            Ok(it) => it,
            Err(e) => {
                tracing::warn!(error = %e, "redis scan failed");
                return Ok(());
            }
        };
        let mut keys: Vec<String> = Vec::new();
        while let Some(k) = iter.next_item().await {
            keys.push(k);
        }
        drop(iter);
        if !keys.is_empty() {
            let _: Result<(), _> = mgr.del(keys).await;
        }
        Ok(())
    }
}
