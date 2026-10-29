import { useEffect, useRef, useState } from "preact/hooks";
import type { ComponentChildren } from "preact";
import {
  native,
  preview,
  request,
  subscribe,
  type Config,
  type State,
} from "./bridge";

type Page = "overview" | "connection" | "routes" | "diagnostics" | "settings";
type IconName =
  | "grid"
  | "link"
  | "sliders"
  | "route"
  | "pulse"
  | "copy"
  | "refresh"
  | "check"
  | "terminal"
  | "eye"
  | "close"
  | "minus"
  | "square";
function Icon({ name, size = 17 }: { name: IconName; size?: number }) {
  const paths: Record<IconName, ComponentChildren> = {
    grid: (
      <>
        <rect x="3" y="3" width="7" height="7" rx="3" />
        <rect x="14" y="3" width="7" height="7" rx="3" />
        <rect x="3" y="14" width="7" height="7" rx="3" />
        <rect x="14" y="14" width="7" height="7" rx="3" />
      </>
    ),
    link: (
      <>
        <path d="M10 13l4-4m-6 7-1 1a4 4 0 0 1-6-6l4-4a4 4 0 0 1 6 0m5 1 1-1a4 4 0 0 1 6 6l-4 4a4 4 0 0 1-6 0" />
      </>
    ),
    sliders: (
      <>
        <path d="M4 7h8m4 0h4M4 17h3m4 0h9" />
        <circle cx="14" cy="7" r="2" />
        <circle cx="9" cy="17" r="2" />
      </>
    ),
    route: (
      <>
        <circle cx="6" cy="5" r="2" />
        <circle cx="18" cy="19" r="2" />
        <path d="M6 7v9a3 3 0 0 0 3 3h7M8 5h7a3 3 0 0 1 0 6H9" />
      </>
    ),
    pulse: <path d="M2 12h5l3-8 4 16 3-8h5" />,
    copy: (
      <>
        <rect x="8" y="8" width="12" height="13" rx="2" />
        <path d="M16 8V3H3v13h5" />
      </>
    ),
    refresh: (
      <>
        <path d="M20 7v5h-5M4 17v-5h5" />
        <path d="M5.7 7a7 7 0 0 1 12-2l2.3 3M4 16l2.3 3a7 7 0 0 0 12-2" />
      </>
    ),
    check: <path d="m5 12 4 4L19 6" />,
    terminal: (
      <>
        <path d="m4 6 6 6-6 6m9 0h7" />
      </>
    ),
    eye: (
      <>
        <path d="M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12Z" />
        <circle cx="12" cy="12" r="3" />
      </>
    ),
    close: <path d="m6 6 12 12M6 18 18 6" />,
    minus: <path d="M5 12h14" />,
    square: <rect x="5" y="5" width="14" height="14" rx="1" />,
  };
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {paths[name]}
    </svg>
  );
}
const bytes = (value = 0) =>
  value < 1024
    ? `${value} B`
    : value < 1048576
      ? `${(value / 1024).toFixed(1)} KiB`
      : `${(value / 1048576).toFixed(1)} MiB`;
const duration = (value: number) =>
  `${Math.floor(value / 3600)
    .toString()
    .padStart(2, "0")}:${Math.floor((value / 60) % 60)
    .toString()
    .padStart(2, "0")}:${Math.floor(value % 60)
    .toString()
    .padStart(2, "0")}`;

