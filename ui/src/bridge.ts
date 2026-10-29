export type Config = Record<string, string | number | boolean | string[]>;
export interface Route {
  dc: number;
  media: boolean;
  kind: string;
  endpoint: string;
  connect_ms: number;
  first_ms: number;
  active: number;
  errors: number;
  successes: number;
  up: number;
  down: number;
}
export interface State {
  version: string;
  config: Config;
  ready: boolean;
  status: string;
  uptime: number;
  stats: Record<string, number>;
  routes: Route[];
  system_dark: boolean;
  diagnostics: string;
  diagnosing: boolean;
  update_status: string;
  review: boolean;
  system_language: string;
}
declare global {
  interface Window {
    chrome?: {
      webview?: {
        postMessage: (message: unknown) => void;
        addEventListener: (
          name: string,
          handler: (event: MessageEvent) => void,
        ) => void;
        removeEventListener: (
          name: string,
          handler: (event: MessageEvent) => void,
        ) => void;
      };
    };
  }
}
export const native = Boolean(window.chrome?.webview);
let serial = 0;
const pending = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (reason: Error) => void }
>();
const listeners = new Set<(state: State) => void>();
window.chrome?.webview?.addEventListener("message", ({ data }) => {
  if (data.event === "state") {
    for (const fn of listeners) fn(data.data);
    return;
  }
  const call = pending.get(data.id);
  if (!call) return;
  pending.delete(data.id);
  if (data.ok) call.resolve(data.data);
  else call.reject(new Error(data.error || "Не удалось выполнить действие"));
});
export function request<T = unknown>(
  command: string,
  data?: unknown,
): Promise<T> {
  if (!native)
    return Promise.reject(
      new Error("Это предпросмотр. Действие доступно в приложении."),
    );
  const id = ++serial;
  return new Promise<T>((resolve, reject) => {
    pending.set(id, { resolve: (value) => resolve(value as T), reject });
    window.chrome!.webview!.postMessage({ id, command, data });
    window.setTimeout(() => {
      if (pending.delete(id))
        reject(new Error("Приложение не ответило. Повторите действие."));
    }, 15000);
  });
}
export function subscribe(fn: (state: State) => void) {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}
export const preview: State = {
  version: "0.0.2",
  ready: false,
  status: "Предпросмотр интерфейса",
  uptime: 0,
  review: false,
  system_language: "ru",
  stats: {},
  routes: [],
  system_dark: true,
  diagnostics: "",
  diagnosing: false,
  update_status: "",
  config: {
    host: "127.0.0.1",
    port: 1443,
    secret: "00000000000000000000000000000000",
    dc_ip: ["2:149.154.167.220", "4:149.154.167.220"],
    buf_kb: 0,
    pool_size: 4,
    max_connections: 512,
    cfproxy: true,
    h2: true,
    h2_max_connections: 2,
    h2_stream_window_kb: 1024,
    h2_connection_window_kb: 4096,
    cfproxy_user_domain_enabled: false,
    cfproxy_user_domain: [],
    cfproxy_worker_enabled: false,
    cfproxy_worker_domain: [],
    no_secure: false,
    fake_tls_domain: "",
    proxy_protocol: false,
    force_test_dc: false,
    verbose: false,
    log_max_mb: 5,
    log_backups: 1,
    check_updates: true,
    autostart: false,
    appearance: "dark",
    language: "ru",
  },
};
