use anyhow::Result;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, MessageId};

use crate::bot::state::{AppState, Dialog, TBot};
use crate::bot::ui;
use crate::bot::user;
use crate::db;

const PAGE_SIZE: i64 = 8;

fn btn(text: impl Into<String>, data: impl Into<String>) -> InlineKeyboardButton {
    InlineKeyboardButton::callback(text, data)
}

fn parse_id(data: &str, prefix: &str) -> Option<i64> {
    data.strip_prefix(prefix)?.parse().ok()
}

pub async fn panel(bot: &TBot, state: &AppState, chat: i64, msg_id: Option<MessageId>) -> Result<()> {
    let (users, sites, down) = db::global_stats(&state.db).await?;
    let text = format!(
        "🛠 <b>Админ-панель</b>\n\nПользователей: {users}\nСайтов: {sites} · недоступно: {down}\n\nВыберите действие:"
    );
    let kb = InlineKeyboardMarkup::new(vec![
        vec![btn("👥 Пользователи", "ulist:0")],
        vec![btn("➕ Добавить пользователя", "uadd")],
        vec![btn("🏠 Меню", "menu")],
    ]);
    ui::show(bot, chat, msg_id, text, kb).await;
    Ok(())
}

pub async fn user_list(bot: &TBot, state: &AppState, chat: i64, msg_id: Option<MessageId>, page: i64) -> Result<()> {
    let count = db::count_users(&state.db).await?;
    let pages = (count + PAGE_SIZE - 1) / PAGE_SIZE.max(1);
    let pages = pages.max(1);
    let page = page.clamp(0, pages - 1);
    let users = db::list_users(&state.db, PAGE_SIZE, page * PAGE_SIZE).await?;

    let mut rows: Vec<Vec<InlineKeyboardButton>> = users
        .iter()
        .map(|u| {
            let mut label = u.display();
            if u.is_banned {
                label = format!("⛔ {label}");
            } else if u.is_blocked {
                label = format!("🚫 {label}");
            }
            if label.chars().count() > 48 {
                label = label.chars().take(47).collect::<String>() + "…";
            }
            vec![btn(label, format!("ucard:{}", u.id))]
        })
        .collect();
    if pages > 1 {
        let mut nav = Vec::new();
        if page > 0 {
            nav.push(btn("◀️", format!("ulist:{}", page - 1)));
        }
        nav.push(btn(format!("{}/{}", page + 1, pages), "noop"));
        if page + 1 < pages {
            nav.push(btn("▶️", format!("ulist:{}", page + 1)));
        }
        rows.push(nav);
    }
    rows.push(vec![btn("⬅️ Админ-панель", "adm")]);

    let text = format!("👥 <b>Пользователи</b> — всего {count}\n\n⛔ — забанен · 🚫 — заблокировал бота (уведомления не доходят)");
    ui::show(bot, chat, msg_id, text, InlineKeyboardMarkup::new(rows)).await;
    Ok(())
}

pub async fn user_card(bot: &TBot, state: &AppState, chat: i64, msg_id: Option<MessageId>, user_id: i64) -> Result<()> {
    let Some(u) = db::get_user(&state.db, user_id).await? else {
        ui::show(bot, chat, msg_id, "Пользователь не найден.".into(), ui::back_kb("ulist:0")).await;
        return Ok(());
    };
    let (total, down) = db::site_stats(&state.db, user_id).await?;
    let username = u.username.as_deref().map(|x| format!("@{x}")).unwrap_or_else(|| "—".into());
    let mut text = format!(
        "👤 <b>{}</b> {}\n\n🆔 ID: <code>{}</code>\n📅 Зарегистрирован: {}\n🌐 Сайтов: {total} · недоступно: {down}\n🚫 Бот заблокирован: {}\n⛔ Бан: {}",
        ui::esc(&u.first_name),
        ui::esc(&username),
        u.id,
        ui::fmt_dt(&u.created_at),
        if u.is_blocked { "да" } else { "нет" },
        if u.is_banned { "да — доступ закрыт, сайты не проверяются" } else { "нет" },
    );
    if state.cfg.is_admin(user_id) {
        text += "\n🛠 Администратор";
    }
    let ban_btn = if u.is_banned {
        btn("✅ Разбанить", format!("unban:{user_id}"))
    } else {
        btn("🚫 Забанить", format!("ban:{user_id}"))
    };
    let kb = InlineKeyboardMarkup::new(vec![
        vec![btn("👁 Использовать бота от его имени", format!("act:{user_id}"))],
        vec![ban_btn, btn("🗑 Удалить", format!("udel:{user_id}"))],
        vec![btn("⬅️ К списку", "ulist:0")],
    ]);
    ui::show(bot, chat, msg_id, text, kb).await;
    Ok(())
}

