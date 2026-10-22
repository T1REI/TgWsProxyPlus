# TgWsProxy+ / TgWsProxyPlus

Небольшой локальный прокси для Telegram Desktop, написанный на Rust 2024. Помогает подключиться к Telegram через WebSocket и резервные маршруты, когда обычное соединение недоступно.

В Windows приложение работает в трее: запускаете один EXE, включаете прокси в Telegram и продолжаете пользоваться мессенджером. Устанавливать Python или дополнительные библиотеки не нужно.

**Автор Rust-версии — T1REI.** Основано на [tg-ws-proxy от Flowseal](https://github.com/Flowseal/tg-ws-proxy), лицензия MIT.

## Скачать и запустить

Готовая версия для **Windows 10/11 x64** находится в [Releases](https://github.com/T1REI/TgWsProxyPlus/releases/latest). EXE занимает около **1,2 МиБ**; ZIP содержит программу, документацию и лицензии.

1. Скачайте `TgWsProxyPlus-windows-x86_64.zip` и распакуйте его в отдельную папку.
2. Запустите `TgWsProxyPlus.exe`.
3. В настройках нажмите **Открыть Telegram** и подтвердите подключение прокси.

По умолчанию используется `127.0.0.1:1443`. Секрет подключения создаётся при первом запуске и сохраняется в настройках. Держите приложение запущенным, пока используете этот прокси.

Закрытие окна скрывает его в трей. Чтобы завершить программу, нажмите правой кнопкой на значок и выберите **Выход**. Повторный запуск открывает настройки уже работающего экземпляра.

Исполняемый файл пока не подписан цифровой подписью. Контрольные суммы опубликованы рядом с файлами релиза в `SHA256SUMS.txt`.

## Возможности

- Нативные настройки Windows, русский и английский интерфейс, светлая и тёмная темы.
- Трей, автозапуск, диагностика соединения и журнал с ротацией.
- MTProto abridged, intermediate и padded intermediate; поддержка Fake TLS и PROXY protocol v1.
- Telegram WebSocket, пулы соединений и резервные пути через Cloudflare WebSocket, HTTP/2, собственный Worker и TCP.
- Проверка обновлений из этого репозитория и проверка SHA-256 перед установкой.
- Консольный режим; исходники сетевого ядра также предназначены для Linux и macOS.

**Звонки через этот прокси не поддерживаются.** Telegram Desktop передаёт голосовой и видеотрафик отдельно от MTProto-подключения. Приложение не является VPN и не перенаправляет весь трафик компьютера.

Доступность маршрутов зависит от вашей сети и удалённых серверов. Публичные резервные домены загружаются из [исходного проекта](https://github.com/Flowseal/tg-ws-proxy); при необходимости можно указать свои домены и Worker. [Инструкция по Worker](https://github.com/Flowseal/tg-ws-proxy/blob/307a384ab7f522e34bb1a045884d0bd90f40bee5/docs/RU/CfWorker.md).

## Настройки и переносной режим

Настройки и журнал находятся в `%APPDATA%\TgWsProxyPlus\`. Чтобы хранить их рядом с программой, создайте пустой файл `TgWsProxyPlus.portable` рядом с EXE или запустите приложение с `--portable`. Данные появятся в папке `TgWsProxyPlus-data`.

Изменения соединения применяются кнопкой **Сохранить**. После смены языка или темы перезапустите приложение. Для открытия настроек можно использовать `--settings`.

Новые конфигурации проверяют обновления в `T1REI/TgWsProxyPlus`. Если вы пользовались ранней сборкой с пустым полем `update_repository`, укажите это значение в настройках. Отключить автоматическую проверку можно там же.

Не публикуйте свой `config.json` и ссылку подключения: они содержат секрет прокси.

## Сборка из исходников

Нужны **Rust/Cargo 1.96.0+** и MSVC Build Tools с Windows SDK. На Linux дополнительно нужны `pkg-config` и заголовки OpenSSL; macOS использует системный TLS.

```powershell
git clone https://github.com/T1REI/TgWsProxyPlus.git
cd TgWsProxyPlus
cargo build --release --locked
.\target\release\TgWsProxyPlus.exe
```

Для консольной сборки без Windows-интерфейса:

```sh
cargo build --release --locked --no-default-features
./target/release/TgWsProxyPlus --console
```

Полезные команды (в Windows используйте путь к EXE):

```sh
TgWsProxyPlus --help
TgWsProxyPlus --init-config my-config.json
TgWsProxyPlus --config my-config.json --check-config
TgWsProxyPlus --config my-config.json --link
TgWsProxyPlus --config my-config.json --diagnose
TgWsProxyPlus --config my-config.json --console
```

Параметры CLI действуют только на текущий запуск и не перезаписывают JSON. Консольный режим завершается по Ctrl+C, в Unix также по SIGTERM.

Windows-дистрибутив собирается командой `powershell -File scripts/package.ps1`. Скрипт добавляет лицензии, документацию и контрольные суммы, проверяя ограничение размера EXE в 2 МиБ. Для уменьшения размера используются LTO, оптимизация `z` и системные Win32/SChannel; Windows CRT встроен в EXE.

Для Docker создайте конфигурацию в папке `proxy-data`, задайте `host` равным `0.0.0.0` и обеспечьте пользователю контейнера доступ к ней:

```sh
docker build -t tgwsproxyplus .
docker run --rm --user "$(id -u):$(id -g)" -p 127.0.0.1:1443:1443 -v "$PWD/proxy-data:/data" tgwsproxyplus --config /data/config.json --console
```

## Проверка и устройство проекта

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked --all-targets --no-default-features
```

Тесты находятся в [`tests/`](tests/README.md): криптография, фрейминг, локальные WebSocket/HTTP/2-соединения, восстановление каналов, конфигурация и CLI. Проверки внешних серверов запускаются отдельно и зависят от сети.

Подробности: [архитектура](docs/architecture.md), [соответствие исходному проекту](docs/upstream-audit.md). Основная пользовательская платформа — Windows 10/11; Linux/macOS предоставляют консольный режим.

## Лицензия

[MIT](LICENSE). © 2026 T1REI. Сохранены [лицензия и авторство Flowseal](LICENSE.upstream). Лицензии зависимостей собраны в [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt).
