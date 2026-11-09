mod backup;
mod bot;
mod checker;
mod config;
mod db;
mod monitor;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use sd_notify::NotifyState;
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
        notify_lock: Mutex::new(()),
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
    let notifier = tokio::spawn(monitor::notification_loop(state.clone()));
    let maintenance = tokio::spawn(monitor::maintenance_loop(state.clone()));
    let backup = if cfg.backup_keep > 0 {
        tracing::info!(
            "автобэкап БД: каждые {} ч в {}, хранить {}",
            cfg.backup_interval_hours,
            cfg.backup_dir,
            cfg.backup_keep
        );
        Some(tokio::spawn(backup::backup_loop(
            pool.clone(),
            cfg.backup_dir.clone(),
            cfg.backup_interval_hours,
            cfg.backup_keep,
        )))
    } else {
        None
    };

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    // systemd watchdog: пингуем на половине таймаута, пока жив tokio-runtime.
    let watchdog = {
        let mut usec = 0u64;
        if sd_notify::watchdog_enabled(false, &mut usec) && usec > 0 {
            let interval = Duration::from_micros(usec / 2);
            tracing::info!("systemd watchdog активен, пинг каждые {} мс", interval.as_millis());
            Some(tokio::spawn(async move {
                let mut tick = tokio::time::interval(interval);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tick.tick().await;
                    let _ = sd_notify::notify(false, &[NotifyState::Watchdog]);
                }
            }))
        } else {
            None
        }
    };

    tracing::info!("бот запущен, интервал планировщика: {} с", cfg.tick_secs);
    let _ = sd_notify::notify(false, &[NotifyState::Ready]);
    tokio::select! {
        _ = dispatcher.dispatch() => tracing::info!("диспетчер завершил работу"),
        _ = sigterm.recv() => tracing::info!("получен SIGTERM, останавливаюсь"),
        _ = sigint.recv() => tracing::info!("получен SIGINT, останавливаюсь"),
    }

    let _ = sd_notify::notify(false, &[NotifyState::Stopping]);
    scheduler.abort();
    notifier.abort();
    maintenance.abort();
    if let Some(b) = backup {
        b.abort();
    }
    if let Some(w) = watchdog {
        w.abort();
    }
    pool.close().await;
    Ok(())
}