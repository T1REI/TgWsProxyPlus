# Тесты

Обычный прогон не требует Python и доступа к внешним сервисам:

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

Тесты `performance`, `h2_pool` и модульные тесты пула проверяют гонки подключений, отмену, ранжирование, учёт трафика, выдачу готовых сокетов, балансировку и общий бюджет памяти H2. Ручные бенчмарки запускаются отдельно, последовательно, в release:

```powershell
cargo test --release --locked --test benchmarks --test h2_windows -- --ignored --nocapture --test-threads=1
python tests/icon_resource.py target/release/TgWsProxyPlus.exe icon.ico
python scripts/measure_resources.py path/to/old.exe target/release/TgWsProxyPlus.exe --output tests/results/resources.json
```

`benchmarks` сравнивает разбор пакетов и шифрованную пересылку с реализацией 0.1.0 из `support/baseline_framing.rs`. `h2_windows` моделирует задержку управляющего трафика HTTP/2; это не тест реальной скорости Telegram. Проверка ICO и измерение процессов предназначены для Windows. Скрипт ресурсов создаёт отдельные временные конфигурации и завершает только запущенные им процессы. Методика и ограничения описаны в [отчёте](../docs/performance.md).
