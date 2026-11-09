use anyhow::Result;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, MessageId};

use crate::bot::state::{AppState, Dialog, TBot};
use crate::bot::ui;
use crate::checker;
use crate::db::{self, Site};
use crate::monitor;

const PAGE_SIZE: usize = 8;

fn parse_id(data: &str, prefix: &str) -> Option<i64> {
    data.strip_prefix(prefix)?.parse().ok()
}

async fn banner(state: &AppState, sender: i64) -> String {
    match state.impersonation_target(sender).await {
        Some(target) => {
            let name = db::get_user(&state.db, target)
                .await
                .ok()
                .flatten()
                .map(|u| u.display())
                .unwrap_or_else(|| format!("id {target}"));
            format!("👁 <b>Режим администратора:</b> {}\n\n", ui::esc(&name))
        }
        None => String::new(),
    }
}

pub async fn main_menu(bot: &TBot, state: &AppState, chat: i64, msg_id: Option<MessageId>, sender: i64) -> Result<()> {
    let eff = state.effective_user(sender).await;
    let (total, down) = db::site_stats(&state.db, eff).await?;
    let impersonating = state.impersonation_target(sender).await.is_some();
    let text = format!(
        "{}🌐 <b>Site Checker</b>\n\nСайтов: {total} · недоступно: {down}\n\nЯ слежу за доступностью сайтов и уведомляю вас, когда сайт недоступен, либо когда вновь становится доступен.\n\nВыберите действие:",
        banner(state, sender).await,
    );
    ui::show(bot, chat, msg_id, text, ui::main_menu_kb(state.cfg.is_admin(sender), impersonating)).await;
    Ok(())
}

pub async fn help(bot: &TBot, chat: i64, msg_id: Option<MessageId>) -> Result<()> {
    let text = "❓ <b>Помощь</b>\n\n\
        Бот проверяет сайты по расписанию и присылает уведомления в этот чат:\n\
        🔴 сайт недоступен — ошибка сети, таймаут, HTTP не 2xx или нужный текст не найден;\n\
        🟢 сайт снова заработал (с указанием времени простоя).\n\n\
        <b>Добавление сайта:</b> URL → текст для поиска (или «-», чтобы пропустить) → интервал проверок.\n\n\
        <b>Текст для поиска</b> — подстрока, которая обязана присутствовать на странице; регистр учитывается. \
        Пригодится, чтобы ловить страницы-заглушки, отдающие код 200.\n\n\
        Каждый пользователь видит только свои сайты; уведомления приходят только владельцу.\n\n\
        Команды: /menu — главное меню, /cancel — отменить ввод, /help — эта справка.";
    ui::show(bot, chat, msg_id, text.into(), ui::back_kb("menu")).await;
    Ok(())
}

async fn owned_site(state: &AppState, eff: i64, id: i64) -> Result<Option<Site>> {
    Ok(db::get_site(&state.db, id).await?.filter(|s| s.user_id == eff))
}

async fn deny(bot: &TBot, chat: i64, msg_id: Option<MessageId>) -> Result<()> {
    ui::show(
        bot,
        chat,
        msg_id,
        "⛔ Сайт не найден или принадлежит другому пользователю.".into(),
        ui::back_kb("menu"),
    )
    .await;
    Ok(())
}

pub async fn site_list(bot: &TBot, state: &AppState, chat: i64, msg_id: Option<MessageId>, sender: i64, page: usize) -> Result<()> {
    let eff = state.effective_user(sender).await;
    let sites = db::list_sites(&state.db, eff).await?;
    let pages = sites.len().div_ceil(PAGE_SIZE).max(1);
    let page = page.min(pages - 1);
    let slice = &sites[page * PAGE_SIZE..(page * PAGE_SIZE + PAGE_SIZE).min(sites.len())];
    let text = format!(
        "{}📋 <b>Сайты</b> — всего {}\n\n🟢 доступен · 🔴 недоступен · ⚪ не проверялся · ⏸ отключён",
        banner(state, sender).await,
        sites.len(),
    );
    ui::show(bot, chat, msg_id, text, ui::site_list_kb(slice, page, pages)).await;
    Ok(())
}

