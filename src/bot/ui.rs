use teloxide::payloads::{EditMessageTextSetters, SendMessageSetters};
use teloxide::prelude::Requester;
use teloxide::types::{ChatId, InlineKeyboardButton, InlineKeyboardMarkup, MessageId, ParseMode};

use crate::db::{self, Site};

use super::state::TBot;

pub const INTERVALS: [(i64, &str); 6] = [
    (300, "5 мин"),
    (900, "15 мин"),
    (1800, "30 мин"),
    (3600, "1 час"),
    (21600, "6 часов"),
    (86400, "24 часа"),
];

pub const REMINDERS: [(i64, &str); 5] = [
    (0, "Выкл"),
    (3600, "1 час"),
    (21600, "6 часов"),
    (43200, "12 часов"),
    (86400, "24 часа"),
];

pub const LATENCY_THRESHOLDS: [(i64, &str); 6] = [
    (0, "Выкл"),
    (300, "300 мс"),
    (500, "500 мс"),
    (1000, "1 с"),
    (2000, "2 с"),
    (5000, "5 с"),
];

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn fmt_dt(ts: &str) -> String {
    db::parse_ts(ts)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%d.%m.%Y %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}

pub fn fmt_interval(secs: i64) -> String {
    match secs {
        300 => "каждые 5 минут".into(),
        900 => "каждые 15 минут".into(),
        1800 => "каждые 30 минут".into(),
        3600 => "каждый час".into(),
        21600 => "каждые 6 часов".into(),
        86400 => "каждые 24 часа".into(),
        _ if secs % 3600 == 0 => format!("каждые {} ч", secs / 3600),
        _ if secs % 60 == 0 => format!("каждые {} мин", secs / 60),
        _ => format!("каждые {secs} сек"),
    }
}

pub fn fmt_reminder(secs: i64) -> String {
    match secs {
        s if s <= 0 => "выключены".into(),
        3600 => "каждый час".into(),
        21600 => "каждые 6 часов".into(),
        43200 => "каждые 12 часов".into(),
        86400 => "каждые 24 часа".into(),
        _ => format!("каждые {secs} с"),
    }
}

pub fn fmt_latency_threshold(ms: i64) -> String {
    if ms <= 0 {
        "выключен".into()
    } else if ms < 1000 {
        format!("{ms} мс")
    } else {
        format!("{} с", ms / 1000)
    }
}

pub fn fmt_secs(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs} сек")
    } else if secs < 3600 {
        format!("{} мин", secs / 60)
    } else if secs < 86400 {
        format!("{} ч {} мин", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{} дн {} ч", secs / 86400, (secs % 86400) / 3600)
    }
}

pub fn fmt_ago(ts: &str) -> String {
    let Some(dt) = db::parse_ts(ts) else {
        return "неизвестно".into();
    };
    fmt_secs((chrono::Utc::now() - dt).num_seconds())
}

/// Длительность между start и end (если end нет — по текущий момент).
pub fn fmt_duration(start: &str, end: Option<&str>) -> String {
    let Some(s) = db::parse_ts(start) else {
        return "неизвестно".into();
    };
    let e = end.and_then(db::parse_ts).unwrap_or_else(chrono::Utc::now);
    fmt_secs((e - s).num_seconds())
}

/// Доля успешных проверок в процентах; None если проверок не было.
pub fn uptime_percent(total: i64, up: i64) -> Option<String> {
    if total <= 0 {
        return None;
    }
    Some(format!("{:.1}%", (up as f64 / total as f64) * 100.0))
}

pub fn status_emoji(site: &Site) -> &'static str {
    if !site.enabled {
        "⏸"
    } else {
        match site.status.as_str() {
            "up" => "🟢",
            "down" => "🔴",
            _ => "⚪",
        }
    }
}

pub fn status_text(site: &Site) -> &'static str {
    if !site.enabled {
        "проверки отключены"
    } else {
        match site.status.as_str() {
            "up" => "доступен",
            "down" => "недоступен",
            _ => "ещё не проверялся",
        }
    }
}

