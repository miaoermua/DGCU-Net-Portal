import { afterEach, describe, expect, it, vi } from 'vitest'
import { createPortalState, defaultSettings, emptySnapshot, formatRate, normalizeSettings } from './usePortal'
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
  it('demo never needs the desktop bridge and remains interactive', async () => {
    vi.useFakeTimers()
    const state = createPortalState()
    await state.initialize()
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
    state.username.value = 'synthetic'; state.password.value = 'not-a-real-password'; state.portalUrl.value = 'http://example.test/portal'
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
    const value = normalizeSettings({ ...defaultSettings(), username: 'synthetic', remember_account: true, auto_redial: true, service_enabled: true })
    expect(value.username).toBe(''); expect(value.remember_account).toBe(false); expect(value.auto_redial).toBe(false); expect(value.service_enabled).toBe(false)
    const state = createPortalState(); await state.initialize()
    state.draft.one_session = false; state.draft.auto_redial = true; state.draft.one_session = true
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
