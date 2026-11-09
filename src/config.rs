use std::collections::HashSet;
use std::env;

use anyhow::{bail, Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub bot_token: String,
    pub admin_ids: HashSet<i64>,
    pub database_path: String,
    pub tick_secs: u64,
    pub http_timeout_secs: u64,
    pub max_concurrent_checks: usize,
    pub failure_threshold: i64,
}

fn var(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn parse_num<T: std::str::FromStr>(key: &str, default: &str) -> Result<T>
where
    <T as std::str::FromStr>::Err: std::error::Error + Send + Sync + 'static,
{
    let raw = var(key, default);
    raw.parse()
        .with_context(|| format!("{key}={raw:?}: ожидалось число"))
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bot_token = env::var("BOT_TOKEN").context("BOT_TOKEN не задан в .env")?;
        if bot_token.trim().is_empty() || bot_token.contains("ВСТАВЬТЕ") {
            bail!("BOT_TOKEN не заполнен: получите токен у @BotFather и укажите его в .env");
        }

        let admin_ids: HashSet<i64> = var("ADMIN_IDS", "547169256")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<i64>().with_context(|| format!("ADMIN_IDS: {s:?} не число")))
            .collect::<Result<_>>()?;

        let cfg = Self {
            bot_token,
            admin_ids,
            database_path: var("DATABASE_PATH", "data/site_checker.db"),
            tick_secs: parse_num("CHECK_TICK_SECONDS", "15")?,
            http_timeout_secs: parse_num("HTTP_TIMEOUT_SECONDS", "15")?,
            max_concurrent_checks: parse_num("MAX_CONCURRENT_CHECKS", "10")?,
            failure_threshold: parse_num("FAILURE_THRESHOLD", "1")?,
        };
        if cfg.tick_secs == 0 || cfg.http_timeout_secs == 0 || cfg.max_concurrent_checks == 0 {
            bail!("CHECK_TICK_SECONDS, HTTP_TIMEOUT_SECONDS и MAX_CONCURRENT_CHECKS должны быть > 0");
        }
        if cfg.failure_threshold < 1 {
            bail!("FAILURE_THRESHOLD должен быть >= 1");
        }
        Ok(cfg)
    }

    pub fn is_admin(&self, user_id: i64) -> bool {
        self.admin_ids.contains(&user_id)
    }
}