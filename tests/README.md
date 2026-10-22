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
