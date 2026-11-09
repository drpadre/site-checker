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