fn site_card_text(site: &Site) -> String {
    let mut text = format!(
        "🌐 <b>{}</b>\n\n{} Статус: {}\n🔎 Текст: {}\n⏱ Интервал: {}",
        ui::esc(&site.url),
        ui::status_emoji(site),
        ui::status_text(site),
        match &site.search_text {
            Some(t) => format!("«{}»", ui::esc(t)),
            None => "не задан (только HTTP-статус)".into(),
        },
        ui::fmt_interval(site.interval_secs),
    );
    text += &format!(
        "\n🕐 Последняя проверка: {}",
        site.last_check_at.as_deref().map(ui::fmt_dt).unwrap_or_else(|| "ещё не было".into()),
    );
    if site.enabled {
        text += &format!("\n⏭ Следующая проверка: {}", ui::fmt_dt(&site.next_check_at));
    }
    if let Some(err) = &site.last_error {
        text += &format!("\n⚠️ Последняя ошибка: {}", ui::esc(err));
    }
    if site.status == "down" {
        if let Some(since) = &site.down_since {
            text += &format!("\n⏳ Недоступен с: {} (простой: {})", ui::fmt_dt(since), ui::fmt_ago(since));
        }
    }
    if site.fail_count > 0 {
        text += &format!("\nНеудачных проверок подряд: {}", site.fail_count);
    }
    text
}

pub async fn site_card(bot: &TBot, state: &AppState, chat: i64, msg_id: Option<MessageId>, sender: i64, site_id: i64) -> Result<()> {
    let eff = state.effective_user(sender).await;
    let Some(site) = owned_site(state, eff, site_id).await? else {
        return deny(bot, chat, msg_id).await;
    };
    ui::show(bot, chat, msg_id, site_card_text(&site), ui::site_card_kb(&site)).await;
    Ok(())
}

pub async fn check_now(bot: &TBot, state: &AppState, chat: i64, msg_id: Option<MessageId>, sender: i64, site_id: i64) -> Result<()> {
    let eff = state.effective_user(sender).await;
    let Some(site) = owned_site(state, eff, site_id).await? else {
        return deny(bot, chat, msg_id).await;
    };
    ui::show(bot, chat, msg_id, format!("⏳ Проверяю {}…", ui::esc(&site.url)), ui::site_card_kb(&site)).await;

    if !state.in_flight.lock().await.insert(site.id) {
        ui::show(bot, chat, msg_id, "⏳ Проверка этого сайта уже выполняется. Результат придёт автоматически.".into(), ui::site_card_kb(&site)).await;
        return Ok(());
    }
    let result = checker::check(&state.http, &site.url, site.search_text.as_deref()).await;
    let detail = result.detail.clone();
    let elapsed_ms = result.elapsed_ms;
    let processed = monitor::process_check(state, &site, result, Some(sender)).await;
    state.in_flight.lock().await.remove(&site.id);
    let transition = processed?;

    let updated = db::get_site(&state.db, site.id).await?.unwrap_or(site);
    let mut text = format!(
        "{} <b>Проверка выполнена</b>\n\n{}\n{}\n⏱ {} мс\nСтатус: {}",
        ui::status_emoji(&updated),
        ui::esc(&updated.url),
        ui::esc(&detail),
        elapsed_ms,
        ui::status_text(&updated),
    );
    if transition.is_some() && sender != updated.user_id {
        text += "\n\nВладелец сайта получил уведомление.";
    }
    ui::show(bot, chat, msg_id, text, ui::site_card_kb(&updated)).await;
    Ok(())
}