export default function App() {
  const [state, setState] = useState<State>(preview);
  const [loaded, setLoaded] = useState(!native);
  const [draft, setDraft] = useState<Config>(preview.config);
  const [page, setPage] = useState<Page>("overview");
  const [toast, setToast] = useState<{ text: string; error: boolean }>();
  const [busy, setBusy] = useState(false);
  const [secretVisible, setSecretVisible] = useState(false);
  const initialized = useRef(!native);
  const toastTimer = useRef<number>();
  const dirty = JSON.stringify(draft) !== JSON.stringify(state.config);
  const ru =
    (draft.language === "auto" ? state.system_language : draft.language) !==
    "en";
  const t = (a: string, b: string) => (ru ? a : b);
  const notify = (text: string, error = false) => {
    setToast({ text, error });
    clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(
      () => setToast(undefined),
      error ? 10000 : 4000,
    );
  };
  useEffect(() => {
    const accept = (next: State) => {
      const first = !initialized.current;
      initialized.current = true;
      setState((previous) => {
        setDraft((current) =>
          JSON.stringify(current) === JSON.stringify(previous.config) || first
            ? next.config
            : current,
        );
        return next;
      });
      setLoaded(true);
    };
    const unsubscribe = subscribe(accept);
    if (native)
      request<State>("state")
        .then(accept)
        .catch((error) => notify(error.message, true));
    return () => {
      unsubscribe();
      clearTimeout(toastTimer.current);
    };
  }, []);
  useEffect(() => {
    const theme =
      draft.appearance === "auto"
        ? state.system_dark
          ? "dark"
          : "light"
        : String(draft.appearance);
    document.documentElement.dataset.theme = theme;
    document.documentElement.lang = ru ? "ru" : "en";
  }, [draft.appearance, ru, state.system_dark]);
  const act = async (command: string, success?: string, data?: unknown) => {
    try {
      await request(command, data);
      if (success) notify(success);
    } catch (error) {
      notify((error as Error).message, true);
    }
  };
  const set = (key: string, value: Config[string]) =>
    setDraft((current) => ({ ...current, [key]: value }));
  const save = async () => {
    setBusy(true);
    try {
      const next = await request<State>("save", draft);
      setState(next);
      setDraft(next.config);
      notify(t("Настройки сохранены", "Settings saved"));
    } catch (error) {
      notify((error as Error).message, true);
    } finally {
      setBusy(false);
    }
  };
  const field = (
    key: string,
    label: string,
    hint?: string,
    options: { type?: string; min?: number; max?: number; step?: number } = {},
  ) => (
    <label class="field">
      <span>{label}</span>
      <input
        value={String(draft[key] ?? "")}
        type={options.type || "text"}
        {...options}
        onInput={(event) =>
          set(
            key,
            options.type === "number"
              ? Number(event.currentTarget.value)
              : event.currentTarget.value,
          )
        }
      />
      {hint && <small>{hint}</small>}
    </label>
  );
  const toggle = (key: string, label: string, hint: string) => (
    <label class="toggle-row">
      <span>
        <strong>{label}</strong>
        <small>{hint}</small>
      </span>
      <input
        type="checkbox"
        class="switch"
        checked={Boolean(draft[key])}
        onChange={(e) => set(key, e.currentTarget.checked)}
      />
    </label>
  );
  const list = (key: string, label: string, hint: string) => (
    <label class="field">
      <span>{label}</span>
      <textarea
        rows={3}
        spellcheck={false}
        value={(draft[key] as string[]).join("\n")}
        onInput={(e) => set(key, e.currentTarget.value.split("\n"))}
      />
      <small>{hint}</small>
    </label>
  );
  const nav: {
    id: Page;
    label: string;
    icon: IconName;
    description: string;
  }[] = [
    {
      id: "overview",
      label: t("Прокси", "Proxy"),
      icon: "grid",
      description: "Telegram Desktop · MTProto",
    },
    {
      id: "connection",
      label: t("Подключение", "Connection"),
      icon: "link",
      description: t(
        "Адрес и защита локального сервера",
        "Local server address and security",
      ),
    },
    {
      id: "routes",
      label: t("Маршруты", "Routes"),
      icon: "route",
      description: t(
        "Транспорт и резервные пути",
        "Transport and fallback routes",
      ),
    },
    {
      id: "diagnostics",
      label: t("Диагностика", "Diagnostics"),
      icon: "pulse",
      description: t(
        "Проверка доступных соединений",
        "Check available connections",
      ),
    },
    {
      id: "settings",
      label: t("Настройки", "Settings"),
      icon: "sliders",
      description: t(
        "Оформление и работа приложения",
        "Appearance and application preferences",
      ),
    },
  ];
  return (
    <div class="app">
      <header
        class="titlebar"
        onPointerDown={(e) => {
          if (e.button === 0 && !(e.target as HTMLElement).closest("button"))
            void act("drag");
        }}
        onDblClick={() => void act("maximize")}
      >
        <div class="wordmark">
          TgWsProxy<span>+</span>
        </div>
        <div class="window-actions">
          <button
            aria-label={t("Свернуть", "Minimize")}
            onClick={() => void act("minimize")}
          >
            <Icon name="minus" size={14} />
          </button>
          <button
            aria-label={t("Развернуть", "Maximize")}
            onClick={() => void act("maximize")}
          >
            <Icon name="square" size={12} />
          </button>
          <button
            class="window-close"
            aria-label={t("Скрыть в трей", "Hide to tray")}
            onClick={() => void act("hide")}
          >
            <Icon name="close" size={15} />
          </button>
        </div>
      </header>
      <div class="workspace">
        <aside class="sidebar">
          <nav aria-label={t("Разделы", "Sections")}>
            {nav.map((item, index) => (
              <>
                {(index === 0 || index === 3) && (
                  <div class="nav-label">
                    {index === 0
                      ? t("Основное", "General")
                      : t("Приложение", "Application")}
                  </div>
                )}
                <button
                  data-page={item.id}
                  class={page === item.id ? "nav-item selected" : "nav-item"}
                  aria-current={page === item.id ? "page" : undefined}
                  onClick={() => setPage(item.id)}
                >
                  <Icon name={item.icon} size={17} />
                  {item.label}
                </button>
              </>
            ))}
          </nav>
          <div class="sidebar-bottom">
            <span>
              TgWsProxy+ <small>{state.version}</small>
            </span>
            <small>
              {t("Время работы", "Uptime")}{" "}
              <span class="mono">{duration(state.uptime)}</span>
            </small>
          </div>
        </aside>
        <main>
          <div class="page-heading">
            <h1>{nav.find((item) => item.id === page)?.label}</h1>
            <p>{nav.find((item) => item.id === page)?.description}</p>
          </div>
          {!loaded && (
            <div class="notice">
              {t(
                "Подключаемся к приложению…",
                "Connecting to the application…",
              )}
            </div>
          )}
          {native && loaded && !state.ready && (
            <div class="notice proxy-error" role="status">
              {state.status ||
                t("Запускаем локальный сервер…", "Starting the local server…")}
            </div>
          )}
          {!native && (
            <div class="notice">
              {t(
                "Предпросмотр без подключения к приложению",
                "Preview without a native connection",
              )}
            </div>
          )}
          {page === "overview" && (
            <>
              <section class="connection-details">
                <div class="section-toolbar">
                  <h2>{t("Подключение", "Connection")}</h2>
                  <button
                    class="text-button"
                    onClick={() => setPage("connection")}
                  >
                    {t("Изменить", "Edit")}
                  </button>
                </div>
                <div class="form-grid endpoint-fields">
                  <label class="field">
                    <span>{t("Адрес", "Address")}</span>
                    <input readOnly value={String(state.config.host)} />
                  </label>
                  <label class="field">
                    <span>{t("Порт", "Port")}</span>
                    <input readOnly value={String(state.config.port)} />
                  </label>
                </div>
                <div class="field">
                  <span>{t("Ссылка подключения", "Connection link")}</span>
                  <div class="endpoint">
                    <span class="mono">
                      tg://proxy?server={String(state.config.host)}&amp;port=
                      {String(state.config.port)}&amp;secret=••••
                    </span>
                    <button
                      class="text-button"
                      aria-label={t(
                        "Скопировать ссылку прокси",
                        "Copy proxy link",
                      )}
                      disabled={!native}
                      title={t("Скопировать ссылку прокси", "Copy proxy link")}
                      onClick={() =>
                        void act("copy", t("Ссылка скопирована", "Link copied"))
                      }
                    >
                      <Icon name="copy" size={16} />
                    </button>
                  </div>
                </div>
                <div class="main-actions">
                  <button
                    class="primary"
                    disabled={!native || !loaded}
                    onClick={() => void act("telegram")}
                  >
                    {t("Открыть Telegram", "Open Telegram")}
                  </button>
                  <button
                    disabled={!native || !state.ready}
                    onClick={() => void act("connect")}
                  >
                    {t("Подключить прокси", "Connect proxy")}
                  </button>
                </div>
              </section>
              <section
                class="traffic-row panel"
                aria-label={t("Статистика сеанса", "Session statistics")}
              >
                <div>
                  <span>{t("Соединения", "Connections")}</span>
                  <strong>
                    {state.stats.active || 0}
                    <small> / {String(state.config.max_connections)}</small>
                  </strong>
                </div>
                <div>
                  <span>{t("Получено", "Downloaded")}</span>
                  <strong>{bytes(state.stats.down)}</strong>
                </div>
                <div>
                  <span>{t("Отправлено", "Uploaded")}</span>
                  <strong>{bytes(state.stats.up)}</strong>
                </div>
              </section>
              <div class="utility-row">
                <button
                  class="text-button"
                  disabled={!native}
                  onClick={() => void act("logs")}
                >
                  <Icon name="terminal" size={14} />
                  {t("Журнал", "Log")}
                </button>
                <button
                  class="text-button"
                  disabled={!native}
                  onClick={() =>
                    void act(
                      "restart",
                      t("Прокси перезапускается", "Restarting proxy"),
                    )
                  }
                >
                  <Icon name="refresh" size={14} />
                  {t("Перезапустить", "Restart")}
                </button>
              </div>
              <p class="footnote">
                {t(
                  "Закрытие окна оставляет прокси работать в трее.",
                  "Closing this window keeps the proxy running in the tray.",
                )}
              </p>
            </>
          )}
          {page === "connection" && (
            <>
              <section class="panel form-panel">
                <div class="section-title">
                  <h3>{t("Локальный сервер", "Local server")}</h3>
                  <p>
                    {t(
                      "Эти параметры Telegram использует для подключения к прокси.",
                      "Telegram uses these details to connect to your proxy.",
                    )}
                  </p>
                </div>
                <div class="form-grid">
                  {field(
                    "host",
                    t("Адрес", "Address"),
                    t(
                      "Оставь 127.0.0.1 для этого компьютера.",
                      "Keep 127.0.0.1 for this computer.",
                    ),
                  )}
                  {field("port", t("Порт", "Port"), "1–65535", {
                    type: "number",
                    min: 1,
                    max: 65535,
                  })}
                </div>
                <label class="field">
                  <span>{t("Секрет подключения", "Connection secret")}</span>
                  <div class="secret-field">
                    <input
                      type={secretVisible ? "text" : "password"}
                      value={String(draft.secret)}
                      spellcheck={false}
                      maxLength={32}
                      onInput={(e) => set("secret", e.currentTarget.value)}
                    />
                    <button
                      title={t("Показать или скрыть", "Show or hide")}
                      aria-label={t(
                        "Показать или скрыть секрет",
                        "Show or hide secret",
                      )}
                      onClick={() => setSecretVisible(!secretVisible)}
                    >
                      <Icon name="eye" size={16} />
                    </button>
                    <button
                      onClick={async () => {
                        try {
                          set("secret", await request<string>("secret"));
                        } catch (e) {
                          notify((e as Error).message, true);
                        }
                      }}
                    >
                      {t("Сгенерировать", "Generate")}
                    </button>
                  </div>
                  <small>
                    {t(
                      "32 шестнадцатеричных символа. После смены подключи прокси в Telegram заново.",
                      "32 hexadecimal characters. Reconnect Telegram after changing the secret.",
                    )}
                  </small>
                </label>
              </section>
              <section class="panel form-panel">
                <div class="section-title">
                  <h3>{t("Защита соединения", "Connection protection")}</h3>
                </div>
                {field(
                  "fake_tls_domain",
                  t("Домен Fake TLS", "Fake TLS domain"),
                  t(
                    "Необязательно. Например, www.microsoft.com. Без https:// и пути.",
                    "Optional. For example, www.microsoft.com, without https:// or a path.",
                  ),
                )}
                {toggle(
                  "no_secure",
                  t("Отключить secure transport", "Disable secure transport"),
                  t(
                    "Режим совместимости. Обычно выключен.",
                    "Compatibility mode. Usually disabled.",
                  ),
                )}
              </section>
              <div class="notice">
                {t(
                  "Сначала сохрани изменения, затем нажми «Подключить прокси» на вкладке «Прокси».",
                  "Save your changes, then select Connect proxy on the Proxy page.",
                )}
              </div>
            </>
          )}
          {page === "routes" && (
            <>
              <section class="panel form-panel">
                <div class="section-title">
                  <h3>{t("Выбор маршрута", "Routing")}</h3>
                  <p>
                    {t(
                      "Подходящий маршрут выбирается отдельно для каждого соединения.",
                      "A suitable route is selected for each connection.",
                    )}
                  </p>
                </div>
                {toggle(
                  "cfproxy",
                  t(
                    "Резервные маршруты Cloudflare",
                    "Cloudflare fallback routes",
                  ),
                  t(
                    "Использовать альтернативный путь, когда прямой недоступен.",
                    "Use an alternative path when the direct route is unavailable.",
                  ),
                )}
                {toggle(
                  "h2",
                  "HTTP/2",
                  t(
                    "Мультиплексирование соединений через Cloudflare.",
                    "Multiplex connections through Cloudflare.",
                  ),
                )}
                {toggle(
                  "cfproxy_user_domain_enabled",
                  t("Свои домены", "Custom domains"),
                  t(
                    "Добавить домены для резервных маршрутов.",
                    "Add domains for fallback routes.",
                  ),
                )}
                {Boolean(draft.cfproxy_user_domain_enabled) &&
                  list(
                    "cfproxy_user_domain",
                    t("Домены", "Domains"),
                    t("По одному домену на строку.", "One domain per line."),
                  )}
                {toggle(
                  "cfproxy_worker_enabled",
                  "Cloudflare Worker",
                  t(
                    "Использовать свой развёрнутый Worker.",
                    "Use your own deployed Worker.",
                  ),
                )}
                {Boolean(draft.cfproxy_worker_enabled) &&
                  list(
                    "cfproxy_worker_domain",
                    t("Домены Worker", "Worker domains"),
                    t("По одному домену на строку.", "One domain per line."),
                  )}
              </section>
              <section class="panel">
                <div class="panel-heading">
                  <h3>{t("Наблюдаемые маршруты", "Observed routes")}</h3>
                  <span class="dim">{state.routes.length}</span>
                </div>
                <div class="table-scroll">
                  <table>
                    <thead>
                      <tr>
                        <th>DC / {t("транспорт", "transport")}</th>
                        <th>{t("Соединения", "Connections")}</th>
                        <th>{t("Подключение", "Connect time")}</th>
                        <th>{t("Ошибки", "Errors")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {state.routes.map((route) => (
                        <tr>
                          <td>
                            <strong>
                              DC{route.dc} · {route.kind}
                              {route.media ? " · media" : ""}
                            </strong>
                            <small class="mono route-endpoint">
                              {route.endpoint}
                            </small>
                          </td>
                          <td>{route.active}</td>
                          <td>
                            {route.successes ? `${route.connect_ms} ms` : "—"}
                          </td>
                          <td>{route.errors}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                {!state.routes.length && (
                  <div class="empty-state">
                    <Icon name="route" size={24} />
                    <span>
                      {t(
                        "Маршруты появятся после первых соединений",
                        "Routes appear after the first connections",
                      )}
                    </span>
                  </div>
                )}
              </section>
              <details class="panel advanced">
                <summary>
                  {t(
                    "Дополнительные параметры маршрутизации",
                    "Advanced routing",
                  )}
                </summary>
                <div class="details-content">
                  {list(
                    "dc_ip",
                    t("Адреса дата-центров", "Data center addresses"),
                    t(
                      "Формат: DC:IPv4, по одному на строку.",
                      "Format: DC:IPv4, one per line.",
                    ),
                  )}
                  {toggle(
                    "proxy_protocol",
                    "PROXY protocol",
                    t(
                      "Для серверов за совместимым балансировщиком.",
                      "For servers behind a compatible load balancer.",
                    ),
                  )}
                  {toggle(
                    "force_test_dc",
                    t("Тестовые дата-центры", "Test data centers"),
                    t(
                      "Использовать тестовую инфраструктуру Telegram.",
                      "Use Telegram test infrastructure.",
                    ),
                  )}
                </div>
              </details>
            </>
          )}
          {page === "diagnostics" && (
            <>
              <section class="panel form-panel">
                <div class="section-title">
                  <h3>{t("Проверка соединения", "Connection check")}</h3>
                  <p>
                    {t(
                      "Проверим доступные маршруты и соберём сетевую статистику. Это может занять немного времени.",
                      "Probe available routes and collect network statistics. This may take a moment.",
                    )}
                  </p>
                </div>
                <div class="button-row">
                  <button
                    class="primary"
                    disabled={!native || state.diagnosing}
                    onClick={() => void act("diagnose")}
                  >
                    <Icon name="pulse" size={16} />
                    {state.diagnosing
                      ? t("Проверяем…", "Checking…")
                      : t("Запустить проверку", "Run diagnostics")}
                  </button>
                  <button disabled={!native} onClick={() => void act("logs")}>
                    <Icon name="terminal" size={16} />
                    {t("Открыть журнал", "Open log")}
                  </button>
                </div>
              </section>
              <section class="panel">
                <div class="panel-heading">
                  <h3>{t("Результат проверки", "Diagnostic results")}</h3>
                  {state.diagnostics && (
                    <button
                      class="text-button"
                      onClick={() =>
                        void act(
                          "copy_diagnostics",
                          t("Отчёт скопирован", "Report copied"),
                        )
                      }
                    >
                      <Icon name="copy" size={14} />
                      {t("Копировать", "Copy")}
                    </button>
                  )}
                </div>
                {state.diagnostics ? (
                  <pre class="diagnostic-output">{state.diagnostics}</pre>
                ) : (
                  <div class="empty-state">
                    <Icon name="terminal" size={25} />
                    <span>
                      {t(
                        "Здесь будет отчёт о соединении",
                        "Your connection report will appear here",
                      )}
                    </span>
                    <small>
                      {t(
                        "Проверка запускается только по твоему запросу.",
                        "Diagnostics only run when you request them.",
                      )}
                    </small>
                  </div>
                )}
              </section>
            </>
          )}
          {page === "settings" && (
            <>
              <section class="panel form-panel">
                {toggle(
                  "autostart",
                  t("Запускать вместе с Windows", "Start with Windows"),
                  t(
                    "Приложение запустится в трее.",
                    "The application will start in the tray.",
                  ),
                )}
                <div class="form-grid">
                  <label class="field">
                    <span>{t("Оформление", "Appearance")}</span>
                    <select
                      value={String(draft.appearance)}
                      onChange={(e) => set("appearance", e.currentTarget.value)}
                    >
                      <option value="auto">
                        {t("Как в системе", "System")}
                      </option>
                      <option value="dark">{t("Тёмное", "Dark")}</option>
                      <option value="light">{t("Светлое", "Light")}</option>
                    </select>
                  </label>
                  <label class="field">
                    <span>{t("Язык", "Language")}</span>
                    <select
                      value={String(draft.language)}
                      onChange={(e) => set("language", e.currentTarget.value)}
                    >
                      <option value="ru">Русский</option>
                      <option value="en">English</option>
                      <option value="auto">
                        {t("Как в системе", "System")}
                      </option>
                    </select>
                  </label>
                </div>
              </section>
              <details class="panel advanced">
                <summary>
                  {t("Производительность и журнал", "Performance and logging")}
                </summary>
                <div class="details-content">
                  <div class="form-grid">
                    {field("pool_size", t("Размер пула", "Pool size"), "0–32", {
                      type: "number",
                      min: 0,
                      max: 32,
                    })}
                    {field(
                      "max_connections",
                      t("Лимит соединений", "Connection limit"),
                      "1–16384",
                      { type: "number", min: 1, max: 16384 },
                    )}
                    {field(
                      "buf_kb",
                      t("Буфер, KiB", "Buffer, KiB"),
                      t("0 — автоматически", "0 — automatic"),
                      { type: "number", min: 0, max: 4096 },
                    )}
                    {field(
                      "h2_max_connections",
                      t("Соединения HTTP/2", "HTTP/2 connections"),
                      "1–3",
                      { type: "number", min: 1, max: 3 },
                    )}
                    {field(
                      "h2_stream_window_kb",
                      t("Окно потока H2, KiB", "H2 stream window, KiB"),
                      "64–4096",
                      { type: "number", min: 64, max: 4096 },
                    )}
                    {field(
                      "h2_connection_window_kb",
                      t("Окно соединения H2, KiB", "H2 connection window, KiB"),
                      "64–16384",
                      { type: "number", min: 64, max: 16384 },
                    )}
                    {field(
                      "log_max_mb",
                      t("Размер журнала, MiB", "Log size, MiB"),
                      "0.03125–1024",
                      {
                        type: "number",
                        min: 0.03125,
                        max: 1024,
                        step: 0.03125,
                      },
                    )}
                    {field(
                      "log_backups",
                      t("Копий журнала", "Log backups"),
                      "1–20",
                      { type: "number", min: 1, max: 20 },
                    )}
                  </div>
                  {toggle(
                    "verbose",
                    t("Подробный журнал", "Verbose logging"),
                    t(
                      "Включай для поиска сетевых проблем.",
                      "Enable when troubleshooting network issues.",
                    ),
                  )}
                </div>
              </details>
              <section class="panel form-panel">
                <div class="about-heading">
                  <h3>TgWsProxy+</h3>
                  <span class="dim">{state.version}</span>
                  <button
                    class="text-button"
                    onClick={() => void act("repository")}
                  >
                    GitHub
                  </button>
                </div>
                {state.review ? (
                  <p class="review-note">
                    {t(
                      "Сборка для проверки. Обновления пока отключены.",
                      "Review build. Updates are currently disabled.",
                    )}
                  </p>
                ) : (
                  <>
                    <div class="update-controls">
                      <label class="update-toggle">
                        <input
                          class="switch"
                          type="checkbox"
                          checked={Boolean(draft.check_updates)}
                          onChange={(event) =>
                            set("check_updates", event.currentTarget.checked)
                          }
                        />
                        <span>
                          {t("Проверять обновления", "Check for updates")}
                        </span>
                      </label>
                      <button
                        class="text-button"
                        disabled={!native}
                        onClick={() => void act("updates")}
                      >
                        {t("Проверить", "Check now")}
                      </button>
                    </div>
                    {state.update_status && (
                      <p class="review-note" role="status">
                        {state.update_status}
                      </p>
                    )}
                  </>
                )}
              </section>
            </>
          )}
        </main>
      </div>
      {dirty && (
        <div class="save-bar">
          <span>
            {t("Есть несохранённые изменения", "You have unsaved changes")}
          </span>
          <div>
            <button disabled={busy} onClick={() => setDraft(state.config)}>
              {t("Отменить", "Discard")}
            </button>
            <button
              class="primary"
              disabled={busy || !native}
              onClick={() => void save()}
            >
              <Icon name="check" size={15} />
              {busy
                ? t("Сохраняем…", "Saving…")
                : t("Сохранить", "Save changes")}
            </button>
          </div>
        </div>
      )}
      {toast && (
        <div
          class={`toast ${toast.error ? "error" : ""}`}
          role={toast.error ? "alert" : "status"}
        >
          <span>{toast.text}</span>
          <button
            aria-label={t("Закрыть уведомление", "Dismiss notification")}
            onClick={() => setToast(undefined)}
          >
            <Icon name="close" size={14} />
          </button>
        </div>
      )}
    </div>
  );
}
