<h1 align="center">TgWsProxy+</h1>
<p align="center">Локальный MTProto-прокси для Telegram. Rust, трей Windows и компактный интерфейс.</p>
<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green" alt="MIT license"></a>
  <a href="https://github.com/T1REI/TgWsProxyPlus/releases/latest"><img src="https://img.shields.io/github/v/release/T1REI/TgWsProxyPlus" alt="Latest release"></a>
  <a href="https://github.com/T1REI/TgWsProxyPlus/actions/workflows/ci.yml"><img src="https://github.com/T1REI/TgWsProxyPlus/actions/workflows/ci.yml/badge.svg" alt="Build and tests"></a>
  <a href="https://github.com/T1REI/TgWsProxyPlus/releases"><img src="https://img.shields.io/github/downloads/T1REI/TgWsProxyPlus/total" alt="Release downloads"></a>
</p>
<p align="center">
  <img src="assets/screenshots/overview.png" width="760" alt="TgWsProxy+: состояние локального прокси и статистика трафика">
</p>
<p align="center">
  <a href="#первый-запуск">Первый запуск</a> ·
  <a href="#если-telegram-не-подключается">Диагностика</a> ·
  <a href="#сборка">Сборка</a> ·
  <a href="CHANGELOG.md">Изменения</a> ·
  <a href="docs/architecture.md">Архитектура</a> ·
  <a href="CONTRIBUTING.md">Участие в проекте</a>
</p>

TgWsProxy+ — небольшой локальный прокси для Telegram Desktop. Он принимает MTProto-соединения на твоём компьютере и передаёт их через WebSocket или резервные маршруты, когда обычный путь недоступен.

Включил прокси один раз — дальше приложение остаётся в трее. Один клик по значку открывает Telegram, правый клик — меню приложения.

## Что нового в 0.0.3

Выпуск от 16 ноября 2026 года.

- **UI открывается быстрее:** медиана готовности окна в трёх локальных замерах сократилась с 2,55 до 0,57 секунды — примерно в 4,5 раза. [Методика измерения](docs/ui-startup.md).
- **Исправлен резервный TCP:** зависшие WS-маршруты больше не лишают его времени на подключение.
- **Диагностика проверяет WebSocket без TLS** и подсказывает подходящий режим при недоступности TLS-маршрутов. Переключатель в настройках получил понятное описание.
- **Обновлено оформление README:** добавлены динамические бейджи лицензии, релиза, проверок и скачиваний.

Полный список изменений и выполненных проверок — в [журнале версий](CHANGELOG.md).

> **Переход с 0.2.x:** скачай 0.0.3 вручную, закрой прежний экземпляр через меню трея и замени EXE. Нумерация начинается заново, поэтому старое приложение не предложит эту версию автоматически. Настройки сохранятся.

## Что внутри

- Rust-ядро и интерфейс на Astro + Preact через установленный в Windows WebView2. Браузерный движок не входит в EXE.
- Спокойные светлая и тёмная темы, русский и английский языки, поддержка системного масштаба.
- Статистика трафика, наблюдаемые маршруты, проверка соединений и журнал.
- MTProto abridged, intermediate и padded intermediate, Fake TLS и PROXY protocol v1.
- Telegram WebSocket, Cloudflare WebSocket и HTTP/2, собственный Worker и резервный TCP.
- Настройки пула и HTTP/2; консольный режим для Windows, Linux и macOS.

Локальная x64-сборка с интерфейсом занимает около **1,4 МиБ**. Точный размер и условия измерения — в [отчёте о версии 0.0.2](docs/ui-0.0.2.md). Файлы установленного WebView2 Runtime и его память в это число не входят.

## Первый запуск

Нужны Windows 10/11 и [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/). Если Runtime уже установлен, скачивать его повторно не потребуется.

