# Как помочь проекту

Нашёл ошибку — открой issue и напиши, что произошло, что ожидалось и как повторить проблему. Для сетевых ошибок полезны версия Windows, версия Telegram и результат диагностики. Секрет подключения и личные данные из журнала перед отправкой убери.

Крупное изменение лучше сначала обсудить в issue. Небольшое исправление можно сразу прислать через pull request. Старайся не смешивать сетевую логику, оформление и обновление зависимостей в одном изменении.

## Локальная сборка

Нужны Rust 1.96, Node.js 24.16 или совместимая версия и инструменты сборки MSVC для Windows.

```sh
npm ci --prefix ui
npm run build --prefix ui
cargo build --release --locked
```

На Windows нужен установленный Microsoft Edge WebView2 Runtime. В приложение браузерный движок не включён. Для Linux, macOS и консольной Windows-сборки достаточно `cargo build --release --locked --no-default-features`.

## Проверки

```sh
npm run build --prefix ui
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked --all-targets --no-default-features
```

На Windows дополнительно:

```sh
python tests/update_install.py
cargo test --locked --bin TgWsProxyPlus system_webview -- --ignored --nocapture
```

Последняя команда открывает изолированное тестовое окно WebView2, сохраняет кадры в `target/ui-review/` и использует отдельный конфиг. Telegram и пользовательские настройки она не трогает.

Интерфейс должен оставаться компактным: встроенные HTML, CSS и JS ограничены 250 КиБ без сжатия, Windows EXE — 2 МиБ. Не добавляй шрифты или библиотеки компонентов, если задачу можно решить существующими средствами. После изменения зависимостей обнови lockfile и уведомления о лицензиях через `python scripts/generate_notices.py`.

Обычная сборка включает проверку обновлений. Для локальной проверки без обращения к релизам задай `TGWS_REVIEW_BUILD=1` при сборке. Публикация релиза выполняется отдельно; `scripts/package.ps1` только собирает локальные файлы.