pub fn short_url(url: &str) -> String {
    let label = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned());
    if label.chars().count() > 30 {
        let truncated: String = label.chars().take(29).collect();
        format!("{truncated}…")
    } else {
        label
    }
}

fn btn(text: impl Into<String>, data: impl Into<String>) -> InlineKeyboardButton {
    InlineKeyboardButton::callback(text, data)
}

pub fn main_menu_kb(is_admin: bool, impersonating: bool) -> InlineKeyboardMarkup {
    let mut rows = vec![
        vec![btn("📋 Мои сайты", "sites"), btn("➕ Добавить сайт", "add")],
        vec![btn("❓ Помощь", "help")],
    ];
    if impersonating {
        rows.push(vec![btn("🚪 Выйти из режима пользователя", "exit")]);
    }
    if is_admin {
        rows.push(vec![btn("🛠 Админ-панель", "adm")]);
    }
    InlineKeyboardMarkup::new(rows)
}

pub fn cancel_kb() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![btn("❌ Отмена", "cancel")]])
}

pub fn interval_kb(prefix: &str, back_data: &str) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = INTERVALS
        .chunks(3)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(secs, label)| btn(*label, format!("{prefix}{secs}")))
                .collect()
        })
        .collect();
    rows.push(vec![btn("⬅️ Назад", back_data)]);
    InlineKeyboardMarkup::new(rows)
}

pub fn reminder_kb(site_id: i64) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = REMINDERS
        .chunks(3)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(secs, label)| btn(*label, format!("remset:{site_id}:{secs}")))
                .collect()
        })
        .collect();
    rows.push(vec![btn("⬅️ Назад", format!("site:{site_id}"))]);
    InlineKeyboardMarkup::new(rows)
}

pub fn latency_kb(site_id: i64) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = LATENCY_THRESHOLDS
        .chunks(3)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(ms, label)| btn(*label, format!("latset:{site_id}:{ms}")))
                .collect()
        })
        .collect();
    rows.push(vec![btn("⬅️ Назад", format!("site:{site_id}"))]);
    InlineKeyboardMarkup::new(rows)
}

pub fn site_card_kb(site: &Site) -> InlineKeyboardMarkup {
    let id = site.id;
    let toggle = if site.enabled {
        btn("⏸ Отключить", format!("tog:{id}"))
    } else {
        btn("▶️ Включить", format!("tog:{id}"))
    };
    InlineKeyboardMarkup::new(vec![
        vec![btn("🔄 Проверить сейчас", format!("chk:{id}"))],
        vec![
            btn("✏️ URL", format!("eurl:{id}")),
            btn("📝 Текст", format!("etxt:{id}")),
            btn("⏱ Интервал", format!("eint:{id}")),
        ],
        vec![
            btn("📜 История", format!("hist:{id}")),
            btn("🔔 Напоминания", format!("rem:{id}")),
            btn("🐌 Задержка", format!("lat:{id}")),
        ],
        vec![toggle, btn("🗑 Удалить", format!("del:{id}"))],
        vec![btn("⬅️ К списку", "sites")],
    ])
}

pub fn site_list_kb(sites: &[Site], page: usize, pages: usize) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = sites
        .iter()
        .map(|s| {
            vec![btn(
                format!("{} {} · {}", status_emoji(s), short_url(&s.url), {
                    let label = INTERVALS
                        .iter()
                        .find(|(secs, _)| *secs == s.interval_secs)
                        .map(|(_, l)| *l)
                        .unwrap_or("другой");
                    label
                }),
                format!("site:{}", s.id),
            )]
        })
        .collect();
    if pages > 1 {
        let mut nav = Vec::new();
        if page > 0 {
            nav.push(btn("◀️", format!("sites:{}", page - 1)));
        }
        nav.push(btn(format!("{}/{}", page + 1, pages), "noop"));
        if page + 1 < pages {
            nav.push(btn("▶️", format!("sites:{}", page + 1)));
        }
        rows.push(nav);
    }
    rows.push(vec![btn("➕ Добавить сайт", "add"), btn("🏠 Меню", "menu")]);
    InlineKeyboardMarkup::new(rows)
}

