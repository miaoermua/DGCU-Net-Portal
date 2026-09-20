import { computed, onMounted, onUnmounted, reactive, ref, watch } from 'vue'
import type { DesktopBridge, InterfaceInfo, LogEntry, Session, Settings, Snapshot, UiPreferences, Unlisten } from './types'

export const defaultSettings = (): Settings => ({ server: 'http://172.18.100.65/lfradius/', auth_url: 'http://172.18.100.65/lfradius/web/admin/login', probe_url: 'http://captive.apple.com/hotspot-detect.html', probe_enabled: true, refresh_policy: 'five_seconds', interface_name: '', bypass_proxy: true, one_session: true, remember_account: false, username: '', auto_redial: false, tray_startup: false, service_enabled: false, show_sessions: false, log_enabled: false, theme_mode: 'system' })
export const emptySnapshot = (): Snapshot => ({ sessions: [], rates: {}, selected_id: null, authenticated: false, one_session: true, background_paused: true, status: 'idle', message: '填写账号后连接校园网' })
export function normalizeSettings(value: Settings): Settings {
  const next = { ...value }
  if (next.one_session) { next.remember_account = false; next.auto_redial = false; next.service_enabled = false }
  if (!next.remember_account) next.username = ''
  return next
}
export function formatBytes(value: number | undefined | null): string {
  if (value == null || !Number.isFinite(value) || value < 0) return '—'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++ }
  return `${value.toFixed(1)} ${units[unit]}`
}
export const formatRate = (value: number | null | undefined): string => value == null ? '等待更新' : `${formatBytes(value)}/s`
export function formatDuration(value: number): string { return [Math.floor(value / 3600), Math.floor(value % 3600 / 60), value % 60].map(v => String(v).padStart(2, '0')).join(':') }
export function mask(value: string): string { const chars = [...value]; return chars.length > 4 ? `${chars.slice(0, 2).join('')}***${chars.slice(-2).join('')}` : '****' }
const demoSessions = (): Session[] => [
  { radacctid: '90001', username: 'demo-student', framedipaddress: '192.0.2.10', acctstarttime: '', acctsessiontime: 720, acctinputoctets: 327680000, acctoutputoctets: 5927054868 },
  { radacctid: '90002', username: 'demo-student', framedipaddress: '192.0.2.20', acctstarttime: '', acctsessiontime: 1200, acctinputoctets: 102400, acctoutputoctets: 2097152 },
]
const phaseLabels: Record<string, string> = { discovering: '正在寻找认证页', reading_form: '正在读取认证表单', authenticating: '正在提交认证', waiting_portal: '正在等待 Portal 认证', waiting_dial: '正在等待代拨结果', accepted: 'Portal 已确认成功' }
export function createPortalState(bridge?: DesktopBridge) {
  const demo = ref(!bridge), busy = ref(false), ready = ref(false), page = ref(0)
  const version = ref('0.2.5')
  const saved = ref(defaultSettings()), draft = reactive(defaultSettings()), snapshot = ref(emptySnapshot())
  const networkInterfaces = ref<InterfaceInfo[]>([])
  const username = ref(''), password = ref(''), portalUrl = ref(''), phase = ref(''), notice = ref('')
  const preferencesBusy = ref(false), logEntries = ref<LogEntry[]>([]), logsOpen = ref(false), sessionPickerOpen = ref(false)
  let logEpoch = 0, demoSequence = 0, logTimer: ReturnType<typeof setInterval> | undefined
  const selected = computed(() => snapshot.value.sessions.find(row => row.radacctid === snapshot.value.selected_id))
  const hasUnsavedConnectionSettings = computed(() => {
    const keys: (keyof Settings)[] = ['server', 'auth_url', 'probe_url', 'probe_enabled', 'refresh_policy', 'interface_name', 'bypass_proxy', 'one_session', 'remember_account', 'auto_redial', 'tray_startup', 'service_enabled']
    const value=normalizeSettings(draft)
    return keys.some(key=>value[key]!==saved.value[key])
  })
  const rate = computed(() => selected.value ? snapshot.value.rates[selected.value.radacctid] : undefined)
  const isOnline = computed(() => ['accepted', 'session_online'].includes(snapshot.value.status))
  const disconnectIntent = computed(() => isOnline.value || !!selected.value || (snapshot.value.status === 'unknown' && snapshot.value.authenticated))
  const primaryLabel = computed(() => busy.value ? '处理中…' : disconnectIntent.value ? '下线' : '上线')
  const title = computed(() => ({ idle: '尚未连接', authenticating: '正在认证', accepted: '认证成功', backend: '已登录后台', session_online: '所选会话在线', offline: '已下线', error: '连接失败', unknown: '下线待确认' })[snapshot.value.status] ?? snapshot.value.status)
  const confirmation = ref<{ title: string; text: string; label: string } | null>(null)
  let confirmResolve: ((ok: boolean) => void) | undefined
  let disposed = false
  const unlisteners: Unlisten[] = []
  const notify = (text: unknown) => { notice.value = String(text) }
  function demoLog(code: string, message: string) {
    if (!demo.value || !saved.value.log_enabled) return
    logEntries.value = [...logEntries.value, { sequence: ++demoSequence, timestamp_ms: Date.now(), level: 'info' as const, code, message: `${message}（模拟）` }].slice(-300)
  }
  async function readLogs() {
    if (!saved.value.log_enabled || demo.value) return
    const epoch = logEpoch
    try {
      const entries = await bridge!.core.invoke<LogEntry[]>('read_logs')
      if (epoch === logEpoch && saved.value.log_enabled && !disposed) logEntries.value = entries
    } catch { if (epoch === logEpoch && logsOpen.value) notify('读取日志失败，请稍后重试') }
  }
  async function clearLogs() {
    logEpoch++
    if (!demo.value) { try { await bridge!.core.invoke('clear_logs') } catch { notify('清空日志失败'); return } }
    logEntries.value = []
  }
  async function openLogs() { if (!saved.value.log_enabled) return; logsOpen.value = true; await readLogs() }
  async function updatePreferences(patch: Partial<UiPreferences>) {
    if (preferencesBusy.value || !ready.value) return
    preferencesBusy.value = true
    try {
      const value: UiPreferences = { show_sessions: saved.value.show_sessions, log_enabled: saved.value.log_enabled, theme_mode: saved.value.theme_mode, refresh_policy: saved.value.refresh_policy, ...patch }
      const result = demo.value ? value : await bridge!.core.invoke<UiPreferences>('save_preferences', { value })
      const wasLogging = saved.value.log_enabled
      Object.assign(saved.value, result); Object.assign(draft, result)
      if (!result.log_enabled) { logEpoch++; logEntries.value = []; logsOpen.value = false }
      else if (!wasLogging) demoLog('log.enabled', '已开启本次会话日志')
    } catch (error) { notify(error) } finally { preferencesBusy.value = false }
  }
  async function primaryAction() {
    if (busy.value || !ready.value) return
    if (!disconnectIntent.value) { await connect(); return }
    if (!selected.value) { sessionPickerOpen.value = true; return }
    await disconnect()
  }
  async function selectForDisconnect(id: string) {
    await select(id)
    if (snapshot.value.selected_id !== id) return
    sessionPickerOpen.value = false
    await disconnect()
  }
  function clearFields() { username.value = ''; password.value = ''; portalUrl.value = '' }
  function receive(value: Snapshot) {
    snapshot.value = value
    if (!value.authenticated && value.one_session && ['offline', 'unknown', 'error'].includes(value.status)) clearFields()
  }
  async function run(action: () => Promise<void>) {
    if (busy.value || !ready.value) return
    busy.value = true
    try { await action() } catch (error) {
      notify(error)
      if (bridge && !demo.value) { try { receive(await bridge.core.invoke<Snapshot>('snapshot')) } catch { /* Keep the original error visible. */ } }
    } finally { busy.value = false; phase.value = '' }
  }
  function ask(title: string, text: string, label: string): Promise<boolean> {
    if (confirmation.value || busy.value) return Promise.resolve(false)
    confirmation.value = { title, text, label }
    return new Promise(resolve => { confirmResolve = resolve })
  }
  function answer(ok: boolean) { confirmation.value = null; const resolve = confirmResolve; confirmResolve = undefined; resolve?.(ok) }
  async function connect(backendOnly = false) {
    if (hasUnsavedConnectionSettings.value) {page.value=1;notify('连接设置已更改，请先保存设置再上线');return;}
    await run(async () => {
      if (demo.value) {
        clearFields()
        for (const status of backendOnly ? ['登录后台'] : ['提交认证', '等待代拨结果']) {
          demoLog('auth.phase', status)
          phase.value = status; await new Promise(resolve => setTimeout(resolve, 300))
        }
        receive({ ...emptySnapshot(), status: backendOnly ? 'backend' : 'accepted', message: '模拟流程完成，没有访问校园网', authenticated: true, one_session: saved.value.one_session, sessions: demoSessions(), selected_id: backendOnly ? null : '90001' })
        page.value = 0; return
      }
      if ((!username.value || !password.value) && !saved.value.remember_account) { page.value = 1; notify('请填写账号和密码'); return }
      let user = username.value, secret = password.value, entry = portalUrl.value
      password.value = ''
      try {
        receive(await bridge!.core.invoke<Snapshot>('connect', { username: user, password: secret, portalUrl: entry, backendOnly }))
        page.value = 0
      } finally { user = ''; secret = ''; entry = ''; if (saved.value.one_session) clearFields() }
    })
  }
  async function refresh() { await run(async () => { demoLog('sessions.read', '读取后台会话列表'); receive(demo.value ? { ...snapshot.value, rates: {}, message: '后台计费尚未更新（模拟）' } : await bridge!.core.invoke<Snapshot>('refresh')) }) }
  async function select(id: string) { await run(async () => receive(demo.value ? { ...snapshot.value, selected_id: id } : await bridge!.core.invoke<Snapshot>('select_session', { id }))) }
  async function disconnect() {
    const id = selected.value?.radacctid
    if (!id || !await ask('下线所选会话', `仅下线会话 ${id}，手动下线会暂停自动重拨。`, '确认下线')) return
    await run(async () => {
      try {
        demoLog('session.disconnect', '请求指定会话下线')
        if (demo.value) receive({ ...snapshot.value, status: 'offline', message: '所选会话已确认下线（模拟）', authenticated: !saved.value.one_session, sessions: saved.value.one_session ? [] : snapshot.value.sessions.filter(s => s.radacctid !== id), selected_id: null, rates: {} })
        else receive(await bridge!.core.invoke<Snapshot>('disconnect', { id }))
        demoLog('session.disconnected', '已确认下线并释放本地会话')
      } finally { if (saved.value.one_session) clearFields() }
    })
  }
  async function forget() {
    if (!await ask('清除本地会话', '释放客户端凭据与 Cookie，不向校园网发送下线请求。远端会话可能仍然在线。', '清除本地信息')) return
    await run(async () => { receive(demo.value ? emptySnapshot() : await bridge!.core.invoke<Snapshot>('forget')); clearFields() })
  }
  async function save() {
    if (snapshot.value.authenticated && !demo.value) { notify('请先结束本地会话，再修改连接与账号设置'); return }
    if (draft.service_enabled !== saved.value.service_enabled && !demo.value) {
      if (!await ask('更改后台启动', draft.service_enabled ? '将为当前用户写入登录时启动任务。只有保存设置后才会执行。' : '将停用当前用户的登录启动任务。', '保存更改')) return
    }
    await run(async () => {
      let secret = password.value
      try {
        const value = normalizeSettings({ ...draft, username: username.value })
        const result = demo.value ? value : await bridge!.core.invoke<Settings>('save_settings', { value, password: secret })
        saved.value = { ...result }; Object.assign(draft, result); snapshot.value.one_session = result.one_session
        // Keep unsaved transient credentials in this window until login/end of session.
        // Only an explicit "remember" saves them to the OS credential store.
        if (result.remember_account) password.value = ''
        notify(demo.value ? '演示设置已更新，仅在此窗口中生效' : '设置已保存')
      } finally { secret = '' }
    })
  }
  async function openSite() { await run(async () => { if (demo.value) notify('演示模式不会打开真实认证后台'); else await bridge!.core.invoke('open_auth_site', { url: saved.value.auth_url }) }) }
  async function openRepository() {
    await run(async () => {
      if (bridge) await bridge.core.invoke('open_repository')
      else if (typeof window !== 'undefined') window.open('https://github.com/miaoermua/dgcu-portal', '_blank', 'noopener,noreferrer')
    })
  }
  async function refreshInterfaces() {
    if (!bridge || demo.value || !ready.value) return
    try { networkInterfaces.value = await bridge.core.invoke<InterfaceInfo[]>('list_interfaces') }
    catch (error) { notify(error) }
  }
  async function close() {
    if (!await ask('退出客户端', saved.value.one_session ? '若已选择会话，将尝试下线后清除本地信息；未选择会话时仅清除本地信息。' : '退出并释放本地内存，远端会话可能继续在线。', '退出')) return
    await run(async () => { clearFields(); if (bridge) await bridge.core.invoke('exit_app') })
  }
  function simulateUpdate() {
    if (!demo.value || busy.value || !snapshot.value.sessions.length) return
    const sessions = snapshot.value.sessions.map(s => ({ ...s, acctsessiontime: s.acctsessiontime + 60, acctinputoctets: s.acctinputoctets + 614400, acctoutputoctets: s.acctoutputoctets + 62914560 }))
    receive({ ...snapshot.value, sessions, rates: Object.fromEntries(sessions.map(s => [s.radacctid, { upload_bps: 10240, download_bps: 1048576, sample_seconds: 60 }])), message: '后台计费更新：最近 60 秒平均下载 1 MiB/s（模拟）' })
  }
  async function initialize() {
    try {
      if (bridge) {
        const initial = await bridge.core.invoke<{ demo: boolean; settings: Settings; version?: string }>('initial')
        version.value = initial.version ?? version.value
        demo.value = initial.demo; saved.value = { ...defaultSettings(), ...initial.settings }; Object.assign(draft, saved.value)
        try {
          const listed = await bridge.core.invoke<unknown>('list_interfaces')
          networkInterfaces.value = Array.isArray(listed) ? listed as InterfaceInfo[] : []
        } catch { networkInterfaces.value = [] }
        username.value = initial.settings.one_session ? '' : initial.settings.username
        for (const [name, handler] of [
          ['auth-phase', (payload: string) => { phase.value = phaseLabels[payload] ?? '正在连接' }],
          ['snapshot', (payload: Snapshot) => { if (!busy.value && !demo.value) receive(payload) }],
          ['close-request', () => { void close() }],
        ] as const) {
          const unlisten = await bridge.event.listen(name, ({ payload }) => (handler as (value: unknown) => void)(payload))
          if (disposed) unlisten(); else unlisteners.push(unlisten)
        }
      }
      snapshot.value.one_session = saved.value.one_session
      ready.value = true
    } catch (error) { notify(`启动失败：${String(error)}`) }
  }
  const stopWatch = watch(() => draft.one_session, value => { if (value) { draft.remember_account = false; draft.auto_redial = false; draft.service_enabled = false } }, { flush: 'sync' })
  const stopLogWatch = watch(logsOpen, open => {
    if (logTimer) clearInterval(logTimer)
    if (open) logTimer = setInterval(() => { void readLogs() }, 1000)
  })
  function dispose() { disposed = true; stopWatch(); stopLogWatch(); if (logTimer) clearInterval(logTimer); logEpoch++; logEntries.value = []; unlisteners.splice(0).forEach(stop => stop()); clearFields(); answer(false) }
  return { demo, busy, ready, page, draft, saved, snapshot, username, password, portalUrl, networkInterfaces, selected, rate, isOnline, title, phase, notice, confirmation, answer, connect, refresh, select, disconnect, forget, save, openSite, close, simulateUpdate, initialize, dispose, preferencesBusy, logEntries, logsOpen, sessionPickerOpen, primaryLabel, primaryAction, selectForDisconnect, openLogs, readLogs, clearLogs, updatePreferences, version, openRepository, refreshInterfaces }
}
export function usePortal() { const state = createPortalState(window.__TAURI__); onMounted(state.initialize); onUnmounted(state.dispose); return state }
