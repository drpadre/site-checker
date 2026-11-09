mod bot;
mod checker;
mod config;
mod db;
mod monitor;

use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide::adaptors::throttle::Limits;
use teloxide::prelude::*;
use teloxide::types::BotCommand;
use tokio::sync::Mutex;

use bot::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = config::Config::from_env()?;
    tracing::info!("администраторы: {:?}", cfg.admin_ids);

    let pool = db::init(&cfg.database_path).await.context("не удалось открыть БД")?;
    let http = checker::build_client(cfg.http_timeout_secs)?;
    let bot = Bot::new(cfg.bot_token.clone()).throttle(Limits::default());

    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        db: pool.clone(),
        http,
        bot: bot.clone(),
        dialogs: Mutex::new(Default::default()),
        acting_as: Mutex::new(Default::default()),
        in_flight: Mutex::new(Default::default()),
    });

    let commands = vec![
        BotCommand::new("start", "Главное меню"),
        BotCommand::new("menu", "Открыть меню"),
        BotCommand::new("help", "Как пользоваться ботом"),
        BotCommand::new("cancel", "Отменить текущий ввод"),
        BotCommand::new("admin", "Админ-панель"),
    ];
    if let Err(e) = bot.set_my_commands(commands).await {
        tracing::warn!("не удалось установить меню команд: {e}");
    }

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(bot::on_message))
        .branch(Update::filter_callback_query().endpoint(bot::on_callback))
        .branch(Update::filter_my_chat_member().endpoint(bot::on_chat_member));

    let mut dispatcher = Dispatcher::builder(bot.clone(), handler)
        .dependencies(dptree::deps![state.clone()])
        .build();

    let scheduler = tokio::spawn(monitor::run(state.clone()));

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    tracing::info!("бот запущен, интервал планировщика: {} с", cfg.tick_secs);
    tokio::select! {
        _ = dispatcher.dispatch() => tracing::info!("диспетчер завершил работу"),
        _ = sigterm.recv() => tracing::info!("получен SIGTERM, останавливаюсь"),
        _ = sigint.recv() => tracing::info!("получен SIGINT, останавливаюсь"),
    }

    scheduler.abort();
    pool.close().await;
    Ok(())
}