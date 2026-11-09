use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use teloxide::payloads::SendMessageSetters;
use teloxide::prelude::Requester;
use teloxide::types::{ChatId, ParseMode};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::bot::state::AppState;
use crate::bot::ui;
use crate::checker::{self, CheckResult};
use crate::db::{self, Site};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    WentDown,
    Recovered,
}

pub struct StatusUpdate {
    pub status: &'static str,
    pub fail_count: i64,
    pub down_since: Option<String>,
    pub transition: Option<Transition>,
}

/// Чистая логика смены статуса: уведомление о падении отправляется только
/// после FAILURE_THRESHOLD неудач подряд, о восстановлении — при возврате в up.
pub fn compute_update(site: &Site, up: bool, threshold: i64) -> StatusUpdate {
    let was_down = site.status == "down";
    if up {
        StatusUpdate {
            status: "up",
            fail_count: 0,
            down_since: None,
            transition: was_down.then_some(Transition::Recovered),
        }
    } else {
        let fail_count = site.fail_count + 1;
        if was_down {
            StatusUpdate {
                status: "down",
                fail_count,
                down_since: site.down_since.clone(),
                transition: None,
            }
        } else if fail_count >= threshold {
            StatusUpdate {
                status: "down",
                fail_count,
                down_since: Some(db::now_str()),
                transition: Some(Transition::WentDown),
            }
        } else {
            StatusUpdate {
                status: if site.status == "up" { "up" } else { "unknown" },
                fail_count,
                down_since: None,
                transition: None,
            }
        }
    }
}

/// Записывает результат проверки и уведомляет владельца при смене статуса.
/// requester — кто запросил проверку вручную: если это владелец, отдельное
/// уведомление ему не дублируется (он получит подробный ответ в чате).
pub async fn process_check(
    state: &AppState,
    site: &Site,
    result: CheckResult,
    requester: Option<i64>,
) -> Result<Option<Transition>> {
    let upd = compute_update(site, result.up, state.cfg.failure_threshold);
    db::record_check(
        &state.db,
        site.id,
        upd.status,
        upd.fail_count,
        if result.up { None } else { Some(result.detail.as_str()) },
        upd.down_since.as_deref(),
        &db::after_secs_str(site.interval_secs),
    )
    .await?;

    if let Some(transition) = upd.transition {
        if requester != Some(site.user_id) {
            notify_owner(state, site, transition, &result.detail).await;
        }
    }
    Ok(upd.transition)
}

async fn notify_owner(state: &AppState, site: &Site, transition: Transition, detail: &str) {
    let now = ui::fmt_dt(&db::now_str());
    let text = match transition {
        Transition::WentDown => format!(
            "🔴 <b>Сайт недоступен</b>\n\n{}\nПричина: {}\n🕐 {now}\n\nБот продолжит проверки и сообщит, когда сайт заработает.",
            ui::esc(&site.url),
            ui::esc(detail),
        ),
        Transition::Recovered => {
            let downtime = site
                .down_since
                .as_deref()
                .map(ui::fmt_ago)
                .unwrap_or_else(|| "неизвестно".into());
            format!(
                "🟢 <b>Сайт снова доступен</b>\n\n{}\n🕐 {now}\nПростой: {downtime}",
                ui::esc(&site.url),
            )
        }
    };

    match state
        .bot
        .send_message(ChatId(site.user_id), text)
        .parse_mode(ParseMode::Html)
        .await
    {
        Ok(_) => {
            if let Ok(Some(user)) = db::get_user(&state.db, site.user_id).await {
                if user.is_blocked {
                    let _ = db::set_blocked(&state.db, site.user_id, false).await;
                }
            }
        }
        Err(e) => {
            tracing::warn!("не удалось уведомить {}: {e}", site.user_id);
            let msg = e.to_string();
            if msg.contains("Forbidden") || msg.contains("chat not found") || msg.contains("blocked") {
                let _ = db::set_blocked(&state.db, site.user_id, true).await;
            }
        }
    }
}

pub async fn run(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(Duration::from_secs(state.cfg.tick_secs));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if let Err(e) = process_due(&state).await {
            tracing::error!("ошибка планировщика: {e:#}");
        }
    }
}

async fn process_due(state: &Arc<AppState>) -> Result<()> {
    let due = db::due_sites(&state.db, &db::now_str(), 200).await?;
    if due.is_empty() {
        return Ok(());
    }
    tracing::debug!("к проверке: {} сайтов", due.len());

    let sem = Arc::new(Semaphore::new(state.cfg.max_concurrent_checks));
    let mut set = JoinSet::new();
    for site in due {
        if !state.in_flight.lock().await.insert(site.id) {
            continue;
        }
        let state = state.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("семафор не закрыт");
            let result = checker::check(&state.http, &site.url, site.search_text.as_deref()).await;
            tracing::debug!("сайт {} ({}): {}", site.id, site.url, result.detail);
            let outcome = process_check(&state, &site, result, None).await;
            state.in_flight.lock().await.remove(&site.id);
            if let Err(e) = outcome {
                tracing::error!("сайт {}: {e:#}", site.id);
            }
        });
    }
    while let Some(joined) = set.join_next().await {
        if let Err(e) = joined {
            tracing::error!("задача проверки упала: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(status: &str, fail_count: i64) -> Site {
        Site {
            id: 1,
            user_id: 1,
            url: "https://example.com/".into(),
            search_text: None,
            interval_secs: 300,
            enabled: true,
            status: status.into(),
            fail_count,
            last_check_at: None,
            next_check_at: db::now_str(),
            last_error: None,
            down_since: None,
        }
    }

    #[test]
    fn first_failure_notifies_with_threshold_1() {
        let upd = compute_update(&site("up", 0), false, 1);
        assert_eq!(upd.status, "down");
        assert_eq!(upd.transition, Some(Transition::WentDown));
        assert!(upd.down_since.is_some());
    }

    #[test]
    fn threshold_delays_notification() {
        let upd = compute_update(&site("up", 0), false, 3);
        assert_eq!(upd.status, "up");
        assert_eq!(upd.fail_count, 1);
        assert_eq!(upd.transition, None);

        let mut s = site("up", 2);
        s.fail_count = 2;
        let upd = compute_update(&s, false, 3);
        assert_eq!(upd.status, "down");
        assert_eq!(upd.transition, Some(Transition::WentDown));
    }

    #[test]
    fn repeated_failure_does_not_renotify() {
        let mut s = site("down", 1);
        s.down_since = Some(db::now_str());
        let upd = compute_update(&s, false, 1);
        assert_eq!(upd.status, "down");
        assert_eq!(upd.transition, None);
        assert_eq!(upd.down_since, s.down_since);
    }

    #[test]
    fn recovery_notifies_once() {
        let mut s = site("down", 5);
        s.down_since = Some(db::now_str());
        let upd = compute_update(&s, true, 1);
        assert_eq!(upd.status, "up");
        assert_eq!(upd.fail_count, 0);
        assert_eq!(upd.transition, Some(Transition::Recovered));
        assert!(upd.down_since.is_none());

        let upd = compute_update(&site("up", 0), true, 1);
        assert_eq!(upd.transition, None);
    }

    #[test]
    fn unknown_site_first_success_no_notification() {
        let upd = compute_update(&site("unknown", 0), true, 1);
        assert_eq!(upd.status, "up");
        assert_eq!(upd.transition, None);
    }
}