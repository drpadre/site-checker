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

pub fn fmt_ago(ts: &str) -> String {
    let Some(dt) = db::parse_ts(ts) else {
        return "неизвестно".into();
    };
    let secs = (chrono::Utc::now() - dt).num_seconds().max(0);
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