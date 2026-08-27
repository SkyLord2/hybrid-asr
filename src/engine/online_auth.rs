use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::OnlineAsrConfig;
use crate::error::HybridAsrResult;

const TOKEN_CACHE_TTL: Duration = Duration::from_secs(4 * 60 * 60);

static APP_ACCESS_TOKEN_CACHE: OnceLock<Mutex<HashMap<String, CachedAccessToken>>> =
    OnceLock::new();

#[derive(Clone, Debug)]
struct CachedAccessToken {
    token: String,
    expires_at: Instant,
}

#[derive(Debug, Serialize)]
struct CreateTokenRequest<'a> {
    kcode: &'a str,
    ksecret: &'a str,
}

#[derive(Debug, Deserialize)]
struct CreateTokenResponse {
    code: i32,
    message: Option<String>,
    data: Option<String>,
}

pub fn get_app_access_token(options: &OnlineAsrConfig) -> HybridAsrResult<String> {
    get_app_access_token_internal(options, false)
}

pub fn refresh_app_access_token(options: &OnlineAsrConfig) -> HybridAsrResult<String> {
    get_app_access_token_internal(options, true)
}

fn get_app_access_token_internal(
    options: &OnlineAsrConfig,
    force_refresh: bool,
) -> HybridAsrResult<String> {
    let cache_key = build_cache_key(options);
    if force_refresh {
        remove_cached_token(&cache_key)?;
    } else if let Some(cached_token) = read_cached_token(&cache_key)? {
        return Ok(cached_token);
    }

    let response: CreateTokenResponse = ureq::post(&options.app_auth_url)
        .set("Content-Type", "application/json")
        .send_json(CreateTokenRequest {
            kcode: &options.k_code,
            ksecret: &options.k_secret,
        })?
        .into_json()?;

    if response.code != 0 {
        return Err(format!(
            "获取 App-Access-Token 失败: code={}, message={}",
            response.code,
            response.message.unwrap_or_default()
        )
        .into());
    }

    let token = response
        .data
        .and_then(|text| {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .ok_or("获取 App-Access-Token 失败: 返回 data 为空")?;

    write_cached_token(cache_key, token.clone())?;
    Ok(token)
}

fn build_cache_key(options: &OnlineAsrConfig) -> String {
    format!(
        "{}::{}::{}",
        options.app_auth_url, options.k_code, options.k_secret
    )
}

fn read_cached_token(cache_key: &str) -> HybridAsrResult<Option<String>> {
    let cache = APP_ACCESS_TOKEN_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().map_err(|_| "App-Access-Token 缓存锁已中毒")?;
    if let Some(entry) = cache.get(cache_key)
        && entry.expires_at > Instant::now()
    {
        return Ok(Some(entry.token.clone()));
    }
    cache.remove(cache_key);
    Ok(None)
}

fn write_cached_token(cache_key: String, token: String) -> HybridAsrResult<()> {
    let cache = APP_ACCESS_TOKEN_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().map_err(|_| "App-Access-Token 缓存锁已中毒")?;
    cache.insert(
        cache_key,
        CachedAccessToken {
            token,
            expires_at: Instant::now() + TOKEN_CACHE_TTL,
        },
    );
    Ok(())
}

fn remove_cached_token(cache_key: &str) -> HybridAsrResult<()> {
    let cache = APP_ACCESS_TOKEN_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().map_err(|_| "App-Access-Token 缓存锁已中毒")?;
    cache.remove(cache_key);
    Ok(())
}