pub fn back_kb(data: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![btn("⬅️ Назад", data)]])
}

/// Редактирует сообщение с клавиатурой; если редактирование невозможно — шлёт новое.
pub async fn show(bot: &TBot, chat: i64, msg_id: Option<MessageId>, text: String, kb: InlineKeyboardMarkup) {
    if let Some(msg_id) = msg_id {
        match bot
            .edit_message_text(ChatId(chat), msg_id, text.clone())
            .parse_mode(ParseMode::Html)
            .reply_markup(kb.clone())
            .await
        {
            Ok(_) => return,
            Err(e) => {
                if e.to_string().contains("message is not modified") {
                    return;
                }
                tracing::debug!("edit_message_text failed: {e}");
            }
        }
    }
    if let Err(e) = bot
        .send_message(ChatId(chat), text)
        .parse_mode(ParseMode::Html)
        .reply_markup(kb)
        .await
    {
        tracing::warn!("send_message to {chat} failed: {e}");
    }
}

pub async fn send(bot: &TBot, chat: i64, text: String, kb: Option<InlineKeyboardMarkup>) {
    let mut req = bot.send_message(ChatId(chat), text).parse_mode(ParseMode::Html);
    if let Some(kb) = kb {
        req = req.reply_markup(kb);
    }
    if let Err(e) = req.await {
        tracing::warn!("send_message to {chat} failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_html() {
        assert_eq!(esc("a<b>&c"), "a&lt;b&gt;&amp;c");
        assert_eq!(esc("plain"), "plain");
    }

    #[test]
    fn interval_labels() {
        assert_eq!(fmt_interval(300), "каждые 5 минут");
        assert_eq!(fmt_interval(3600), "каждый час");
        assert_eq!(fmt_interval(7200), "каждые 2 ч");
        assert_eq!(fmt_interval(120), "каждые 2 мин");
        assert_eq!(fmt_interval(45), "каждые 45 сек");
    }

    #[test]
    fn secs_formatting() {
        assert_eq!(fmt_secs(30), "30 сек");
        assert_eq!(fmt_secs(90), "1 мин");
        assert_eq!(fmt_secs(3660), "1 ч 1 мин");
        assert_eq!(fmt_secs(90000), "1 дн 1 ч");
        assert_eq!(fmt_secs(-5), "0 сек");
    }

    #[test]
    fn duration_between() {
        let start = "2026-11-09T10:00:00.000Z";
        let end = "2026-11-09T11:30:00.000Z";
        assert_eq!(fmt_duration(start, Some(end)), "1 ч 30 мин");
        assert_eq!(fmt_duration("invalid", Some(end)), "неизвестно");
    }

    #[test]
    fn uptime_pct() {
        assert_eq!(uptime_percent(0, 0), None);
        assert_eq!(uptime_percent(4, 3).unwrap(), "75.0%");
        assert_eq!(uptime_percent(2, 2).unwrap(), "100.0%");
    }

    #[test]
    fn short_url_host() {
        assert_eq!(short_url("https://example.com/path"), "example.com");
        let long = format!("https://{}.com/", "a".repeat(40));
        let s = short_url(&long);
        assert!(s.chars().count() <= 30);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn reminder_and_latency_labels() {
        assert_eq!(fmt_reminder(0), "выключены");
        assert_eq!(fmt_reminder(3600), "каждый час");
        assert_eq!(fmt_latency_threshold(0), "выключен");
        assert_eq!(fmt_latency_threshold(500), "500 мс");
        assert_eq!(fmt_latency_threshold(2000), "2 с");
    }
}