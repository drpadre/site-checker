use std::collections::HashSet;
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

    db::insert_history(&state.db, site.id, result.up, result.elapsed_ms, &result.detail).await?;
    match upd.transition {
        Some(Transition::WentDown) => {
            if let Some(started) = upd.down_since.as_deref() {
                db::open_incident(&state.db, site.id, started, &result.detail).await?;
            }
        }
        Some(Transition::Recovered) => {
            db::close_incident(&state.db, site.id, &db::now_str()).await?;
        }
        None => {}
    }

    if let Some(transition) = upd.transition {
        if requester != Some(site.user_id) {
            let text = render_transition(site, transition, &result.detail);
            db::enqueue_notification(&state.db, site.user_id, site.id, &text).await?;
            flush_notifications(state).await;
        }
    }

    // Пока сайт остаётся недоступным, периодически напоминаем (только автопроверки).
    if requester.is_none() && upd.transition.is_none() && upd.status == "down" {
        let now = db::now_str();
        if reminder_due(
            site.reminder_secs,
            site.last_reminder_at.as_deref(),
            site.down_since.as_deref(),
            &now,
        ) {
            let text = render_reminder(site, &now);
            db::enqueue_notification(&state.db, site.user_id, site.id, &text).await?;
            db::set_last_reminder(&state.db, site.id, &now).await?;
            flush_notifications(state).await;
        }
    }

    // Алерт о медленном ответе (только автопроверки): один раз за эпизод.
    if requester.is_none() {
        if let Some(new_flag) = slow_decision(
            result.up,
            result.elapsed_ms,
            site.latency_threshold_ms,
            site.slow_notified,
        ) {
            db::set_slow_notified(&state.db, site.id, new_flag).await?;
            if new_flag {
                let text = render_slow(site, result.elapsed_ms, site.latency_threshold_ms);
                db::enqueue_notification(&state.db, site.user_id, site.id, &text).await?;
                flush_notifications(state).await;
            }
        }
    }
    Ok(upd.transition)
}

/// Решение по slow-алерту: Some(true) — установить флаг и отправить алерт,
/// Some(false) — сбросить флаг (ответ снова быстрый или сайт упал), None — без изменений.
pub fn slow_decision(up: bool, elapsed_ms: u64, threshold_ms: i64, already_notified: bool) -> Option<bool> {
    if threshold_ms <= 0 {
        return None;
    }
    let slow = up && elapsed_ms as i64 > threshold_ms;
    match (slow, already_notified) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

pub fn render_slow(site: &Site, elapsed_ms: u64, threshold_ms: i64) -> String {
    format!(
        "🐌 <b>Медленный ответ</b>\n\n{}\n⏱ Ответил за {elapsed_ms} мс (порог: {threshold_ms} мс)\n\nСайт доступен, но отвечает медленнее обычного.",
        ui::esc(&site.url),
    )
}

/// Пора ли напомнить о продолжающемся простое. Точка отсчёта — последнее
/// напоминание, а если его не было — начало простоя.
pub fn reminder_due(
    reminder_secs: i64,
    last_reminder_at: Option<&str>,
    down_since: Option<&str>,
    now: &str,
) -> bool {
    if reminder_secs <= 0 {
        return false;
    }
    let Some(base) = last_reminder_at.or(down_since).and_then(db::parse_ts) else {
        return false;
    };
    let Some(now_dt) = db::parse_ts(now) else {
        return false;
    };
    (now_dt - base).num_seconds() >= reminder_secs
}

pub fn render_reminder(site: &Site, now: &str) -> String {
    let downtime = site
        .down_since
        .as_deref()
        .map(ui::fmt_ago)
        .unwrap_or_else(|| "неизвестно".into());
    let now_fmt = ui::fmt_dt(now);
    format!(
        "🔴 <b>Сайт всё ещё недоступен</b>\n\n{}\n⏳ Простой: {downtime}\n🕐 {now_fmt}\n\nБот сообщит, когда сайт заработает.",
        ui::esc(&site.url),
    )
}

/// Готовит текст уведомления о смене статуса. Вызывается один раз при переходе,
/// результат сохраняется в очередь и переживает неудачные попытки доставки.
pub fn render_transition(site: &Site, transition: Transition, detail: &str) -> String {
    let now = ui::fmt_dt(&db::now_str());
    match transition {
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
    }
}

/// Задержка перед повторной попыткой доставки: 30с, 60с, 120с… максимум 5 минут.
pub fn notification_backoff(attempts: i64) -> i64 {
    let exp = 30i64.saturating_mul(2i64.saturating_pow(attempts.clamp(0, 10) as u32));
    exp.min(300)
}

/// Доставляет созревшие уведомления из очереди. Успех — удаляет строку;
/// постоянная ошибка (Forbidden/chat not found) — помечает пользователя
/// заблокировавшим бота и удаляет строку; временная — откладывает с backoff.
/// Заблокированным админом пользователям уведомления не шлются.
pub async fn flush_notifications(state: &AppState) {
    let _guard = state.notify_lock.lock().await;
    let now = db::now_str();
    let due = match db::due_notifications(&state.db, &now, 100).await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("очередь уведомлений: {e:#}");
            return;
        }
    };
    let mut skip_users: HashSet<i64> = HashSet::new();
    for n in due {
        if skip_users.contains(&n.user_id) {
            continue;
        }
        if let Ok(Some(u)) = db::get_user(&state.db, n.user_id).await {
            if u.is_banned {
                let _ = db::delete_notification(&state.db, n.id).await;
                continue;
            }
        }
        match state
            .bot
            .send_message(ChatId(n.user_id), n.text.clone())
            .parse_mode(ParseMode::Html)
            .await
        {
            Ok(_) => {
                let _ = db::delete_notification(&state.db, n.id).await;
                if let Ok(Some(u)) = db::get_user(&state.db, n.user_id).await {
                    if u.is_blocked {
                        let _ = db::set_blocked(&state.db, n.user_id, false).await;
                    }
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("Forbidden") || msg.contains("chat not found") || msg.contains("blocked") {
                    let _ = db::set_blocked(&state.db, n.user_id, true).await;
                    let _ = db::delete_notification(&state.db, n.id).await;
                    skip_users.insert(n.user_id);
                } else {
                    let backoff = notification_backoff(n.attempts);
                    let _ = db::defer_notification(&state.db, n.id, &db::after_secs_str(backoff)).await;
                    skip_users.insert(n.user_id);
                    tracing::warn!(
                        "уведомление {} не доставлено (попытка {}), повтор через {backoff} с: {e}",
                        n.id,
                        n.attempts + 1
                    );
                }
            }
        }
    }
}

