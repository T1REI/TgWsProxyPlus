# Тесты

Сначала собери интерфейс (`npm ci --prefix ui`, `npm run build --prefix ui`). Обычный Cargo-прогон не требует доступа к внешним сервисам:

```powershell
cargo test --locked --all-targets
cargo test --locked --all-targets --no-default-features
```

Реальные Telegram TCP/WS probes запускаются отдельно:

```powershell
cargo test --locked --test live_telegram -- --ignored --nocapture
```

Они проверяют анонимный req_pq_multi/resPQ с nonce. Их результат зависит от сети; локальные проверки используют loopback или in-memory duplex. Перед пересборкой завершите GUI, запущенный из `target/debug`, иначе Windows не позволит заменить исполняемый файл.

`fixtures/python-reference.json` создан вызовами оригинального Python-кода. Для повторной генерации клонируйте [Flowseal/tg-ws-proxy](https://github.com/Flowseal/tg-ws-proxy) в `upstream/` и выберите commit `307a384ab7f522e34bb1a045884d0bd90f40bee5`. Затем установите Python-зависимости из `upstream/pyproject.toml` в отдельное `tests/.venv` и выполните `tests/.venv/Scripts/python.exe tests/generate_vectors.py`. Для обычного Cargo-прогона достаточно сохранённого JSON.

Локальные тесты не подтверждают доступность публичных ретрансляторов. Звонки Telegram в эти проверки не входят: приложение не предоставляет VPN-туннель.

`updating` проверяет HTTP/1.1 framing и лимиты, ссылки и метаданные релизов, SHA-256, архитектуру EXE, миграцию старого поля репозитория и ожидание подтверждения запуска дочернего процесса. Windows-модульный тест закрывает отдельное окно через WM_CLOSE и SC_CLOSE, включая повторный вход при занятом состоянии UI; окно должно остаться существовать и скрыться.

Полный сценарий Windows-установщика проверяется отдельно:

```powershell
python tests/update_install.py
```

Проверки реального GitHub API через HTTP/1.1 и загрузки официального бинарника без установки запускаются вручную и требуют сети:

```powershell
cargo test --locked --test updating official_github -- --ignored --nocapture
```

Скрипт собирает небольшие тестовые EXE в изолированной временной папке и использует настоящий код обновления: ожидание старого процесса, проверку версии, замену файла, подтверждение готовности и удаление вспомогательных файлов. Второй сценарий имитирует аварийное завершение новой версии и проверяет восстановление и запуск прежней. Пользовательское приложение и его конфигурация в этих проверках не участвуют.

Тесты `performance`, `h2_pool` и модульные тесты пула проверяют гонки подключений, отмену, ранжирование, учёт трафика, выдачу готовых сокетов, балансировку и общий бюджет памяти H2. Ручные бенчмарки запускаются отдельно, последовательно, в release:

```powershell
cargo test --release --locked --test benchmarks --test h2_windows -- --ignored --nocapture --test-threads=1
python tests/icon_resource.py target/release/TgWsProxyPlus.exe icon.ico
python scripts/measure_resources.py path/to/old.exe target/release/TgWsProxyPlus.exe --output tests/results/resources.json
```

`benchmarks` сравнивает разбор пакетов и шифрованную пересылку с реализацией 0.1.0 из `support/baseline_framing.rs`. `h2_windows` моделирует задержку управляющего трафика HTTP/2; это не тест реальной скорости Telegram. Проверка ICO и измерение процессов предназначены для Windows. Скрипт ресурсов создаёт отдельные временные конфигурации и завершает только запущенные им процессы. Методика и ограничения описаны в [отчёте](../docs/performance.md).

## Интерфейс WebView2

```powershell
cargo test --locked --bin TgWsProxyPlus system_webview -- --ignored --nocapture
```

Проверяется реальный системный Runtime: загрузка встроенных ресурсов, Preact, снимок состояния Rust, запрет неизвестной команды и удалённой навигации, сохранение светлой темы без перезапуска прокси и сохранение локального слушателя при закрытии окна. Используются отдельные конфиг и порт. Скриншоты сохраняются в `target/ui-review`. Telegram не запускается. Модульные тесты отдельно проверяют валидацию настроек и разбор зарегистрированного пути Telegram.exe.
