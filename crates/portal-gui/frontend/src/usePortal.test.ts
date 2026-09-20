import { afterEach, describe, expect, it, vi } from 'vitest'
import { createPortalState, defaultSettings, emptySnapshot, formatRate, normalizeSettings } from './usePortal'
import { licenseGroups } from './licenses'
import type { DesktopBridge, Session } from './types'

const row = (id: string): Session => ({ radacctid: id, username: 'synthetic-user', framedipaddress: '192.0.2.1', acctstarttime: '', acctsessiontime: 60, acctinputoctets: 600, acctoutputoctets: 6000 })
function desktop() {
  const stops = [vi.fn(), vi.fn(), vi.fn()]
  const invoke = vi.fn(async (command: string) => {
    if (command === 'initial') return { demo: false, settings: defaultSettings() }
    if (command === 'snapshot') return emptySnapshot()
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
  it('normalizes one-session settings before sending them to Rust', async () => {
    const value = normalizeSettings({ ...defaultSettings(), credential_store: 'memory', username: 'synthetic', auto_redial: true, service_enabled: true })
    expect(value.username).toBe(''); expect(value.credential_store).toBe('memory'); expect(value.auto_redial).toBe(false); expect(value.service_enabled).toBe(false)
    const state = createPortalState(); await state.initialize()
    state.draft.credential_store = 'system'; state.draft.auto_redial = true; state.draft.credential_store = 'memory'
    expect(state.draft.auto_redial).toBe(false)
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
  it('canceling a confirmation never sends disconnect', async () => {
    const mock = desktop(), state = createPortalState(mock.bridge); await state.initialize()
    state.snapshot.value = { ...emptySnapshot(), authenticated: true, sessions: [row('A')], selected_id: 'A' }
    const action = state.disconnect(); state.answer(false); await action
    expect(mock.invoke).not.toHaveBeenCalledWith('disconnect', expect.anything())
    state.dispose()
  })
})
