# Site Checker

Telegram-бот на Rust для мониторинга доступности сайтов. Каждый пользователь добавляет свои сайты и получает уведомления о падении и восстановлении в личный чат с ботом.

## Возможности

- Проверка сайта: HTTP-статус 2xx + поиск текста-подстроки на странице (регистр учитывается; поддерживаются UTF-8, windows-1251, koi8-r и другие кодировки из заголовка Content-Type).
- Интервалы проверок: 5 мин, 15 мин, 30 мин, 1 час, 6 часов, 24 часа.
- CRUD сайтов: добавление, редактирование URL/текста/интервала, включение/отключение, удаление, кнопка «Проверить сейчас».
- Уведомления: 🔴 при падении (с причиной), 🟢 при восстановлении (с временем простоя). Повторные проверки уже упавшего сайта не спамят. Надёжная доставка: сообщение сохраняется в очередь и переживает временные сбои Telegram (ретраи с backoff); при блокировке бота пользователем очередь очищается.
- Напоминания «всё ещё недоступен»: опционально, раз в 1/6/12/24 часа пока сайт лежит.
- История и статистика: uptime % за 24ч/7д, средняя задержка, журнал инцидентов (начало/конец/причина/длительность) в карточке сайта.
- Алерт «медленный ответ»: опциональный порог задержки (300 мс … 5 с), одно уведомление за эпизод.
- Лимит сайтов на пользователя (защита от злоупотреблений).
- Админ-панель: статистика, CRUD пользователей, бан/разбан (забаненный теряет доступ к боту, его сайты не проверяются), режим «использовать бота от имени пользователя».
- Защита от SSRF: запрещены localhost, приватные IP и домены, резолвящиеся во внутреннюю сеть.
- Автобэкап БД (VACUUM INTO) с ротацией копий.
- SQLite (WAL), параллельные проверки, троттлинг запросов к Telegram API, systemd watchdog.

## Требования

- Rust ≥ 1.94 (stable)
- Токен бота от [@BotFather](https://t.me/BotFather)

## Быстрый старт

```bash
cp .env.example .env   # впишите BOT_TOKEN и ADMIN_IDS
cargo run --release
```

Напишите боту `/start`. Пользователи из `ADMIN_IDS` увидят кнопку «🛠 Админ-панель».

## Конфигурация (.env)

| Переменная | По умолчанию | Описание |
|---|---|---|
| `BOT_TOKEN` | — | токен от BotFather (обязательно) |
| `ADMIN_IDS` | `547169256` | Telegram ID администраторов через запятую |
| `DATABASE_PATH` | `data/site_checker.db` | путь к файлу SQLite |
| `CHECK_TICK_SECONDS` | `15` | период просмотра очереди проверок |
| `HTTP_TIMEOUT_SECONDS` | `15` | таймаут запроса к сайту |
| `MAX_CONCURRENT_CHECKS` | `10` | одновременные проверки |
| `FAILURE_THRESHOLD` | `1` | неудач подряд до уведомления о падении |
| `MAX_SITES_PER_USER` | `50` | лимит сайтов на пользователя (0 = без лимита) |
| `BACKUP_DIR` | `data/backups` | каталог автобэкапов БД |
| `BACKUP_INTERVAL_HOURS` | `24` | периодичность бэкапов |
| `BACKUP_KEEP` | `7` | сколько копий хранить (0 = отключить бэкапы) |
| `RUST_LOG` | `info` | уровень логов |

## Деплой под systemd

Собирать рекомендуется на целевой машине (или проверить совместимость glibc: `objdump -T site-checker | grep GLIBC_`). Сборочные зависимости: только C-компилятор (`build-essential`), OpenSSL не нужен — TLS через rustls.

```bash
cargo build --release

sudo mkdir -p /opt/site-checker/data
sudo cp target/release/site-checker /opt/site-checker/
sudo cp .env /opt/site-checker/.env
sudo cp deploy/site-checker.service /etc/systemd/system/

sudo groupadd --system sitechecker
sudo useradd --system --gid sitechecker --home-dir /opt/site-checker \
  --no-create-home --shell /usr/sbin/nologin sitechecker
sudo chown -R sitechecker:sitechecker /opt/site-checker
sudo chmod 600 /opt/site-checker/.env

sudo systemctl daemon-reload
sudo systemctl enable --now site-checker
journalctl -u site-checker -f
```

Unit уже настроен: автоперезапуск, отдельный пользователь, `ProtectSystem=strict` с записью только в `data/` (включая каталог бэкапов `data/backups`), корректная остановка по SIGTERM. `Type=notify` + `WatchdogSec=120`: процесс сообщает о готовности и пингует watchdog; при зависании systemd перезапустит сервис. Если меняете `BACKUP_DIR` за пределы `data/`, добавьте путь в `ReadWritePaths`.

### Обновление через GitHub Releases

При пуше тега `v*` GitHub Actions прогоняет тесты, собирает release-бинарь (ubuntu-24.04, glibc совместим с Debian 12+/Ubuntu 24.04+) и публикует его в Releases вместе с `SHA256SUMS`.

Выпуск новой версии:

```bash
# поднять version в Cargo.toml, закоммитить
git tag v0.1.1 && git push origin v0.1.1
```

Однократная подготовка сервера:

```bash
apt-get install -y curl jq
scp deploy/update.sh root@SERVER:/opt/site-checker/update.sh
ssh root@SERVER 'chmod +x /opt/site-checker/update.sh'
```

Обновление на сервере:

```bash
/opt/site-checker/update.sh           # до последнего релиза
/opt/site-checker/update.sh --force   # переустановить текущий
```

Скрипт проверяет sha256, останавливает сервис, заменяет бинарь, запускает и автоматически откатывается на предыдущую версию, если сервис не поднялся. Миграции БД применяются приложением при старте.

Запасной вариант без CI — собрать локально и залить вручную:

```bash
cargo build --release
scp target/release/site-checker root@SERVER:/opt/site-checker/site-checker
ssh root@SERVER 'systemctl restart site-checker'
```

## Команды бота

- `/start`, `/menu` — главное меню
- `/help` — справка
- `/cancel` — отменить текущий ввод
- `/admin` — админ-панель (только для администраторов)

## Тесты

```bash
cargo test
```

Покрыты: HTTP-проверки (локальный тестовый сервер), кодировки, SSRF-фильтр, CRUD и очередь БД, история/инциденты/uptime, очередь уведомлений и backoff, логика смены статусов, напоминаний и slow-алертов, бэкап и ротация, UI-хелперы, режим «от имени пользователя» (включая самовосстановление при удалении цели).