/// Фоновый цикл повторной доставки уведомлений (не зависит от планировщика проверок).
pub async fn notification_loop(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        flush_notifications(&state).await;
    }
}

const HISTORY_RETENTION_DAYS: i64 = 30;
const INCIDENT_RETENTION_DAYS: i64 = 90;

/// Ежечасовное обслуживание: удаляет старые записи истории и закрытые инциденты.
pub async fn maintenance_loop(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let hist_before = db::after_secs_str(-HISTORY_RETENTION_DAYS * 86400);
        let inc_before = db::after_secs_str(-INCIDENT_RETENTION_DAYS * 86400);
        match db::prune_history(&state.db, &hist_before).await {
            Ok(n) if n > 0 => tracing::debug!("удалено записей истории: {n}"),
            Err(e) => tracing::error!("прунинг истории: {e:#}"),
            _ => {}
        }
        match db::prune_incidents(&state.db, &inc_before).await {
            Ok(n) if n > 0 => tracing::debug!("удалено инцидентов: {n}"),
            Err(e) => tracing::error!("прунинг инцидентов: {e:#}"),
            _ => {}
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
            reminder_secs: 0,
            last_reminder_at: None,
            latency_threshold_ms: 0,
            slow_notified: false,
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

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(notification_backoff(0), 30);
        assert_eq!(notification_backoff(1), 60);
        assert_eq!(notification_backoff(2), 120);
        assert_eq!(notification_backoff(3), 240);
        assert_eq!(notification_backoff(4), 300);
        assert_eq!(notification_backoff(100), 300);
    }

    #[test]
    fn render_transition_mentions_url_and_reason() {
        let s = site("up", 0);
        let down = render_transition(&s, Transition::WentDown, "HTTP 500");
        assert!(down.contains("недоступен"));
        assert!(down.contains("example.com"));
        assert!(down.contains("HTTP 500"));

        let mut s = site("down", 1);
        s.down_since = Some(db::now_str());
        let up = render_transition(&s, Transition::Recovered, "");
        assert!(up.contains("снова доступен"));
        assert!(up.contains("Простой:"));
    }

    #[test]
    fn reminder_due_logic() {
        let now = db::now_str();
        let old = db::after_secs_str(-7200);
        let recent = db::after_secs_str(-60);
        assert!(!reminder_due(0, Some(&old), Some(&old), &now));
        assert!(!reminder_due(3600, None, None, &now));
        assert!(reminder_due(3600, None, Some(&old), &now));
        assert!(!reminder_due(3600, None, Some(&recent), &now));
        assert!(!reminder_due(3600, Some(&recent), Some(&old), &now));
        assert!(reminder_due(3600, Some(&old), Some(&recent), &now));
    }

    #[test]
    fn slow_decision_logic() {
        assert_eq!(slow_decision(true, 9000, 0, false), None);
        assert_eq!(slow_decision(true, 100, 500, false), None);
        assert_eq!(slow_decision(true, 900, 500, false), Some(true));
        assert_eq!(slow_decision(true, 900, 500, true), None);
        assert_eq!(slow_decision(true, 100, 500, true), Some(false));
        assert_eq!(slow_decision(false, 0, 500, true), Some(false));
        assert_eq!(slow_decision(false, 0, 500, false), None);
    }
}