pub async fn handle(bot: &TBot, state: &AppState, sender: i64, chat: i64, msg_id: MessageId, data: &str) -> Result<()> {
    let mid = Some(msg_id);
    if !state.cfg.is_admin(sender) {
        ui::show(bot, chat, mid, "⛔ Доступно только администратору.".into(), ui::back_kb("menu")).await;
        return Ok(());
    }

    if data == "adm" {
        return panel(bot, state, chat, mid).await;
    }
    if let Some(p) = data.strip_prefix("ulist:") {
        return user_list(bot, state, chat, mid, p.parse().unwrap_or(0)).await;
    }
    if let Some(id) = parse_id(data, "ucard:") {
        return user_card(bot, state, chat, mid, id).await;
    }
    if data == "uadd" {
        state.dialogs.lock().await.insert(sender, Dialog::AdminAddUser);
        ui::show(
            bot,
            chat,
            mid,
            "➕ <b>Добавление пользователя</b>\n\nПришлите Telegram ID пользователя (число).\n\nВы сможете управлять его сайтами от его имени. Чтобы он сам получал уведомления, он должен открыть диалог с ботом и нажать /start.".into(),
            ui::cancel_kb(),
        )
        .await;
        return Ok(());
    }
    if let Some(id) = parse_id(data, "udel:") {
        let Some(u) = db::get_user(&state.db, id).await? else {
            return user_list(bot, state, chat, mid, 0).await;
        };
        let (total, _) = db::site_stats(&state.db, id).await?;
        let kb = InlineKeyboardMarkup::new(vec![vec![
            btn("🗑 Да, удалить", format!("udely:{id}")),
            btn("⬅️ Отмена", format!("ucard:{id}")),
        ]]);
        let mut text = format!(
            "🗑 Удалить пользователя <b>{}</b> и все его сайты ({} шт.)?",
            ui::esc(&u.display()),
            total,
        );
        if id == sender {
            text += "\n\n⚠️ Это ваш аккаунт. После удаления вы зарегистрируетесь заново по /start.";
        }
        ui::show(bot, chat, mid, text, kb).await;
        return Ok(());
    }
    if let Some(id) = parse_id(data, "udely:") {
        db::delete_user(&state.db, id).await?;
        state.acting_as.lock().await.retain(|_, target| *target != id);
        return user_list(bot, state, chat, mid, 0).await;
    }
    if let Some(id) = parse_id(data, "ban:") {
        if id == sender {
            ui::show(bot, chat, mid, "⚠️ Нельзя забанить самого себя.".into(), ui::back_kb(&format!("ucard:{id}"))).await;
            return Ok(());
        }
        db::set_banned(&state.db, id, true).await?;
        return user_card(bot, state, chat, mid, id).await;
    }
    if let Some(id) = parse_id(data, "unban:") {
        db::set_banned(&state.db, id, false).await?;
        return user_card(bot, state, chat, mid, id).await;
    }
    if let Some(id) = parse_id(data, "act:") {
        if db::get_user(&state.db, id).await?.is_none() {
            ui::show(bot, chat, mid, "Пользователь не найден.".into(), ui::back_kb("ulist:0")).await;
            return Ok(());
        }
        state.acting_as.lock().await.insert(sender, id);
        return user::main_menu(bot, state, chat, mid, sender).await;
    }
    if data == "exit" {
        state.acting_as.lock().await.remove(&sender);
        return user::main_menu(bot, state, chat, mid, sender).await;
    }

    panel(bot, state, chat, mid).await
}