async fn start_add(bot: &TBot, state: &AppState, chat: i64, msg_id: Option<MessageId>, sender: i64) -> Result<()> {
    let eff = state.effective_user(sender).await;
    state.dialogs.lock().await.insert(sender, Dialog::AddUrl { for_user: eff });
    ui::show(
        bot,
        chat,
        msg_id,
        "➕ <b>Добавление сайта</b>\n\nШаг 1/3. Пришлите URL сайта, например <code>https://example.com</code>\n\nЕсли схема не указана — подставится https://".into(),
        ui::cancel_kb(),
    )
    .await;
    Ok(())
}

pub async fn handle_text(bot: &TBot, state: &AppState, sender: i64, chat: i64, text: &str) -> Result<()> {
    let dialog = state.dialogs.lock().await.get(&sender).cloned();
    let Some(dialog) = dialog else {
        ui::send(bot, chat, "Не понимаю. Нажмите /menu, чтобы открыть главное меню.".into(), None).await;
        return Ok(());
    };
    match dialog {
        Dialog::AddUrl { for_user } => match checker::validate_url(text).await {
            Err(e) => {
                ui::send(bot, chat, format!("⚠️ {e:#}\n\nПопробуйте ещё раз или нажмите «Отмена»."), Some(ui::cancel_kb())).await;
            }
            Ok(url) => {
                state.dialogs.lock().await.insert(sender, Dialog::AddText { for_user, url: url.clone() });
                ui::send(
                    bot,
                    chat,
                    format!(
                        "✅ URL принят: {}\n\nШаг 2/3. Пришлите текст, который должен присутствовать на странице.\n\nПришлите «-», чтобы проверять только HTTP-статус.",
                        ui::esc(&url),
                    ),
                    Some(ui::cancel_kb()),
                )
                .await;
            }
        },
        Dialog::AddText { for_user, url } => {
            let trimmed = text.trim();
            let search = if trimmed == "-" {
                None
            } else if trimmed.is_empty() {
                ui::send(bot, chat, "⚠️ Пустой текст. Пришлите строку для поиска или «-».".into(), Some(ui::cancel_kb())).await;
                return Ok(());
            } else {
                Some(trimmed.to_owned())
            };
            state.dialogs.lock().await.insert(
                sender,
                Dialog::AddInterval { for_user, url: url.clone(), text: search.clone() },
            );
            ui::send(
                bot,
                chat,
                format!(
                    "Шаг 3/3. Выберите интервал проверок:\n\nURL: {}\nТекст: {}",
                    ui::esc(&url),
                    search.as_deref().map(ui::esc).unwrap_or_else(|| "не задан".into()),
                ),
                Some(ui::interval_kb("ai:", "cancel")),
            )
            .await;
        }
        Dialog::AddInterval { .. } => {
            ui::send(bot, chat, "Выберите интервал кнопкой выше или нажмите «Отмена».".into(), Some(ui::interval_kb("ai:", "cancel"))).await;
        }
        Dialog::EditUrl { site_id } => {
            let eff = state.effective_user(sender).await;
            if owned_site(state, eff, site_id).await?.is_none() {
                state.dialogs.lock().await.remove(&sender);
                return deny(bot, chat, None).await;
            }
            match checker::validate_url(text).await {
                Err(e) => {
                    ui::send(bot, chat, format!("⚠️ {e:#}\n\nПопробуйте ещё раз или нажмите «Отмена»."), Some(ui::cancel_kb())).await;
                }
                Ok(url) => {
                    db::update_site_url(&state.db, site_id, &url).await?;
                    state.dialogs.lock().await.remove(&sender);
                    ui::send(bot, chat, "✅ URL обновлён, статус сброшен. Сайт будет проверен в ближайшем цикле.".into(), None).await;
                    site_card(bot, state, chat, None, sender, site_id).await?;
                }
            }
        }
        Dialog::EditText { site_id } => {
            let eff = state.effective_user(sender).await;
            if owned_site(state, eff, site_id).await?.is_none() {
                state.dialogs.lock().await.remove(&sender);
                return deny(bot, chat, None).await;
            }
            let trimmed = text.trim();
            let search = if trimmed == "-" { None } else { Some(trimmed.to_owned()) };
            if search.as_deref() == Some("") {
                ui::send(bot, chat, "⚠️ Пустой текст. Пришлите строку или «-».".into(), Some(ui::cancel_kb())).await;
                return Ok(());
            }
            db::update_site_text(&state.db, site_id, search.as_deref()).await?;
            state.dialogs.lock().await.remove(&sender);
            ui::send(bot, chat, "✅ Текст обновлён, статус сброшен.".into(), None).await;
            site_card(bot, state, chat, None, sender, site_id).await?;
        }
        Dialog::AdminAddUser => {
            if !state.cfg.is_admin(sender) {
                state.dialogs.lock().await.remove(&sender);
                return Ok(());
            }
            match text.trim().parse::<i64>() {
                Ok(id) if id > 0 => {
                    let created = db::insert_user_placeholder(&state.db, id).await?;
                    state.dialogs.lock().await.remove(&sender);
                    let note = if created {
                        format!("✅ Пользователь {id} добавлен. Чтобы он получал уведомления, он должен сам открыть диалог с ботом (/start).")
                    } else {
                        format!("ℹ️ Пользователь {id} уже зарегистрирован.")
                    };
                    ui::send(bot, chat, note, None).await;
                    super::admin::user_card(bot, state, chat, None, id).await?;
                }
                _ => {
                    ui::send(bot, chat, "⚠️ Нужен числовой Telegram ID (положительное число). Попробуйте ещё раз или нажмите «Отмена».".into(), Some(ui::cancel_kb())).await;
                }
            }
        }
    }
    Ok(())
}

