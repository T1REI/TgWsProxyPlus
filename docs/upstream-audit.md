# Карта переноса исходного проекта

Источник: https://github.com/Flowseal/tg-ws-proxy.git. Зафиксированный commit: `307a384ab7f522e34bb1a045884d0bd90f40bee5`, версия 1.11.2. Справочную копию можно получить отдельным клонированием этого commit в `upstream/`. Она исключена из репозитория и не нужна для сборки или запуска Rust-приложения.

Исходная программа принимает локальный MTProto, проверяет секрет, заменяет внешний obfuscation AES-CTR и передаёт данные в транспорт Telegram Web. Cloudflare служит резервным путём, Worker пересылает WS в TCP, H2 отправляет отдельные MTProto messages в POST `/api`. Исходный интерфейс построен на Python/customtkinter; Windows/Linux/macOS entry points связывают настройки, трей, сеть и обновления.

| Исходные части | Rust-реализация | Проверка |
| --- | --- | --- |
| `proxy/tg_ws_proxy.py`, `_aes.py`, `utils.py` | `crypto`, `server`, `cli` | Python vectors, NIST CTR, init, wrong secret |
| `proxy/bridge.py` | `server`, `framing`, `routing` | backpressure, WS framing/ping, zero length/quick ack |
| `proxy/raw_websocket.py` | `network`, tokio-tungstenite | настоящий локальный WS handshake и бинарные кадры |
| `proxy/pool.py` | `pool` | TTL/backoff реализованы; доступность публичных endpoints зависит от сети |
| `proxy/balancer.py`, `config.py` | `routing::Domains`, `config` | домены, test DC, validation |
| `proxy/h2_transport.py` | `h2_transport` | мультиплексирование и большие тела с flow control |
| `proxy/cf_h2.py` | `h2_channel` | framing, encrypted replay, partial response, перегруженный канал |
| `proxy/fake_tls.py` | `fake_tls`, `server` | Python HMAC, server hello, CCS и однобайтовые чтения |
| `proxy/stats.py`, `network_debug.py` | `stats`, `logging` | bounded rotation, censoring |
| `proxy/event_loop.py` | Tokio, владение задачами | shutdown закрывает клиентов |
| `ui/*`, `windows.py`, Win32 theme | `windows` | ручная/автоматизированная визуальная проверка Windows |
| `utils/default_config.py`, `tray_common.py` | `config`, `windows` | JSON roundtrip, unknown fields, atomic save |
| `utils/update_check.py`, `windows_update.py` | `updates`, `windows` | SHA-256, semver, rollback при ошибке запуска |
| `utils/diagnostics.py` | `server::diagnose` | direct/Worker/CF WS/H2/TCP, параллельные bounded probes |
| Python packaging/PyInstaller | Cargo, Win32 resource, `scripts/package.ps1`, CI | локальная Windows release-сборка |
| `linux.py`, `macos.py`, Linux updater | переносное ядро и CLI | платформенные сборки и тесты в CI |

## Осознанные отличия

Перенос воспроизводит сетевые возможности и Windows-пользовательские сценарии, но не внутреннюю организацию Python и не каждый элемент customtkinter. Оболочка переписана на Win32 ради размера. Нативный Linux/macOS-трей, DMG/DEB/RPM, Windows 7 и подпись SignPath исходного автора не включены. Пользователь выбрал приоритет Windows 10/11; другие ОС поддерживаются на уровне исходников CLI и требуют отдельной проверки.

Расширены лимиты/проверки входных данных, ограничены запросы/память/история повторов, введены обязательный digest обновления и независимое имя репозитория обновлений. Сторонние ошибки сети завершают текущий канал; клиент переподключается. Debug-tracing upstream заменён ограниченным журналом Rust. Перезапуск слушателя выполняется при сохранении настроек; язык и тема — при следующем запуске.

Сохранены upstream MIT license и права Flowseal. Автор Rust-версии и Windows-ресурсов — T1REI. Удалённый Cloudflare Worker является отдельно развёртываемым сервисом; его исходный JavaScript и инструкции доступны в исходном репозитории Flowseal. Для Rust-клиента не требуется запуск Python или JavaScript.
