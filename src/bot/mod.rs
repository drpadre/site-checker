pub mod admin;
pub mod state;
pub mod ui;
pub mod user;

use std::sync::Arc;

use teloxide::payloads::AnswerCallbackQuerySetters;
use teloxide::prelude::Requester;
use teloxide::types::{CallbackQuery, ChatMemberStatus, ChatMemberUpdated, MaybeInaccessibleMessage, Message};

use crate::db;

use state::{AppState, TBot};

pub async fn on_message(bot: TBot, msg: Message, state: Arc<AppState>) -> anyhow::Result<()> {
    if let Err(e) = handle_message(&bot, &msg, &state).await {
        tracing::error!("обработка сообщения: {e:#}");
    }
    Ok(())
}

async fn handle_message(bot: &TBot, msg: &Message, state: &AppState) -> anyhow::Result<()> {
    if !msg.chat.is_private() {
        return Ok(());
    }
    let Some(from) = &msg.from else {
        return Ok(());
    };
    let sender = from.id.0 as i64;
    let chat = msg.chat.id.0;

    if let Some(u) = db::get_user(&state.db, sender).await? {
        if u.is_banned {
            ui::send(bot, chat, "⛔ Вы заблокированы администратором. Доступ к боту закрыт.".into(), None).await;
            return Ok(());
        }
    }

    let text = msg.text().unwrap_or("").trim();
    let command = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("");

    match command {
        "/start" => {
            db::upsert_user(&state.db, sender, from.username.as_deref(), &from.first_name).await?;
            state.dialogs.lock().await.remove(&sender);
            user::main_menu(bot, state, chat, None, sender).await?;
        }
        "/menu" => {
            user::main_menu(bot, state, chat, None, sender).await?;
        }
        "/help" => {
            user::help(bot, chat, None).await?;
        }
        "/admin" => {
            if state.cfg.is_admin(sender) {
                admin::panel(bot, state, chat, None).await?;
            } else {
                ui::send(bot, chat, "⛔ Команда доступна только администратору.".into(), None).await;
            }
        }
        "/cancel" => {
            state.dialogs.lock().await.remove(&sender);
            user::main_menu(bot, state, chat, None, sender).await?;
        }
        _ => {
            user::handle_text(bot, state, sender, chat, text).await?;
        }
    }
    Ok(())
}

pub async fn on_callback(bot: TBot, q: CallbackQuery, state: Arc<AppState>) -> anyhow::Result<()> {
    let data = q.data.clone().unwrap_or_default();
    if let Err(e) = handle_callback(&bot, &q, &state, &data).await {
        tracing::error!("обработка callback {data:?}: {e:#}");
    }
    Ok(())
}

async fn handle_callback(bot: &TBot, q: &CallbackQuery, state: &AppState, data: &str) -> anyhow::Result<()> {
    let sender = q.from.id.0 as i64;
    let (chat, msg_id) = match &q.message {
        Some(MaybeInaccessibleMessage::Regular(m)) => (m.chat.id.0, m.id),
        Some(MaybeInaccessibleMessage::Inaccessible(m)) => (m.chat.id.0, m.message_id),
        None => return Ok(()),
    };

    if let Some(u) = db::get_user(&state.db, sender).await? {
        if u.is_banned {
            let _ = bot
                .answer_callback_query(q.id.clone())
                .text("⛔ Вы заблокированы администратором.")
                .show_alert(true)
                .await;
            return Ok(());
        }
    }
    let _ = bot.answer_callback_query(q.id.clone()).await;

    if !q.from.is_bot {
        db::upsert_user(&state.db, sender, q.from.username.as_deref(), &q.from.first_name).await?;
    }

    let is_admin_route = data == "adm"
        || data == "exit"
        || ["ulist:", "ucard:", "uadd", "udel:", "udely:", "act:", "ban:", "unban:"]
            .iter()
            .any(|p| data.starts_with(p));

    let result = if is_admin_route {
        admin::handle(bot, state, sender, chat, msg_id, data).await
    } else {
        user::handle(bot, state, sender, chat, msg_id, data).await
    };
    if let Err(e) = result {
        tracing::error!("callback {data:?}: {e:#}");
        ui::send(bot, chat, format!("⚠️ Ошибка: {e:#}"), None).await;
    }
    Ok(())
}

pub async fn on_chat_member(upd: ChatMemberUpdated, state: Arc<AppState>) -> anyhow::Result<()> {
    if !upd.chat.is_private() {
        return Ok(());
    }
    let id = upd.chat.id.0;
    let blocked = matches!(upd.new_chat_member.status(), ChatMemberStatus::Left | ChatMemberStatus::Banned);
    match db::get_user(&state.db, id).await {
        Ok(Some(_)) => {
            if let Err(e) = db::set_blocked(&state.db, id, blocked).await {
                tracing::error!("set_blocked {id}: {e:#}");
            }
        }
        Ok(None) => {}
        Err(e) => tracing::error!("get_user {id}: {e:#}"),
    }
    Ok(())
}