pub async fn handle(bot: &TBot, state: &AppState, sender: i64, chat: i64, msg_id: MessageId, data: &str) -> Result<()> {
    let mid = Some(msg_id);
    if data == "noop" {
        return Ok(());
    }
    if data == "menu" {
        return main_menu(bot, state, chat, mid, sender).await;
    }
    if data == "help" {
        return help(bot, chat, mid).await;
    }
    if data == "sites" {
        return site_list(bot, state, chat, mid, sender, 0).await;
    }
    if let Some(p) = data.strip_prefix("sites:") {
        return site_list(bot, state, chat, mid, sender, p.parse().unwrap_or(0)).await;
    }
    if data == "add" {
        return start_add(bot, state, chat, mid, sender).await;
    }
    if data == "cancel" {
        state.dialogs.lock().await.remove(&sender);
        return main_menu(bot, state, chat, mid, sender).await;
    }
    if data == "exit" {
        state.acting_as.lock().await.remove(&sender);
        return main_menu(bot, state, chat, mid, sender).await;
    }
    if let Some(id) = parse_id(data, "site:") {
        return site_card(bot, state, chat, mid, sender, id).await;
    }
    if let Some(id) = parse_id(data, "chk:") {
        return check_now(bot, state, chat, mid, sender, id).await;
    }

    let eff = state.effective_user(sender).await;
    if let Some(id) = parse_id(data, "eurl:") {
        let Some(site) = owned_site(state, eff, id).await? else {
            return deny(bot, chat, mid).await;
        };
        state.dialogs.lock().await.insert(sender, Dialog::EditUrl { site_id: id });
        ui::show(bot, chat, mid, format!("✏️ <b>Изменение URL</b>\n\nТекущий: {}\n\nПришлите новый URL:", ui::esc(&site.url)), ui::cancel_kb()).await;
        return Ok(());
    }
    if let Some(id) = parse_id(data, "etxt:") {
        let Some(site) = owned_site(state, eff, id).await? else {
            return deny(bot, chat, mid).await;
        };
        state.dialogs.lock().await.insert(sender, Dialog::EditText { site_id: id });
        let current = site.search_text.as_deref().map(ui::esc).unwrap_or_else(|| "не задан".into());
        ui::show(bot, chat, mid, format!("📝 <b>Изменение текста</b>\n\nТекущий: {current}\n\nПришлите новый текст для поиска или «-», чтобы убрать:"), ui::cancel_kb()).await;
        return Ok(());
    }
    if let Some(id) = parse_id(data, "eint:") {
        let Some(site) = owned_site(state, eff, id).await? else {
            return deny(bot, chat, mid).await;
        };
        ui::show(bot, chat, mid, format!("⏱ <b>Интервал проверок</b>\n\nТекущий: {}\n\nВыберите новый:", ui::fmt_interval(site.interval_secs)), ui::interval_kb(&format!("si:{id}:"), &format!("site:{id}"))).await;
        return Ok(());
    }
    if let Some(rest) = data.strip_prefix("si:") {
        let (id, secs) = match rest.split_once(':') {
            Some((id, secs)) => match (id.parse::<i64>(), secs.parse::<i64>()) {
                (Ok(id), Ok(secs)) => (id, secs),
                _ => return Ok(()),
            },
            None => return Ok(()),
        };
        if owned_site(state, eff, id).await?.is_none() {
            return deny(bot, chat, mid).await;
        }
        if !ui::INTERVALS.iter().any(|(s, _)| *s == secs) {
            return Ok(());
        }
        db::update_site_interval(&state.db, id, secs).await?;
        return site_card(bot, state, chat, mid, sender, id).await;
    }
    if let Some(id) = parse_id(data, "tog:") {
        let Some(site) = owned_site(state, eff, id).await? else {
            return deny(bot, chat, mid).await;
        };
        db::set_site_enabled(&state.db, id, !site.enabled).await?;
        return site_card(bot, state, chat, mid, sender, id).await;
    }
    if let Some(id) = parse_id(data, "del:") {
        let Some(site) = owned_site(state, eff, id).await? else {
            return deny(bot, chat, mid).await;
        };
        let kb = InlineKeyboardMarkup::new(vec![vec![
            InlineKeyboardButton::callback("🗑 Да, удалить", format!("dely:{id}")),
            InlineKeyboardButton::callback("⬅️ Отмена", format!("site:{id}")),
        ]]);
        ui::show(bot, chat, mid, format!("🗑 Удалить сайт <b>{}</b>?", ui::esc(&site.url)), kb).await;
        return Ok(());
    }
    if let Some(id) = parse_id(data, "dely:") {
        if owned_site(state, eff, id).await?.is_none() {
            return deny(bot, chat, mid).await;
        }
        db::delete_site(&state.db, id).await?;
        return site_list(bot, state, chat, mid, sender, 0).await;
    }
    if let Some(raw) = data.strip_prefix("ai:") {
        let Ok(secs) = raw.parse::<i64>() else {
            return Ok(());
        };
        let dialog = state.dialogs.lock().await.get(&sender).cloned();
        let Some(Dialog::AddInterval { for_user, url, text }) = dialog else {
            ui::show(bot, chat, mid, "Сессия добавления не найдена. Начните заново: «➕ Добавить сайт».".into(), ui::main_menu_kb(state.cfg.is_admin(sender), false)).await;
            return Ok(());
        };
        if !ui::INTERVALS.iter().any(|(s, _)| *s == secs) {
            return Ok(());
        }
        match db::add_site(&state.db, for_user, &url, text.as_deref(), secs).await {
            Err(e) => {
                tracing::error!("add_site: {e:#}");
                ui::show(bot, chat, mid, "⚠️ Не удалось добавить сайт. Попробуйте ещё раз через «➕ Добавить сайт».".into(), ui::main_menu_kb(state.cfg.is_admin(sender), false)).await;
                state.dialogs.lock().await.remove(&sender);
            }
            Ok(site_id) => {
                state.dialogs.lock().await.remove(&sender);
                return check_now(bot, state, chat, mid, sender, site_id).await;
            }
        }
        return Ok(());
    }

    main_menu(bot, state, chat, mid, sender).await
}