1. Скачай [Windows x64 EXE](https://github.com/T1REI/TgWsProxyPlus/releases/latest/download/TgWsProxyPlus-windows-x86_64.exe) и запусти его. Все сборки — на [странице релизов](https://github.com/T1REI/TgWsProxyPlus/releases/latest).
2. В разделе «Прокси» нажми **Подключить прокси** и подтверди добавление в Telegram.
3. Закрой окно приложения — прокси продолжит работать в трее.

По умолчанию используется `127.0.0.1:1443`. Секрет создаётся при первом запуске и сохраняется в конфигурации. После смены адреса, порта или секрета добавь прокси в Telegram заново.

**ЛКМ по трею** открывает Telegram. **ПКМ → Открыть TgWsProxy+** показывает интерфейс. Для полного завершения выбери **Выход**. Повторный запуск EXE открывает окно уже работающего экземпляра.

Если локальный сервер не может запуститься, приложение покажет сообщение с причиной. Доступность удалённых маршрутов проверяется отдельно в разделе «Диагностика».

## Если Telegram не подключается

Открой **Диагностика → Проверить соединения** или выполни `TgWsProxyPlus --diagnose`. Запущенный локальный сервер ещё не означает, что удалённые маршруты доступны.

- `bad > 0`: проверь адрес, порт и секрет; добавь прокси в Telegram заново после изменения настроек.
- `TCP connect timed out`: прямой адрес Telegram недоступен из текущей сети.
- CF WS/H2 зависают на TLS, но **CF WS without TLS** отвечает: в настройках подключения включи **Без TLS** (`"no_secure": true` в JSON) и сохрани. Этот режим использует WebSocket на порту 80; H2 отключается. MTProto-шифрование сохраняется, дополнительный TLS-слой отсутствует. Включай его осознанно; когда TLS снова доступен, выключи.
- Не отвечают и резервные маршруты: проверь другую сеть или настрой собственный [Cloudflare Worker](https://github.com/Flowseal/tg-ws-proxy/blob/main/docs/RU/CfWorker.md). Доступность публичных серверов зависит от сети.

Не публикуй конфигурацию целиком и ссылку `tg://proxy`: в них есть секрет. Для отчёта об ошибке достаточно версии, текста диагностики и журнала с удалёнными секретами.

## Настройки и переносной режим

Конфигурация и журнал находятся в `%APPDATA%\TgWsProxyPlus`. Профиль WebView2 хранится рядом в подкаталоге `WebView2`.

Для переносного режима создай рядом с EXE пустой файл `TgWsProxyPlus.portable` или запусти программу с `--portable`. Данные будут в `TgWsProxyPlus-data` рядом с приложением.

Проверка обновлений включена по умолчанию: при запуске и каждые шесть часов. Её можно отключить в настройках.

Смена темы, языка, автозапуска и параметров журнала не разрывает соединения. Изменение сетевых параметров перезапускает прокси. Несохранённые поля остаются в окне, пока ты не сохранишь или не отменишь изменения.

## Сборка

Для Windows нужны Rust **1.96**, MSVC Build Tools и Node.js **24.16** или совместимая версия.

```sh
npm ci --prefix ui
npm run build --prefix ui
cargo build --release --locked
```

Результат: `target/release/TgWsProxyPlus.exe`. HTML, CSS и JS встроены в этот файл; Node.js на компьютере пользователя не нужен. `scripts/package.ps1` собирает локальный ZIP с документацией и лицензиями и ничего не публикует.

Для разработки интерфейса: `npm run dev --prefix ui`. В браузере открывается явно обозначенный предпросмотр без подключения к Rust-ядру.

Консольная сборка не требует фронтенда или WebView2:

```sh
cargo build --release --locked --no-default-features
```

```sh
TgWsProxyPlus --console
TgWsProxyPlus --diagnose
TgWsProxyPlus --help
```

Параметры командной строки действуют только для текущего запуска. Свой JSON можно создать через `--init-config path/to/config.json` и использовать через `--config path/to/config.json`.

## Что стоит знать

Приложение не является VPN. Оно не перенаправляет весь трафик компьютера и не обеспечивает голосовые или видеозвонки Telegram. Доступность резервных маршрутов зависит от сети и удалённых серверов.

Основной процесс прокси компактный, но открытое окно WebView2 использует отдельные процессы и дополнительную память. Окно создаётся только по запросу; обычный запуск в трее не загружает WebView2.

## Документация

[История изменений](CHANGELOG.md) · [Архитектура](docs/architecture.md) · [Измерения 0.0.2](docs/ui-0.0.2.md) · [Скорость открытия UI](docs/ui-startup.md) · [Сетевые измерения](docs/performance.md) · [Сверка с upstream](docs/upstream-audit.md) · [Тесты](tests/README.md)

Нашёл ошибку или есть идея — [открой issue](https://github.com/T1REI/TgWsProxyPlus/issues). Как собрать проект и подготовить изменение, описано в [CONTRIBUTING.md](CONTRIBUTING.md).

## Авторы и лицензия

Rust-версию и приложение развивает **T1REI**. Основа — [tg-ws-proxy от Flowseal](https://github.com/Flowseal/tg-ws-proxy).

Код распространяется под MIT: [лицензия проекта](LICENSE), [лицензия upstream](LICENSE.upstream). Уведомления о сторонних компонентах находятся в [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt).
