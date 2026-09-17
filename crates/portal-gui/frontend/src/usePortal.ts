import { computed, onMounted, onUnmounted, reactive, ref, watch } from 'vue'
import type { DesktopBridge, Session, Settings, Snapshot, Unlisten } from './types'

export const defaultSettings = (): Settings => ({ server: 'http://172.18.100.65/lfradius/', auth_url: 'http://172.18.100.65/lfradius/web/admin/login', probe_url: 'http://captive.apple.com/hotspot-detect.html', bypass_proxy: true, one_session: true, remember_account: false, username: '', auto_redial: false, tray_startup: false, service_enabled: false })
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
  const saved = ref(defaultSettings()), draft = reactive(defaultSettings()), snapshot = ref(emptySnapshot())
  const username = ref(''), password = ref(''), portalUrl = ref(''), phase = ref(''), notice = ref('')
  const selected = computed(() => snapshot.value.sessions.find(row => row.radacctid === snapshot.value.selected_id))
  const rate = computed(() => selected.value ? snapshot.value.rates[selected.value.radacctid] : undefined)
  const isOnline = computed(() => ['accepted', 'session_online'].includes(snapshot.value.status))
  const title = computed(() => ({ idle: '尚未连接', authenticating: '正在认证', accepted: '认证成功', backend: '已登录后台', session_online: '所选会话在线', offline: '已下线', error: '连接失败', unknown: '下线待确认' })[snapshot.value.status] ?? snapshot.value.status)
  const confirmation = ref<{ title: string; text: string; label: string } | null>(null)
  let confirmResolve: ((ok: boolean) => void) | undefined
  let disposed = false
  const unlisteners: Unlisten[] = []
  const notify = (text: unknown) => { notice.value = String(text) }
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
    await run(async () => {
      if (demo.value) {
        clearFields()
        for (const status of backendOnly ? ['登录后台'] : ['提交认证', '等待代拨结果']) {
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
  async function refresh() { await run(async () => receive(demo.value ? { ...snapshot.value, rates: {}, message: '后台计费尚未更新（模拟）' } : await bridge!.core.invoke<Snapshot>('refresh'))) }
  async function select(id: string) { await run(async () => receive(demo.value ? { ...snapshot.value, selected_id: id } : await bridge!.core.invoke<Snapshot>('select_session', { id }))) }
  async function disconnect() {
    const id = selected.value?.radacctid
    if (!id || !await ask('下线所选会话', `仅下线会话 ${id}，手动下线会暂停自动重拨。`, '确认下线')) return
    await run(async () => {
      try {
        if (demo.value) receive({ ...snapshot.value, status: 'offline', message: '所选会话已确认下线（模拟）', authenticated: !saved.value.one_session, sessions: saved.value.one_session ? [] : snapshot.value.sessions.filter(s => s.radacctid !== id), selected_id: null, rates: {} })
        else receive(await bridge!.core.invoke<Snapshot>('disconnect', { id }))
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
        password.value = ''; if (result.one_session) clearFields()
        notify(demo.value ? '演示设置已更新，仅在此窗口中生效' : '设置已保存')
      } finally { secret = '' }
    })
  }
  async function openSite() { await run(async () => { if (demo.value) notify('演示模式不会打开真实认证网站'); else await bridge!.core.invoke('open_auth_site', { url: saved.value.auth_url }) }) }
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
        const initial = await bridge.core.invoke<{ demo: boolean; settings: Settings }>('initial')
        demo.value = initial.demo; saved.value = initial.settings; Object.assign(draft, initial.settings)
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
  function dispose() { disposed = true; stopWatch(); unlisteners.splice(0).forEach(stop => stop()); clearFields(); answer(false) }
  return { demo, busy, ready, page, draft, saved, snapshot, username, password, portalUrl, selected, rate, isOnline, title, phase, notice, confirmation, answer, connect, refresh, select, disconnect, forget, save, openSite, close, simulateUpdate, initialize, dispose }
}
export function usePortal() { const state = createPortalState(window.__TAURI__); onMounted(state.initialize); onUnmounted(state.dispose); return state }
