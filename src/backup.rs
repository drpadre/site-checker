use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::SqlitePool;

/// Создаёт согласованный снимок БД через VACUUM INTO и ротирует старые копии.
pub async fn run_backup(pool: &SqlitePool, dir: &str, keep: usize) -> Result<()> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("не удалось создать каталог бэкапов {dir}"))?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f");
    let filename = format!("site_checker-{stamp}.db");
    let path = Path::new(dir).join(&filename);
    let path_str = path.to_string_lossy().into_owned();
    sqlx::query("VACUUM INTO ?")
        .bind(&path_str)
        .execute(pool)
        .await
        .context("VACUUM INTO")?;
    tracing::info!("бэкап БД создан: {}", path.display());
    rotate(dir, keep).await?;
    Ok(())
}

/// Оставляет keep новейших файлов site_checker-*.db, остальные удаляет.
async fn rotate(dir: &str, keep: usize) -> Result<()> {
    let mut files: Vec<String> = Vec::new();
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("site_checker-") && name.ends_with(".db") {
            files.push(name);
        }
    }
    files.sort();
    if files.len() > keep {
        for name in &files[..files.len() - keep] {
            let p = Path::new(dir).join(name);
            if let Err(e) = tokio::fs::remove_file(&p).await {
                tracing::warn!("не удалось удалить старый бэкап {}: {e}", p.display());
            }
        }
    }
    Ok(())
}

pub async fn backup_loop(pool: SqlitePool, dir: String, interval_hours: u64, keep: usize) {
    let mut tick = tokio::time::interval(Duration::from_secs(interval_hours * 3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if let Err(e) = run_backup(&pool, &dir, keep).await {
            tracing::error!("бэкап БД: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sc-backup-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn rotate_keeps_newest() {
        let dir = temp_dir("rotate");
        for name in [
            "site_checker-20260101-000000-000.db",
            "site_checker-20260102-000000-000.db",
            "site_checker-20260103-000000-000.db",
            "other.txt",
        ] {
            tokio::fs::write(dir.join(name), b"x").await.unwrap();
        }
        rotate(dir.to_str().unwrap(), 2).await.unwrap();
        assert!(!dir.join("site_checker-20260101-000000-000.db").exists());
        assert!(dir.join("site_checker-20260102-000000-000.db").exists());
        assert!(dir.join("site_checker-20260103-000000-000.db").exists());
        assert!(dir.join("other.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn vacuum_into_creates_readable_backup() {
        let dir = temp_dir("vacuum");
        let db_path = dir.join("source.db");
        let opts = SqliteConnectOptions::from_str(db_path.to_str().unwrap())
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (x INTEGER)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO t VALUES (42)").execute(&pool).await.unwrap();

        let backup_dir = dir.join("backups");
        run_backup(&pool, backup_dir.to_str().unwrap(), 7).await.unwrap();

        let mut found = None;
        let mut rd = tokio::fs::read_dir(&backup_dir).await.unwrap();
        while let Some(e) = rd.next_entry().await.unwrap() {
            if e.file_name().to_string_lossy().starts_with("site_checker-") {
                found = Some(e.path());
            }
        }
        let backup_path = found.expect("бэкап должен быть создан");
        let bopts = SqliteConnectOptions::from_str(backup_path.to_str().unwrap()).unwrap();
        let bpool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(bopts)
            .await
            .unwrap();
        let val: i64 = sqlx::query_scalar("SELECT x FROM t").fetch_one(&bpool).await.unwrap();
        assert_eq!(val, 42);
        pool.close().await;
        bpool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}