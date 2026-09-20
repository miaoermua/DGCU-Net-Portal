export interface Settings {
  server: string
  auth_url: string
  probe_url: string
  probe_enabled: boolean
  refresh_policy: 'one_minute' | 'disabled'
  traffic_enabled: boolean
  credential_store: 'system' | 'file' | 'memory'
  interface_name: string
  bypass_proxy: boolean
  auto_redial: boolean
  tray_startup: boolean
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
export interface Snapshot {
  status: string
  message: string
  sessions: Session[]
  rates: Record<string, Rate>
  selected_id: string | null
  background_paused: boolean
  authenticated: boolean
  one_session: boolean
}
export type Unlisten = () => void
export interface DesktopBridge {
  core: { invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> }
  event: { listen<T>(name: string, handler: (event: { payload: T }) => void): Promise<Unlisten> }
}
declare global { interface Window { __TAURI__?: DesktopBridge } }
