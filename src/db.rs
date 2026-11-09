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
    pub reminder_secs: i64,
    pub last_reminder_at: Option<String>,
    pub latency_threshold_ms: i64,
    pub slow_notified: bool,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct PendingNotification {
    pub id: i64,
    pub user_id: i64,
    pub text: String,
    pub attempts: i64,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Incident {
    pub started_at: String,
    pub ended_at: Option<String>,
    pub reason: Option<String>,
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
    reminder_secs INTEGER NOT NULL DEFAULT 0,
    last_reminder_at TEXT,
    latency_threshold_ms INTEGER NOT NULL DEFAULT 0,
    slow_notified INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sites_user ON sites(user_id);
CREATE INDEX IF NOT EXISTS idx_sites_due ON sites(enabled, next_check_at);
CREATE TABLE IF NOT EXISTS pending_notifications (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    site_id         INTEGER NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    text            TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pending_due ON pending_notifications(next_attempt_at);
CREATE TABLE IF NOT EXISTS check_history (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    site_id    INTEGER NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    checked_at TEXT NOT NULL,
    up         INTEGER NOT NULL,
    elapsed_ms INTEGER NOT NULL,
    detail     TEXT
);
CREATE INDEX IF NOT EXISTS idx_history_site_time ON check_history(site_id, checked_at);
CREATE TABLE IF NOT EXISTS incidents (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    site_id    INTEGER NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    started_at TEXT NOT NULL,
    ended_at   TEXT,
    reason     TEXT
);
CREATE INDEX IF NOT EXISTS idx_incidents_site ON incidents(site_id, started_at);
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
    add_column(&pool, "users", "is_banned", "ALTER TABLE users ADD COLUMN is_banned INTEGER NOT NULL DEFAULT 0").await?;
    add_column(&pool, "sites", "reminder_secs", "ALTER TABLE sites ADD COLUMN reminder_secs INTEGER NOT NULL DEFAULT 0").await?;
    add_column(&pool, "sites", "last_reminder_at", "ALTER TABLE sites ADD COLUMN last_reminder_at TEXT").await?;
    add_column(&pool, "sites", "latency_threshold_ms", "ALTER TABLE sites ADD COLUMN latency_threshold_ms INTEGER NOT NULL DEFAULT 0").await?;
    add_column(&pool, "sites", "slow_notified", "ALTER TABLE sites ADD COLUMN slow_notified INTEGER NOT NULL DEFAULT 0").await?;
    Ok(pool)
}

/// Добавляет колонку, если её ещё нет (идемпотентная миграция).
/// alter_sql — литеральное выражение, table/column используются только в pragma-проверке.
async fn add_column(pool: &SqlitePool, table: &str, column: &str, alter_sql: &'static str) -> Result<()> {
    let exists: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?")
            .bind(table)
            .bind(column)
            .fetch_one(pool)
            .await?;
    if exists == 0 {
        sqlx::query(alter_sql).execute(pool).await?;
    }
    Ok(())
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

pub async fn count_sites(pool: &SqlitePool, user_id: i64) -> Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sites WHERE user_id = ?")
        .bind(user_id)
        .fetch_one(pool)
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
         down_since = NULL, last_reminder_at = NULL, next_check_at = ? WHERE id = ?",
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
         down_since = NULL, last_reminder_at = NULL, next_check_at = ? WHERE id = ?",
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

pub async fn set_site_reminder(pool: &SqlitePool, id: i64, reminder_secs: i64) -> Result<()> {
    sqlx::query("UPDATE sites SET reminder_secs = ?, last_reminder_at = NULL WHERE id = ?")
        .bind(reminder_secs)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_last_reminder(pool: &SqlitePool, id: i64, ts: &str) -> Result<()> {
    sqlx::query("UPDATE sites SET last_reminder_at = ? WHERE id = ?")
        .bind(ts)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_site_latency_threshold(pool: &SqlitePool, id: i64, threshold_ms: i64) -> Result<()> {
    sqlx::query("UPDATE sites SET latency_threshold_ms = ?, slow_notified = 0 WHERE id = ?")
        .bind(threshold_ms)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_slow_notified(pool: &SqlitePool, id: i64, notified: bool) -> Result<()> {
    sqlx::query("UPDATE sites SET slow_notified = ? WHERE id = ?")
        .bind(notified)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
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
         last_check_at = ?, next_check_at = ?,
         last_reminder_at = CASE WHEN ? IS NULL THEN NULL ELSE last_reminder_at END
         WHERE id = ?",
    )
    .bind(status)
    .bind(fail_count)
    .bind(last_error)
    .bind(down_since)
    .bind(now_str())
    .bind(next_check_at)
    .bind(down_since)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------- notification queue ----------

pub async fn enqueue_notification(
    pool: &SqlitePool,
    user_id: i64,
    site_id: i64,
    text: &str,
) -> Result<i64> {
    let now = now_str();
    let res = sqlx::query(
        "INSERT INTO pending_notifications (user_id, site_id, text, created_at, next_attempt_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(site_id)
    .bind(text)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(res.last_insert_rowid())
}

pub async fn due_notifications(pool: &SqlitePool, now: &str, limit: i64) -> Result<Vec<PendingNotification>> {
    Ok(sqlx::query_as::<_, PendingNotification>(
        "SELECT * FROM pending_notifications WHERE next_attempt_at <= ? ORDER BY id LIMIT ?",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

pub async fn delete_notification(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM pending_notifications WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn defer_notification(pool: &SqlitePool, id: i64, next_attempt_at: &str) -> Result<()> {
    sqlx::query(
        "UPDATE pending_notifications SET attempts = attempts + 1, next_attempt_at = ? WHERE id = ?",
    )
    .bind(next_attempt_at)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_notifications_for_user(pool: &SqlitePool, user_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM pending_notifications WHERE user_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------- history & incidents ----------

pub async fn insert_history(
    pool: &SqlitePool,
    site_id: i64,
    up: bool,
    elapsed_ms: u64,
    detail: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO check_history (site_id, checked_at, up, elapsed_ms, detail)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(site_id)
    .bind(now_str())
    .bind(up as i64)
    .bind(elapsed_ms as i64)
    .bind(detail)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn open_incident(pool: &SqlitePool, site_id: i64, started_at: &str, reason: &str) -> Result<()> {
    sqlx::query("INSERT INTO incidents (site_id, started_at, reason) VALUES (?, ?, ?)")
        .bind(site_id)
        .bind(started_at)
        .bind(reason)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn close_incident(pool: &SqlitePool, site_id: i64, ended_at: &str) -> Result<()> {
    sqlx::query("UPDATE incidents SET ended_at = ? WHERE site_id = ? AND ended_at IS NULL")
        .bind(ended_at)
        .bind(site_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_incidents(pool: &SqlitePool, site_id: i64, limit: i64) -> Result<Vec<Incident>> {
    Ok(sqlx::query_as::<_, Incident>(
        "SELECT started_at, ended_at, reason FROM incidents WHERE site_id = ?
         ORDER BY started_at DESC LIMIT ?",
    )
    .bind(site_id)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// (всего проверок, из них успешных) начиная с since.
pub async fn site_uptime(pool: &SqlitePool, site_id: i64, since: &str) -> Result<(i64, i64)> {
    let row: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(up), 0) FROM check_history WHERE site_id = ? AND checked_at >= ?",
    )
    .bind(site_id)
    .bind(since)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn site_avg_latency(pool: &SqlitePool, site_id: i64, since: &str) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar::<_, Option<i64>>(
        "SELECT CAST(AVG(elapsed_ms) AS INTEGER) FROM check_history
         WHERE site_id = ? AND checked_at >= ? AND up = 1",
    )
    .bind(site_id)
    .bind(since)
    .fetch_one(pool)
    .await?)
}

pub async fn prune_history(pool: &SqlitePool, before: &str) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM check_history WHERE checked_at < ?")
        .bind(before)
        .execute(pool)
        .await?
        .rows_affected())
}

pub async fn prune_incidents(pool: &SqlitePool, before: &str) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM incidents WHERE ended_at IS NOT NULL AND ended_at < ?")
        .bind(before)
        .execute(pool)
        .await?
        .rows_affected())
}

#[cfg(test)]
pub(crate) async fn test_pool() -> SqlitePool {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(count_sites(&pool, 1).await.unwrap(), 2);
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

    #[tokio::test]
    async fn notification_queue_lifecycle() {
        let pool = test_pool().await;
        upsert_user(&pool, 1, None, "Owner").await.unwrap();
        let site_id = add_site(&pool, 1, "https://example.com/", None, 300).await.unwrap();

        let n1 = enqueue_notification(&pool, 1, site_id, "первое").await.unwrap();
        let n2 = enqueue_notification(&pool, 1, site_id, "второе").await.unwrap();

        let due = due_notifications(&pool, &now_str(), 10).await.unwrap();
        assert_eq!(due.len(), 2);
        assert_eq!(due[0].id, n1);
        assert_eq!(due[0].text, "первое");
        assert_eq!(due[0].attempts, 0);

        defer_notification(&pool, n1, &after_secs_str(60)).await.unwrap();
        let due = due_notifications(&pool, &now_str(), 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, n2);
        let future = due_notifications(&pool, &after_secs_str(61), 10).await.unwrap();
        assert_eq!(future.len(), 2);
        assert_eq!(future[0].attempts, 1);

        delete_notification(&pool, n2).await.unwrap();
        assert_eq!(due_notifications(&pool, &after_secs_str(61), 10).await.unwrap().len(), 1);

        delete_notifications_for_user(&pool, 1).await.unwrap();
        assert!(due_notifications(&pool, &after_secs_str(61), 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn notification_cascades_on_site_and_user_delete() {
        let pool = test_pool().await;
        upsert_user(&pool, 1, None, "Owner").await.unwrap();
        let site_id = add_site(&pool, 1, "https://example.com/", None, 300).await.unwrap();
        enqueue_notification(&pool, 1, site_id, "x").await.unwrap();
        delete_site(&pool, site_id).await.unwrap();
        assert!(due_notifications(&pool, &now_str(), 10).await.unwrap().is_empty());

        let site_id = add_site(&pool, 1, "https://example.com/", None, 300).await.unwrap();
        enqueue_notification(&pool, 1, site_id, "y").await.unwrap();
        delete_user(&pool, 1).await.unwrap();
        assert!(due_notifications(&pool, &now_str(), 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn history_uptime_and_incidents() {
        let pool = test_pool().await;
        upsert_user(&pool, 1, None, "Owner").await.unwrap();
        let site_id = add_site(&pool, 1, "https://example.com/", None, 300).await.unwrap();
        let since = after_secs_str(-3600);

        insert_history(&pool, site_id, true, 100, "HTTP 200").await.unwrap();
        insert_history(&pool, site_id, true, 200, "HTTP 200").await.unwrap();
        insert_history(&pool, site_id, false, 0, "HTTP 500").await.unwrap();
        assert_eq!(site_uptime(&pool, site_id, &since).await.unwrap(), (3, 2));
        assert_eq!(site_avg_latency(&pool, site_id, &since).await.unwrap(), Some(150));

        open_incident(&pool, site_id, &now_str(), "HTTP 500").await.unwrap();
        let inc = list_incidents(&pool, site_id, 10).await.unwrap();
        assert_eq!(inc.len(), 1);
        assert!(inc[0].ended_at.is_none());
        assert_eq!(inc[0].reason.as_deref(), Some("HTTP 500"));

        close_incident(&pool, site_id, &now_str()).await.unwrap();
        let inc = list_incidents(&pool, site_id, 10).await.unwrap();
        assert!(inc[0].ended_at.is_some());

        // закрытый инцидент старше cutoff удаляется, открытый — нет
        open_incident(&pool, site_id, &now_str(), "timeout").await.unwrap();
        prune_incidents(&pool, &after_secs_str(3600)).await.unwrap();
        assert_eq!(list_incidents(&pool, site_id, 10).await.unwrap().len(), 1);

        prune_history(&pool, &after_secs_str(3600)).await.unwrap();
        assert_eq!(site_uptime(&pool, site_id, &after_secs_str(-7200)).await.unwrap(), (0, 0));
        assert_eq!(site_avg_latency(&pool, site_id, &since).await.unwrap(), None);
    }
}