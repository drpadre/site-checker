use std::collections::{HashMap, HashSet};

use sqlx::SqlitePool;
use teloxide::adaptors::Throttle;
use teloxide::Bot;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::db;

pub type TBot = Throttle<Bot>;

#[derive(Clone, Debug)]
pub enum Dialog {
    AddUrl { for_user: i64 },
    AddText { for_user: i64, url: String },
    AddInterval { for_user: i64, url: String, text: Option<String> },
    EditUrl { site_id: i64 },
    EditText { site_id: i64 },
    AdminAddUser,
}

pub struct AppState {
    pub cfg: Config,
    pub db: SqlitePool,
    pub http: reqwest::Client,
    pub bot: TBot,
    pub dialogs: Mutex<HashMap<i64, Dialog>>,
    pub acting_as: Mutex<HashMap<i64, i64>>,
    pub in_flight: Mutex<HashSet<i64>>,
    pub notify_lock: Mutex<()>,
}

impl AppState {
    /// Для админа в режиме «от имени пользователя» возвращает целевого пользователя.
    pub async fn effective_user(&self, sender: i64) -> i64 {
        if self.cfg.is_admin(sender) {
            let target = self.acting_as.lock().await.get(&sender).copied();
            if let Some(target) = target {
                if db::get_user(&self.db, target).await.ok().flatten().is_some() {
                    return target;
                }
                self.acting_as.lock().await.remove(&sender);
            }
        }
        sender
    }

    pub async fn impersonation_target(&self, admin: i64) -> Option<i64> {
        if self.cfg.is_admin(admin) {
            self.acting_as.lock().await.get(&admin).copied()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use teloxide::adaptors::throttle::Limits;
    use teloxide::prelude::*;

    async fn test_state() -> AppState {
        let cfg = Config {
            bot_token: "123456:TEST".into(),
            admin_ids: HashSet::from([1]),
            database_path: ":memory:".into(),
            tick_secs: 15,
            http_timeout_secs: 15,
            max_concurrent_checks: 10,
            failure_threshold: 1,
            max_sites_per_user: 50,
            backup_dir: "data/backups".into(),
            backup_interval_hours: 24,
            backup_keep: 7,
        };
        let pool = db::test_pool().await;
        let http = crate::checker::build_client(5).unwrap();
        let bot = Bot::new("123456:TEST").throttle(Limits::default());
        AppState {
            cfg,
            db: pool,
            http,
            bot,
            dialogs: Mutex::new(Default::default()),
            acting_as: Mutex::new(Default::default()),
            in_flight: Mutex::new(Default::default()),
            notify_lock: Mutex::new(()),
        }
    }

    #[tokio::test]
    async fn effective_user_normal_and_impersonation() {
        let state = test_state().await;
        db::upsert_user(&state.db, 1, Some("admin"), "Admin").await.unwrap();
        db::upsert_user(&state.db, 2, None, "User").await.unwrap();

        assert_eq!(state.effective_user(2).await, 2);
        assert_eq!(state.impersonation_target(2).await, None);
        assert_eq!(state.effective_user(1).await, 1);

        state.acting_as.lock().await.insert(1, 2);
        assert_eq!(state.effective_user(1).await, 2);
        assert_eq!(state.impersonation_target(1).await, Some(2));

        // не-админ не может использовать режим даже с записью в acting_as
        state.acting_as.lock().await.insert(2, 1);
        assert_eq!(state.effective_user(2).await, 2);
        assert_eq!(state.impersonation_target(2).await, None);
    }

    #[tokio::test]
    async fn impersonation_self_heals_deleted_target() {
        let state = test_state().await;
        db::upsert_user(&state.db, 1, None, "Admin").await.unwrap();
        db::upsert_user(&state.db, 2, None, "User").await.unwrap();
        state.acting_as.lock().await.insert(1, 2);
        assert_eq!(state.effective_user(1).await, 2);

        db::delete_user(&state.db, 2).await.unwrap();
        assert_eq!(state.effective_user(1).await, 1);
        assert!(state.acting_as.lock().await.get(&1).is_none());
    }
}