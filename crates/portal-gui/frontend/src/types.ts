export type RunMode = 'standard' | 'tray_startup' | 'lightweight'
export interface Settings {
  server: string
  auth_url: string
  probe_url: string
  paip: string
  basip: string
  probe_enabled: boolean
  refresh_policy: 'one_minute' | 'disabled'
  poll_jitter: 'low' | 'medium' | 'high' | 'disabled'
  traffic_enabled: boolean
  credential_store: 'system' | 'file' | 'memory'
  interface_name: string
  bypass_proxy: boolean
  reconnect_mode: 'disabled' | 'new_session' | 'terminate_and_reconnect'
  run_mode: RunMode
  service_enabled: boolean
  username: string
  show_sessions: boolean
  log_enabled: boolean
  theme_mode: 'system' | 'light' | 'dark'
}
export interface InterfaceInfo {
  name: string
  ipv4: string | null
  mac: string | null
  internal: boolean
}
export type UiPreferences = Pick<Settings, 'show_sessions' | 'log_enabled' | 'theme_mode' | 'refresh_policy' | 'traffic_enabled'>
export interface LogEntry { sequence: number; timestamp_ms: number; level: 'info' | 'warn' | 'error'; code: string; message: string }
export interface Session {
  radacctid: string
  username: string
  acctstarttime: string
  acctsessiontime: number
  framedipaddress: string | null
  acctinputoctets: number
  acctoutputoctets: number
}
export interface Rate {
  upload_bps: number | null
  download_bps: number | null
  sample_seconds: number | null
}
export interface AccountInfo {
  plan: string
  bandwidth: string | null
  expires_on: string | null
  // 后台把未订购账号的套餐名写成“学生”，此时卡片显示 Free，而不是回退成原始套餐名。
  unpurchased: boolean
}
export type Reachability = 'reachable' | 'captive' | 'unreachable'
export interface Diagnostic {
  auth: Reachability
  auth_latency_ms: number | null
  // 绑定所选网卡（与认证同一条路径）的探测结果，区分“直连正常”和“本机走了代理”。
  internet_direct: Reachability
  internet: Reachability
  internet_latency_ms: number | null
}
export interface Snapshot {
  status: string
  message: string
  sessions: Session[]
  rates: Record<string, Rate>
  selected_id: string | null
  background_paused: boolean
  authenticated: boolean
  one_session: boolean
  account: AccountInfo | null
}
export interface UpdateInfo {
  current: string
  latest: string
  newer: boolean
  url: string
}
// idle：还没查过（演示模式停在这一步）；checking：正在查；latest：已是最新；available：有新版本；error：超时或网络失败。
export type UpdatePhase = 'idle' | 'checking' | 'latest' | 'available' | 'error'
export type Unlisten = () => void
export interface DesktopBridge {
  core: { invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> }
  event: { listen<T>(name: string, handler: (event: { payload: T }) => void): Promise<Unlisten> }
}
declare global { interface Window { __TAURI__?: DesktopBridge } }
