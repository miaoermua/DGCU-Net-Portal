export interface Settings {
  server: string
  auth_url: string
  probe_url: string
  bypass_proxy: boolean
  one_session: boolean
  auto_redial: boolean
  tray_startup: boolean
  service_enabled: boolean
  remember_account: boolean
  username: string
}
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
