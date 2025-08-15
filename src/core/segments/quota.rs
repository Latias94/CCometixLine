use super::Segment;
use crate::config::InputData;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

#[derive(Debug, Deserialize, Serialize)]
struct ApiQuota {
    remaining: f64,
    total: f64,
    used: f64,
    timestamp: SystemTime,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsageResponse {
    #[serde(rename = "remaining_credit_in_usd")]
    remaining: f64,
    #[serde(rename = "credit_limit_in_usd")]
    limit: f64,
}

#[derive(Debug, Deserialize)]
struct CustomApiUserInfo {
    #[allow(dead_code)]
    balance_usd: String,
    #[allow(dead_code)]
    total_spent_usd: String,
    daily_budget_usd: String,
    daily_spent_usd: Option<String>,
    #[allow(dead_code)]
    monthly_budget_usd: String,
    #[allow(dead_code)]
    monthly_spent_usd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeSettings {
    env: Option<ClaudeCodeEnv>,
    info_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeEnv {
    #[serde(rename = "ANTHROPIC_BASE_URL")]
    base_url: Option<String>,
    #[serde(rename = "ANTHROPIC_AUTH_TOKEN")]
    auth_token: Option<String>,
    #[serde(rename = "ANTHROPIC_API_KEY")]
    api_key: Option<String>,
}

pub struct QuotaSegment {
    enabled: bool,
    api_key: Option<String>,
    base_url: String,
    info_url: Option<String>,
}

impl QuotaSegment {
    pub fn new(enabled: bool) -> Self {
        let (api_key, base_url, info_url) = Self::load_api_config();
        Self {
            enabled,
            api_key,
            base_url,
            info_url,
        }
    }

    fn load_api_config() -> (Option<String>, String, Option<String>) {
        // Try multiple sources for API configuration

        // 1. Claude Code settings.json
        if let Some(config_dir) = Self::get_claude_config_dir() {
            let settings_path = config_dir.join("settings.json");
            if let Ok(content) = fs::read_to_string(&settings_path) {
                if let Ok(settings) = serde_json::from_str::<ClaudeCodeSettings>(&content) {
                    let info_url = settings.info_url.clone();
                    if let Some(env) = settings.env {
                        let api_key = env.auth_token.or(env.api_key);
                        let base_url = env
                            .base_url
                            .unwrap_or_else(|| "https://api.anthropic.com".to_string());
                        if api_key.is_some() {
                            return (api_key, base_url, info_url);
                        }
                    }
                }
            }
        }

        // 2. Environment variable
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .or_else(|| std::env::var("ANTHROPIC_AUTH_TOKEN").ok());

        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_string());

        let info_url = std::env::var("INFO_URL").ok();

        // 3. Claude Code api_key file
        if api_key.is_none() {
            if let Some(home) = dirs::home_dir() {
                let config_path = home.join(".claude").join("api_key");
                if let Ok(key) = fs::read_to_string(config_path) {
                    return (Some(key.trim().to_string()), base_url, info_url);
                }
            }
        }

        (api_key, base_url, info_url)
    }

    fn get_claude_config_dir() -> Option<PathBuf> {
        // Claude Code config directory is ~/.claude
        dirs::home_dir().map(|home| home.join(".claude"))
    }

    // Cache methods removed - no longer needed

    fn fetch_quota(&self) -> Option<ApiQuota> {
        // No cache - fetch fresh data every time
        // Fetch from API
        let api_key = self.api_key.as_ref()?;

        // If we have a custom info_url, use that instead
        if let Some(info_url) = &self.info_url {
            let client = reqwest::blocking::Client::new();
            let response = client
                .get(info_url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("accept", "*/*")
                .header("content-type", "application/json")
                .timeout(Duration::from_secs(5))
                .send()
                .ok()?;

            if response.status().is_success() {
                let user_info: CustomApiUserInfo = response.json().ok()?;

                // Parse the string values
                let daily_budget = user_info.daily_budget_usd.parse::<f64>().unwrap_or(0.0);
                let daily_spent = user_info
                    .daily_spent_usd
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);

                let quota = ApiQuota {
                    remaining: daily_budget - daily_spent,
                    total: daily_budget,
                    used: daily_spent,
                    timestamp: SystemTime::now(),
                };

                // No cache anymore

                return Some(quota);
            }
        }

        // Fallback to standard Anthropic API
        let url = if self.base_url.contains("api.anthropic.com") {
            format!("{}/v1/dashboard/usage", self.base_url)
        } else {
            // For proxy/custom endpoints, try common patterns
            format!("{}/v1/dashboard/usage", self.base_url)
        };

        let client = reqwest::blocking::Client::new();
        let mut request = client.get(&url).timeout(Duration::from_secs(5));

        // Handle different auth header formats based on the endpoint
        if self.base_url.contains("api.anthropic.com") {
            request = request
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01");
        } else {
            // For custom endpoints, try both header formats
            request = request
                .header("Authorization", format!("Bearer {}", api_key))
                .header("x-api-key", api_key);
        }

        let response = request.send().ok()?;

        if response.status().is_success() {
            let usage: AnthropicUsageResponse = response.json().ok()?;

            let quota = ApiQuota {
                remaining: usage.remaining,
                total: usage.limit,
                used: usage.limit - usage.remaining,
                timestamp: SystemTime::now(),
            };

            // No cache anymore

            Some(quota)
        } else {
            None
        }
    }

    fn format_quota(&self, quota: &ApiQuota) -> String {
        // For daily spent display
        format!("Today: ${:.2}", quota.used)
    }
}

impl Segment for QuotaSegment {
    fn render(&self, _input: &InputData) -> String {
        if !self.enabled || self.api_key.is_none() {
            return String::new();
        }

        // Try to fetch quota (from cache or API)
        if let Some(quota) = self.fetch_quota() {
            self.format_quota(&quota)
        } else {
            // If we can't get quota, show unknown
            "◔ Quota: N/A".to_string()
        }
    }

    fn enabled(&self) -> bool {
        self.enabled && self.api_key.is_some()
    }
}
