import { afterEach, describe, expect, it, vi } from 'vitest'
import { createPortalState, defaultSettings, emptySnapshot, formatRate, normalizeSettings } from './usePortal'
import { licenseGroups } from './licenses'
import type { DesktopBridge, Session, Settings } from './types'

const row = (id: string): Session => ({ radacctid: id, username: 'synthetic-user', framedipaddress: '192.0.2.1', acctstarttime: '', acctsessiontime: 60, acctinputoctets: 600, acctoutputoctets: 6000 })
// passive 是后台轮询与事件返回的快照；one_session 与后端一致，随凭据保存方式变化。
function desktop(status = 'idle', credentialStore: Settings['credential_store'] = 'system') {
  const stops = [vi.fn(), vi.fn(), vi.fn()]
  const passive = { ...emptySnapshot(), status, authenticated: false, one_session: credentialStore === 'memory' }
  const invoke = vi.fn(async (command: string) => {
    if (command === 'initial') return { demo: false, settings: { ...defaultSettings(), credential_store: credentialStore } }
    if (command === 'snapshot') return passive
    return { ...emptySnapshot(), status: 'accepted', authenticated: true, sessions: [row('A'), row('B')] }
  })
  const listen = vi.fn(async () => stops[listen.mock.calls.length - 1]!)
  const bridge = { core: { invoke }, event: { listen } } as unknown as DesktopBridge
  return { bridge, invoke, listen, stops }
}
afterEach(() => vi.useRealTimers())
describe('Vue migration preserves privacy and IPC behavior', () => {
  it('saving transient settings keeps typed credentials for the upcoming connection but not in persisted settings', async () => {
    const state=createPortalState();await state.initialize()
    state.username.value='synthetic-user';state.password.value='not-real';
    await state.save()
    expect(state.saved.value.username).toBe('synthetic-user')
    expect(state.username.value).toBe('synthetic-user');expect(state.password.value).toBe('')
    state.dispose();expect(state.password.value).toBe('')
  })
  it('opens the repository through a dedicated desktop command without account data', async () => {
    const mock=desktop(),state=createPortalState(mock.bridge);await state.initialize()
    await state.openRepository();expect(mock.invoke).toHaveBeenLastCalledWith('open_repository')
    state.dispose()
  })
  it('opens an open source license link with the url as the only payload', async () => {
    const mock=desktop(),state=createPortalState(mock.bridge);await state.initialize()
    await state.openUrl('https://github.com/vuejs/core')
    expect(mock.invoke).toHaveBeenLastCalledWith('open_external',{url:'https://github.com/vuejs/core'})
    expect(mock.invoke).not.toHaveBeenCalledWith('connect',expect.anything())
    state.dispose()
  })
  it('keeps every open source entry unique and pointing at an https page', () => {
    const entries=licenseGroups.flatMap(group=>group.entries)
    expect(entries.length).toBeGreaterThan(0)
    for(const entry of entries){
      expect(entry.url).toMatch(/^https:\/\/[^\s]+$/)
      expect(entry.name.trim()).toBe(entry.name)
      expect(entry.license.trim()).not.toBe('')
    }
    expect(new Set(entries.map(entry=>entry.name)).size).toBe(entries.length)
  })
  it('will not connect under a stale privacy setting before the change is saved', async()=>{
    const mock=desktop(),state=createPortalState(mock.bridge);await state.initialize()
    state.draft.credential_store='memory'
    state.username.value='synthetic';state.password.value='not-real'
    await state.connect()
    expect(mock.invoke).not.toHaveBeenCalledWith('connect',expect.anything())
    expect(state.page.value).toBe(1)
    expect(state.notice.value).toContain('保存')
    state.dispose()
  })
  it('defaults to hidden management, disabled logs and system theme', async () => {
    const state=createPortalState();await state.initialize()
    expect(state.saved.value.show_sessions).toBe(false)
    expect(state.saved.value.log_enabled).toBe(false)
    expect(state.saved.value.theme_mode).toBe('system')
    expect(state.primaryLabel.value).toBe('上线')
    state.dispose()
  })
  it('asks for a session choice exactly when authenticated without a bound session', async () => {
    const state=createPortalState();await state.initialize()
    // 未认证：没有可管理的会话，不该提示选择。
    state.snapshot.value={...emptySnapshot(),authenticated:false}
    expect(state.needsSelection.value).toBe(false)
    // 已认证但会计系统还没上报会话：仍要露出会话区，让用户看到"正在等待"。
    state.snapshot.value={...emptySnapshot(),authenticated:true,selected_id:null,sessions:[]}
    expect(state.needsSelection.value).toBe(true)
    // 已认证、多条会话都没绑定：需要用户自己选，且流量不得伪造。
    state.snapshot.value={...emptySnapshot(),authenticated:true,selected_id:null,sessions:[row('A'),row('B')]}
    expect(state.needsSelection.value).toBe(true)
    expect(state.selected.value).toBeUndefined()
    expect(state.rate.value).toBeUndefined()
    // 绑定之后不再提示，流量按所选会话取。
    state.snapshot.value={...emptySnapshot(),authenticated:true,selected_id:'A',sessions:[row('A'),row('B')],rates:{A:{upload_bps:1024,download_bps:2048,sample_seconds:60}}}
    expect(state.needsSelection.value).toBe(false)
    expect(state.selected.value?.radacctid).toBe('A')
    expect(state.rate.value?.download_bps).toBe(2048)
    state.dispose()
  })
  it('primary action disconnects while online and never chooses an ambiguous session', async () => {
    vi.useFakeTimers()
    const state=createPortalState();await state.initialize()
    const action=state.primaryAction();await vi.runAllTimersAsync();await action
    expect(state.primaryLabel.value).toBe('下线')
    state.snapshot.value.selected_id=null
    await state.primaryAction()
    expect(state.sessionPickerOpen.value).toBe(true)
    expect(state.snapshot.value.selected_id).toBeNull()
    const picking=state.selectForDisconnect('90002')
    await vi.waitFor(()=>expect(state.confirmation.value).not.toBeNull())
    state.answer(true);await picking
    expect(state.primaryLabel.value).toBe('上线')
    state.dispose()
  })
  it('enables logs on demand and disabling removes entries and the dialog', async () => {
    vi.useFakeTimers()
    const state=createPortalState();await state.initialize()
    state.saved.value.credential_store='memory'; state.draft.credential_store='memory'; const first=state.connect();await vi.runAllTimersAsync();await first
    expect(state.logEntries.value).toHaveLength(0)
    await state.updatePreferences({log_enabled:true})
    const second=state.connect();await vi.runAllTimersAsync();await second
    expect(state.logEntries.value.length).toBeGreaterThan(1)
    await state.openLogs();expect(state.logsOpen.value).toBe(true)
    await state.updatePreferences({log_enabled:false})
    expect(state.logsOpen.value).toBe(false)
    expect(state.logEntries.value).toHaveLength(0)
    state.dispose()
  })
  it('changes UI preferences while authenticated without sending credentials', async () => {
    const mock=desktop(),state=createPortalState(mock.bridge);await state.initialize()
    state.snapshot.value.authenticated=true
    state.password.value='never-send-this'
    mock.invoke.mockImplementationOnce(async(_command,args?:Record<string,unknown>)=>args?.value as never)
    await state.updatePreferences({show_sessions:true,theme_mode:'dark'})
    expect(mock.invoke).toHaveBeenLastCalledWith('save_preferences',{value:{show_sessions:true,theme_mode:'dark',log_enabled:false,refresh_policy:'one_minute',traffic_enabled:false}})
    expect(state.saved.value.show_sessions).toBe(true)
    expect(state.password.value).toBe('never-send-this')
    state.dispose()
  })
  it('demo never needs the desktop bridge and remains interactive', async () => {
    vi.useFakeTimers()
    const state = createPortalState()
    await state.initialize()
    state.saved.value.credential_store='memory'; state.draft.credential_store='memory'
    const connecting = state.connect()
    await vi.runAllTimersAsync(); await connecting
    expect(state.snapshot.value.sessions).toHaveLength(2)
    state.simulateUpdate()
    expect(state.rate.value?.download_bps).toBe(1048576)
    const disconnecting = state.disconnect(); state.answer(true); await disconnecting
    expect(state.snapshot.value.sessions).toHaveLength(0)
    expect(state.snapshot.value.authenticated).toBe(false)
    state.dispose()
  })
  it('clears username, password and portal URL when connection fails', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge)
    await state.initialize()
    mock.invoke.mockRejectedValueOnce(new Error('Synthetic connection failure'))
    state.username.value = 'synthetic'; state.password.value = 'not-a-real-password'; state.portalUrl.value = 'http://example.test/portal'; state.saved.value.credential_store='memory'; state.draft.credential_store='memory'
    await state.connect()
    expect(state.username.value).toBe(''); expect(state.password.value).toBe(''); expect(state.portalUrl.value).toBe('')
    expect(state.busy.value).toBe(false)
    expect(state.notice.value).toContain('Synthetic')
    state.dispose()
  })
  // 回归：仅一次会话模式下后端会长期保留未认证的 offline/unknown/error，
  // 每两秒的轮询与后台事件都会把它送进 receive()。过去这里顺手 clearFields()，
  // 用户重新填写后又被下一次轮询清掉，表现为输入内容自己消失。
  it.each(['error', 'offline', 'unknown'])('keeps credentials typed in one-session mode when polling reports %s', async status => {
    vi.useFakeTimers()
    const mock = desktop(status, 'memory'), state = createPortalState(mock.bridge)
    await state.initialize()
    expect(state.snapshot.value.one_session).toBe(true)
    state.username.value = 'synthetic-user'
    state.password.value = 'not-a-real-password'
    state.portalUrl.value = 'http://portal.example.test/'
    await vi.advanceTimersByTimeAsync(2000)
    // 轮询必须真的跑过并接收了这条状态，否则上面的“保留”只是什么都没发生。
    expect(mock.invoke.mock.calls.filter(([command]) => command === 'snapshot').length).toBeGreaterThan(0)
    expect(state.snapshot.value.status).toBe(status)
    expect(state.username.value).toBe('synthetic-user')
    expect(state.password.value).toBe('not-a-real-password')
    expect(state.portalUrl.value).toBe('http://portal.example.test/')
    state.dispose()
  })
  it('keeps credentials retyped after a failed login through the next poll', async () => {
    vi.useFakeTimers()
    const mock = desktop('error', 'memory'), state = createPortalState(mock.bridge)
    await state.initialize()
    state.username.value = 'synthetic-user'; state.password.value = 'not-a-real-password'; state.portalUrl.value = 'http://portal.example.test/'
    mock.invoke.mockRejectedValueOnce(new Error('Synthetic rejection'))
    await state.connect()
    // 主动认证失败仍按既有语义释放临时凭据。
    expect(state.username.value).toBe(''); expect(state.password.value).toBe(''); expect(state.portalUrl.value).toBe('')
    const polls = mock.invoke.mock.calls.filter(([command]) => command === 'snapshot').length
    state.username.value = 'synthetic-user'; state.password.value = 'not-a-real-password'; state.portalUrl.value = 'http://portal.example.test/'
    await vi.advanceTimersByTimeAsync(2000)
    expect(mock.invoke.mock.calls.filter(([command]) => command === 'snapshot').length).toBeGreaterThan(polls)
    expect(state.snapshot.value.status).toBe('error')
    expect(state.username.value).toBe('synthetic-user')
    expect(state.password.value).toBe('not-a-real-password')
    expect(state.portalUrl.value).toBe('http://portal.example.test/')
    state.dispose()
  })
  it('still releases one-session credentials when an explicit login completes', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge)
    await state.initialize()
    state.saved.value.credential_store = 'memory'; state.draft.credential_store = 'memory'
    state.username.value = 'synthetic-user'; state.password.value = 'not-a-real-password'; state.portalUrl.value = 'http://portal.example.test/'
    await state.connect()
    expect(state.snapshot.value.status).toBe('accepted')
    expect(state.username.value).toBe(''); expect(state.password.value).toBe(''); expect(state.portalUrl.value).toBe('')
    state.dispose()
  })
  it('never chooses the first of multiple sessions and disconnects only selected ID', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge)
    await state.initialize()
    state.snapshot.value = { ...emptySnapshot(), authenticated: true, sessions: [row('A'), row('B')] }
    await state.disconnect()
    expect(mock.invoke).not.toHaveBeenCalledWith('disconnect', expect.anything())
    state.snapshot.value.selected_id = 'B'
    const action = state.disconnect(); state.answer(true); await action
    expect(mock.invoke).toHaveBeenCalledWith('disconnect', { id: 'B' })
    state.dispose()
  })
  it('keeps redial and background service choices when one-session storage is selected', async () => {
    const value = normalizeSettings({ ...defaultSettings(), credential_store: 'memory', username: 'synthetic', reconnect_mode: 'new_session', service_enabled: true })
    expect(value.username).toBe(''); expect(value.credential_store).toBe('memory'); expect(value.reconnect_mode).toBe('new_session'); expect(value.service_enabled).toBe(true)
    const state = createPortalState(); await state.initialize()
    state.draft.credential_store = 'system'; state.draft.reconnect_mode = 'terminate_and_reconnect'; state.draft.service_enabled = true
    state.draft.credential_store = 'memory'
    expect(state.draft.reconnect_mode).toBe('terminate_and_reconnect')
    expect(state.draft.service_enabled).toBe(true)
    state.dispose()
  })
  it('retains input while editing and releases event listeners and secrets on disposal', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge)
    await state.initialize()
    state.password.value = 'not-a-real-password'
    expect(state.password.value).not.toBe('')
    state.dispose()
    expect(state.password.value).toBe('')
    mock.stops.forEach(stop => expect(stop).toHaveBeenCalledOnce())
  })
  it('keeps unavailable accounting rate distinct from zero', () => {
    expect(formatRate(null)).toBe('等待更新')
    expect(formatRate(0)).toBe('0.0 B/s')
    expect(formatRate(1048576)).toBe('1.0 MiB/s')
  })
  it('keeps the last rate and says so only when a manual refresh returns identical accounting', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    state.saved.value.traffic_enabled = true
    const online = { ...emptySnapshot(), status: 'accepted', authenticated: true, sessions: [row('A')], selected_id: 'A' }
    mock.invoke.mockResolvedValueOnce({ ...online, rates: { A: { upload_bps: 2048, download_bps: 40960, sample_seconds: 60 } } } as never)
    await state.refresh()
    expect(state.rate.value?.download_bps).toBe(40960)
    expect(state.notice.value).toBe('')
    // 后台计费尚未更新：接口返回空速率，界面沿用上一次算出的结果而不是回到“等待更新”。
    const pendingRate = { upload_bps: null, download_bps: null, sample_seconds: null }
    mock.invoke.mockResolvedValueOnce({ ...online, rates: { A: pendingRate } } as never)
    await state.refresh()
    expect(state.rate.value?.download_bps).toBe(40960)
    expect(state.notice.value).toBe('刷新成功，后台返回一致流量')
    // 后台真的更新后立刻换成新值，且不产生新的提示。
    state.notice.value = ''
    mock.invoke.mockResolvedValueOnce({ ...online, rates: { A: { upload_bps: 4096, download_bps: 81920, sample_seconds: 60 } } } as never)
    await state.refresh()
    expect(state.rate.value?.download_bps).toBe(81920)
    expect(state.notice.value).toBe('')
    state.dispose()
  })
  it('never announces identical accounting on the first result or while polling in the background', async () => {
    vi.useFakeTimers()
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    state.saved.value.traffic_enabled = true
    const online = { ...emptySnapshot(), status: 'accepted', authenticated: true, sessions: [row('A')], selected_id: 'A' }
    const pendingRate = { upload_bps: null, download_bps: null, sample_seconds: null }
    // 首次连接后还没有“上一个结果”，此时一致也必须保持安静。
    mock.invoke.mockResolvedValueOnce({ ...online, rates: { A: pendingRate } } as never)
    await state.refresh()
    expect(state.notice.value).toBe('')
    expect(formatRate(state.rate.value?.download_bps)).toBe('等待更新')
    mock.invoke.mockResolvedValueOnce({ ...online, rates: { A: { upload_bps: 2048, download_bps: 40960, sample_seconds: 60 } } } as never)
    await state.refresh()
    expect(state.rate.value?.download_bps).toBe(40960)
    state.notice.value = ''
    // 后台轮询同样沿用旧值，但不弹提示。
    mock.invoke.mockImplementation(async (command: string) => command === 'snapshot' ? { ...online, rates: { A: pendingRate } } : emptySnapshot())
    await vi.advanceTimersByTimeAsync(2000)
    expect(state.rate.value?.download_bps).toBe(40960)
    expect(state.notice.value).toBe('')
    state.dispose()
  })
  it('diagnose sends no credentials and keeps the latency on the card while staying silent', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    state.password.value = 'never-send-this'
    mock.invoke.mockResolvedValueOnce({ auth: 'reachable', auth_latency_ms: 3, internet_direct: 'reachable', internet: 'reachable', internet_latency_ms: 38 } as never)
    await state.diagnose()
    expect(mock.invoke).toHaveBeenLastCalledWith('diagnose')
    expect(state.diagnostic.value?.internet_latency_ms).toBe(38)
    expect(state.diagnosing.value).toBe(false)
    expect(state.busy.value).toBe(false)
    expect(state.password.value).toBe('never-send-this')
    state.dispose()
  })
  it('diagnose distinguishes an unreachable auth server from an unreachable internet', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    mock.invoke.mockResolvedValueOnce({ auth: 'unreachable', auth_latency_ms: null, internet_direct: 'unreachable', internet: 'unreachable', internet_latency_ms: null } as never)
    await state.diagnose()
    expect(state.notice.value).toContain('无法访问到认证服务器')
    mock.invoke.mockResolvedValueOnce({ auth: 'reachable', auth_latency_ms: 5, internet_direct: 'unreachable', internet: 'unreachable', internet_latency_ms: null } as never)
    await state.diagnose()
    expect(state.notice.value).toContain('运营商外网不可达')
    mock.invoke.mockResolvedValueOnce({ auth: 'reachable', auth_latency_ms: 5, internet_direct: 'unreachable', internet: 'captive', internet_latency_ms: 9 } as never)
    await state.diagnose()
    expect(state.notice.value).toContain('认证页')
    state.dispose()
  })
  it('a proxied path keeps the internet reachable and explains it in a notice', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    // 本机 TUN 代理挡住了物理网卡的直连探测，系统路由仍能上网：
    // 只把直连那一档记为未通过并给一条说明提示，绝不弹“校园网可能存在故障”。
    mock.invoke.mockResolvedValueOnce({ auth: 'reachable', auth_latency_ms: 3, internet_direct: 'unreachable', internet: 'reachable', internet_latency_ms: 46 } as never)
    await state.diagnose()
    expect(state.notice.value).toBe('直连探测未通过，当前经代理或 VPN 上网')
    expect(state.diagnostic.value?.internet).toBe('reachable')
    expect(state.diagnostic.value?.internet_direct).toBe('unreachable')
    expect(state.diagnostic.value?.internet_latency_ms).toBe(46)
    state.dispose()
  })
  it('clearing the local session goes straight to the daemon without asking first', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    state.username.value = 'alice'; state.password.value = 'secret'; state.portalUrl.value = 'http://portal.example/'
    mock.invoke.mockResolvedValueOnce({ ...emptySnapshot(), message: '本地会话已清除' } as never)
    await state.forget()
    expect(mock.invoke).toHaveBeenLastCalledWith('forget')
    expect(state.confirmation.value).toBeNull()
    expect(state.notice.value).toBe('')
    expect(state.username.value).toBe('')
    expect(state.password.value).toBe('')
    expect(state.portalUrl.value).toBe('')
    expect(state.busy.value).toBe(false)
    state.dispose()
  })
  it('checks for an update when the about page opens and only opens the release page', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    state.password.value = 'never-send-this'
    mock.invoke.mockResolvedValueOnce({ current: '0.5.0', latest: 'v0.6.0', newer: true, url: 'https://github.com/miaoermua/DGCU-Net-Portal/releases/tag/v0.6.0' } as never)
    state.page.value = 2
    await vi.waitFor(() => expect(state.updatePhase.value).toBe('available'))
    expect(mock.invoke).toHaveBeenLastCalledWith('check_update')
    expect(state.updateInfo.value?.latest).toBe('v0.6.0')
    // 更新动作只打开 Release 页面：不下载、不替换自身，也不带任何账号数据。
    await state.openUpdate()
    expect(mock.invoke).toHaveBeenLastCalledWith('open_external', { url: 'https://github.com/miaoermua/DGCU-Net-Portal/releases/tag/v0.6.0' })
    expect(state.password.value).toBe('never-send-this')
    state.dispose()
  })
  it('surfaces the ten second timeout as a retryable error and recovers on retry', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    mock.invoke.mockRejectedValueOnce('检测更新超时，请检查网络后重试')
    await state.checkUpdate()
    expect(state.updatePhase.value).toBe('error')
    expect(state.updateError.value).toContain('超时')
    // 检查更新失败不占用全局忙碌状态，网络页的按钮该用还能用。
    expect(state.busy.value).toBe(false)
    mock.invoke.mockResolvedValueOnce({ current: '0.5.0', latest: 'v0.5.0', newer: false, url: 'https://github.com/miaoermua/DGCU-Net-Portal/releases' } as never)
    await state.checkUpdate()
    expect(state.updatePhase.value).toBe('latest')
    state.dispose()
  })
  it('reuses a recent result instead of spending the unauthenticated github quota', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    mock.invoke.mockResolvedValue({ current: '0.5.0', latest: 'v0.5.0', newer: false, url: 'https://github.com/miaoermua/DGCU-Net-Portal/releases' } as never)
    await state.checkUpdate()
    await state.checkUpdate()
    expect(mock.invoke.mock.calls.filter(([command]) => command === 'check_update')).toHaveLength(1)
    state.dispose()
  })
  it('always refetches when the refresh button forces a check', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    mock.invoke.mockResolvedValue({ current: '0.5.0', latest: '0.5.0', newer: false, url: 'https://github.com/miaoermua/DGCU-Net-Portal/releases' } as never)
    await state.checkUpdate()
    // 关于页的「刷新」按钮常驻，一分钟内也必须真查一次，否则刚自动查完时按了没反应。
    await state.checkUpdate(true)
    expect(mock.invoke.mock.calls.filter(([command]) => command === 'check_update')).toHaveLength(2)
    state.dispose()
  })
  it('never checks for an update in demo mode', async () => {
    const state = createPortalState(); await state.initialize()
    await state.checkUpdate()
    expect(state.updatePhase.value).toBe('idle')
    state.dispose()
  })
})
