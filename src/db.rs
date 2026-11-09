use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

pub fn now_str() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn after_secs_str(secs: i64) -> String {
    (Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub username: Option<String>,
    pub first_name: String,
    pub is_blocked: bool,
    pub is_banned: bool,
    pub created_at: String,
}

impl User {
    pub fn display(&self) -> String {
        match &self.username {
            Some(u) => format!("{u} (id {})", self.id),
            None => format!("{} (id {})", self.first_name, self.id),
        }
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Site {
    pub id: i64,
    pub user_id: i64,
    pub url: String,
    pub search_text: Option<String>,
    pub interval_secs: i64,
    pub enabled: bool,
    pub status: String,
    pub fail_count: i64,
    pub last_check_at: Option<String>,
    pub next_check_at: String,
    pub last_error: Option<String>,
    pub down_since: Option<String>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS users (
    id          INTEGER PRIMARY KEY,
    username    TEXT,
    first_name  TEXT NOT NULL DEFAULT '',
    is_blocked  INTEGER NOT NULL DEFAULT 0,
    is_banned   INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sites (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    url           TEXT NOT NULL,
    search_text   TEXT,
    interval_secs INTEGER NOT NULL,
    enabled       INTEGER NOT NULL DEFAULT 1,
    status        TEXT NOT NULL DEFAULT 'unknown',
    fail_count    INTEGER NOT NULL DEFAULT 0,
    last_check_at TEXT,
    next_check_at TEXT NOT NULL,
    last_error    TEXT,
    down_since    TEXT,
    created_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sites_user ON sites(user_id);
CREATE INDEX IF NOT EXISTS idx_sites_due ON sites(enabled, next_check_at);
";

pub async fn init(path: &str) -> Result<SqlitePool> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let opts = SqliteConnectOptions::from_str(path)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(10));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    sqlx::raw_sql(SCHEMA).execute(&pool).await?;
    let has_banned: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info('users') WHERE name = 'is_banned'")
            .fetch_one(&pool)
            .await?;
    if has_banned == 0 {
        sqlx::query("ALTER TABLE users ADD COLUMN is_banned INTEGER NOT NULL DEFAULT 0")
            .execute(&pool)
            .await?;
    }
    Ok(pool)
}

// ---------- users ----------

pub async fn upsert_user(pool: &SqlitePool, id: i64, username: Option<&str>, first_name: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO users (id, username, first_name, is_blocked, created_at)
         VALUES (?, ?, ?, 0, ?)
         ON CONFLICT(id) DO UPDATE SET
           username = excluded.username,
           first_name = excluded.first_name,
           is_blocked = 0",
    )
    .bind(id)
    .bind(username)
    .bind(first_name)
    .bind(now_str())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn insert_user_placeholder(pool: &SqlitePool, id: i64) -> Result<bool> {
    let res = sqlx::query(
        "INSERT OR IGNORE INTO users (id, first_name, created_at) VALUES (?, '(добавлен администратором)', ?)",
    )
    .bind(id)
    .bind(now_str())
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn get_user(pool: &SqlitePool, id: i64) -> Result<Option<User>> {
    Ok(sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

pub async fn list_users(pool: &SqlitePool, limit: i64, offset: i64) -> Result<Vec<User>> {
    Ok(sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY created_at DESC, id LIMIT ? OFFSET ?")
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?)
}

pub async fn count_users(pool: &SqlitePool) -> Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?)
}

pub async fn delete_user(pool: &SqlitePool, id: i64) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected())
}

pub async fn set_blocked(pool: &SqlitePool, id: i64, blocked: bool) -> Result<()> {
    sqlx::query("UPDATE users SET is_blocked = ? WHERE id = ?")
        .bind(blocked)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_banned(pool: &SqlitePool, id: i64, banned: bool) -> Result<()> {
    sqlx::query("UPDATE users SET is_banned = ? WHERE id = ?")
        .bind(banned)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------- sites ----------

pub async fn add_site(
    pool: &SqlitePool,
    user_id: i64,
    url: &str,
    search_text: Option<&str>,
    interval_secs: i64,
) -> Result<i64> {
    let res = sqlx::query(
        "INSERT INTO sites (user_id, url, search_text, interval_secs, next_check_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(url)
    .bind(search_text)
    .bind(interval_secs)
    .bind(now_str())
    .bind(now_str())
    .execute(pool)
    .await?;
    Ok(res.last_insert_rowid())
}

pub async fn get_site(pool: &SqlitePool, id: i64) -> Result<Option<Site>> {
    Ok(sqlx::query_as::<_, Site>("SELECT * FROM sites WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

pub async fn list_sites(pool: &SqlitePool, user_id: i64) -> Result<Vec<Site>> {
    Ok(sqlx::query_as::<_, Site>("SELECT * FROM sites WHERE user_id = ? ORDER BY id")
        .bind(user_id)
        .fetch_all(pool)
        .await?)
}

pub async fn site_stats(pool: &SqlitePool, user_id: i64) -> Result<(i64, i64)> {
    let row: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(enabled = 1 AND status = 'down'), 0) FROM sites WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn global_stats(pool: &SqlitePool) -> Result<(i64, i64, i64)> {
    let users = count_users(pool).await?;
    let row: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(enabled = 1 AND status = 'down'), 0) FROM sites",
    )
    .fetch_one(pool)
    .await?;
    Ok((users, row.0, row.1))
}

pub async fn update_site_url(pool: &SqlitePool, id: i64, url: &str) -> Result<()> {
    sqlx::query(
        "UPDATE sites SET url = ?, status = 'unknown', fail_count = 0, last_error = NULL,
         down_since = NULL, next_check_at = ? WHERE id = ?",
    )
    .bind(url)
    .bind(now_str())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_site_text(pool: &SqlitePool, id: i64, text: Option<&str>) -> Result<()> {
    sqlx::query(
        "UPDATE sites SET search_text = ?, status = 'unknown', fail_count = 0, last_error = NULL,
         down_since = NULL, next_check_at = ? WHERE id = ?",
    )
    .bind(text)
    .bind(now_str())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_site_interval(pool: &SqlitePool, id: i64, interval_secs: i64) -> Result<()> {
    sqlx::query("UPDATE sites SET interval_secs = ?, next_check_at = ? WHERE id = ?")
        .bind(interval_secs)
        .bind(after_secs_str(interval_secs))
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_site_enabled(pool: &SqlitePool, id: i64, enabled: bool) -> Result<()> {
    let next = if enabled { now_str() } else { after_secs_str(3600) };
    sqlx::query("UPDATE sites SET enabled = ?, next_check_at = ? WHERE id = ?")
        .bind(enabled)
        .bind(next)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_site(pool: &SqlitePool, id: i64) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM sites WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected())
}

pub async fn due_sites(pool: &SqlitePool, now: &str, limit: i64) -> Result<Vec<Site>> {
    Ok(sqlx::query_as::<_, Site>(
        "SELECT sites.* FROM sites JOIN users ON users.id = sites.user_id
         WHERE sites.enabled = 1 AND sites.next_check_at <= ? AND users.is_banned = 0
         ORDER BY sites.next_check_at LIMIT ?",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

#[allow(clippy::too_many_arguments)]
pub async fn record_check(
    pool: &SqlitePool,
    id: i64,
    status: &str,
    fail_count: i64,
    last_error: Option<&str>,
    down_since: Option<&str>,
    next_check_at: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE sites SET status = ?, fail_count = ?, last_error = ?, down_since = ?,
         last_check_at = ?, next_check_at = ? WHERE id = ?",
    )
    .bind(status)
    .bind(fail_count)
    .bind(last_error)
    .bind(down_since)
    .bind(now_str())
    .bind(next_check_at)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        let opts = SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::raw_sql(SCHEMA).execute(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn users_crud() {
        let pool = test_pool().await;
        upsert_user(&pool, 1, Some("alice"), "Alice").await.unwrap();
        upsert_user(&pool, 2, None, "Bob").await.unwrap();

        let u = get_user(&pool, 1).await.unwrap().unwrap();
        assert_eq!(u.username.as_deref(), Some("alice"));
        assert!(!u.is_blocked);
        assert!(!u.is_banned);
        assert_eq!(count_users(&pool).await.unwrap(), 2);
        assert_eq!(list_users(&pool, 10, 0).await.unwrap().len(), 2);

        assert!(insert_user_placeholder(&pool, 3).await.unwrap());
        assert!(!insert_user_placeholder(&pool, 3).await.unwrap());

        set_blocked(&pool, 1, true).await.unwrap();
        assert!(get_user(&pool, 1).await.unwrap().unwrap().is_blocked);
        upsert_user(&pool, 1, Some("alice_new"), "Alice").await.unwrap();
        let u = get_user(&pool, 1).await.unwrap().unwrap();
        assert!(!u.is_blocked);
        assert_eq!(u.username.as_deref(), Some("alice_new"));

        assert_eq!(delete_user(&pool, 2).await.unwrap(), 1);
        assert!(get_user(&pool, 2).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn sites_crud_and_cascade_delete() {
        let pool = test_pool().await;
        upsert_user(&pool, 1, None, "Owner").await.unwrap();
        let id = add_site(&pool, 1, "https://example.com/", Some("hello"), 300).await.unwrap();
        add_site(&pool, 1, "https://other.com/", None, 3600).await.unwrap();

        let sites = list_sites(&pool, 1).await.unwrap();
        assert_eq!(sites.len(), 2);
        assert_eq!(site_stats(&pool, 1).await.unwrap(), (2, 0));

        let s = get_site(&pool, id).await.unwrap().unwrap();
        assert_eq!(s.status, "unknown");
        assert_eq!(s.search_text.as_deref(), Some("hello"));

        update_site_url(&pool, id, "https://new.example.com/").await.unwrap();
        update_site_text(&pool, id, None).await.unwrap();
        update_site_interval(&pool, id, 900).await.unwrap();
        let s = get_site(&pool, id).await.unwrap().unwrap();
        assert_eq!(s.url, "https://new.example.com/");
        assert!(s.search_text.is_none());
        assert_eq!(s.interval_secs, 900);

        record_check(&pool, id, "down", 1, Some("HTTP 500"), Some(&now_str()), &after_secs_str(900)).await.unwrap();
        assert_eq!(site_stats(&pool, 1).await.unwrap(), (2, 1));
        assert_eq!(global_stats(&pool).await.unwrap(), (1, 2, 1));

        delete_user(&pool, 1).await.unwrap();
        assert!(list_sites(&pool, 1).await.unwrap().is_empty());
        assert_eq!(global_stats(&pool).await.unwrap(), (0, 0, 0));
    }

    #[tokio::test]
    async fn due_sites_queue() {
        let pool = test_pool().await;
        upsert_user(&pool, 1, None, "Owner").await.unwrap();
        let id = add_site(&pool, 1, "https://example.com/", None, 300).await.unwrap();

        let due = due_sites(&pool, &now_str(), 10).await.unwrap();
        assert_eq!(due.len(), 1);

        set_site_enabled(&pool, id, false).await.unwrap();
        assert!(due_sites(&pool, &now_str(), 10).await.unwrap().is_empty());

        set_site_enabled(&pool, id, true).await.unwrap();
        assert_eq!(due_sites(&pool, &now_str(), 10).await.unwrap().len(), 1);

        record_check(&pool, id, "up", 0, None, None, &after_secs_str(300)).await.unwrap();
        assert!(due_sites(&pool, &now_str(), 10).await.unwrap().is_empty());
        assert_eq!(due_sites(&pool, &after_secs_str(301), 10).await.unwrap().len(), 1);

        set_banned(&pool, 1, true).await.unwrap();
        assert!(due_sites(&pool, &after_secs_str(301), 10).await.unwrap().is_empty());
        set_banned(&pool, 1, false).await.unwrap();
        assert_eq!(due_sites(&pool, &after_secs_str(301), 10).await.unwrap().len(), 1);
    }
}