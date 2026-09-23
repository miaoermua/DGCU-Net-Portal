<script setup lang="ts">
import { computed, nextTick, onUnmounted, ref, watch } from 'vue'
import { MotionConfig } from 'motion-v'
import { MiuixBasicComponent, MiuixButton, MiuixCard, MiuixDropdownPreference, MiuixIcon, MiuixIconButton, MiuixProgressIndicator, MiuixSwitchPreference, MiuixSnackbarHost, showSnackbar, setThemeMode } from 'miuix-vue'
import { Clear, Close, File, Forward, Info, Link, Settings as SettingsIcon } from 'miuix-vue/icons'
import { usePortal, formatBytes, formatRate, formatDuration, mask } from './usePortal'
import { licenseGroups, licenseNotice } from './licenses'
import xiaoweiLogo from './assets/xiaowei.png'

const { demo, busy, ready, page, draft, saved, snapshot, username, password, portalUrl, networkInterfaces, selected, rate, isOnline, title, phase, notice, confirmation, answer, connect, refresh, select, forget, save, openSite, simulateUpdate, preferencesBusy, logEntries, logsOpen, sessionPickerOpen, primaryLabel, primaryAction, selectForDisconnect, openLogs, clearLogs, updatePreferences, version, openRepository, openUrl, refreshInterfaces, close, hasUnsavedConnectionSettings, serviceRunning } = usePortal()
watch(() => saved.value.theme_mode, value => setThemeMode(value), { immediate: true })
const styleNonce = document.querySelector<HTMLStyleElement>('#motion-csp')?.nonce || undefined
const locked = computed(() => busy.value || !ready.value)
// Settings are persisted through the daemon and can be edited while a session
// is online; changing them does not terminate the current daemon session.
const settingsLocked = computed(() => locked.value)
const dialog = ref<HTMLDialogElement>()
const logDialog = ref<HTMLDialogElement>(), sessionDialog = ref<HTMLDialogElement>(), licenseDialog = ref<HTMLDialogElement>()
let previousFocus: HTMLElement | null = null
let logFocus: HTMLElement | null = null, sessionFocus: HTMLElement | null = null, licenseFocus: HTMLElement | null = null
watch(notice, message => { if (message) { void showSnackbar({ message, withDismissAction: true, duration: 6000 }); notice.value = '' } })
watch(confirmation, async value => {
  if (value) { previousFocus = document.activeElement as HTMLElement; await nextTick(); dialog.value?.showModal(); dialog.value?.querySelector<HTMLButtonElement>('button')?.focus() }
  else { dialog.value?.close(); previousFocus?.focus() }
})
onUnmounted(() => dialog.value?.close())
watch(logsOpen, async open => {
  if (open) { logFocus = document.activeElement as HTMLElement; await nextTick(); logDialog.value?.showModal(); logDialog.value?.querySelector<HTMLButtonElement>('button')?.focus() }
  else { logDialog.value?.close(); logFocus?.focus() }
})
watch(sessionPickerOpen, async open => {
  if (open) { sessionFocus = document.activeElement as HTMLElement; await nextTick(); sessionDialog.value?.showModal() }
  else { sessionDialog.value?.close(); sessionFocus?.focus() }
})
const licensesOpen = ref(false)
const licenseCount = computed(() => licenseGroups.reduce((total, group) => total + group.entries.length, 0))
watch(licensesOpen, async open => {
  if (open) { licenseFocus = document.activeElement as HTMLElement; await nextTick(); licenseDialog.value?.showModal(); licenseDialog.value?.querySelector<HTMLButtonElement>('button')?.focus() }
  else { licenseDialog.value?.close(); licenseFocus?.focus() }
})
onUnmounted(() => { logDialog.value?.close(); sessionDialog.value?.close(); licenseDialog.value?.close() })
const logTime = (value: number) => new Date(value).toLocaleTimeString('zh-CN', { hour12: false })
const selectedInterface = computed(() => networkInterfaces.value.find(item => item.name === draft.interface_name) || networkInterfaces.value.find(item => !item.internal && item.ipv4 && item.mac))
const credentialStoreItems = ['系统凭证（推荐）', '配置文件（明文，仅测试）', '仅一次会话']
const credentialStoreIndex = computed({ get: () => ({ system: 0, file: 1, memory: 2 }[draft.credential_store]), set: (value: number) => { draft.credential_store = (['system', 'file', 'memory'] as const)[value] ?? 'system' } })
const reconnectItems = ['禁用', '新建后上线', '终止后重上']
const reconnectIndex = computed({ get: () => ({ disabled: 0, new_session: 1, terminate_and_reconnect: 2 }[draft.reconnect_mode]), set: (value: number) => { draft.reconnect_mode = (['disabled', 'new_session', 'terminate_and_reconnect'] as const)[value] ?? 'disabled' } })
const refreshEnabled = computed({ get: () => draft.refresh_policy !== 'disabled', set: (value: boolean) => { draft.refresh_policy = value ? 'one_minute' : 'disabled'; void updatePreferences({ refresh_policy: draft.refresh_policy }) } })
const jitterItems = ['低（±5%）', '中（±10%）', '高（±20%）', '禁用（0%）']
const jitterIndex = computed({ get: () => ({ low: 0, medium: 1, high: 2, disabled: 3 }[draft.poll_jitter]), set: (value: number) => { draft.poll_jitter = (['low', 'medium', 'high', 'disabled'] as const)[value] ?? 'low' } })
const themeItems = ['跟随系统', '浅色', '深色']
const themeIndex = computed({ get: () => ({ system: 0, light: 1, dark: 2 }[saved.value.theme_mode]), set: (value: number) => { void updatePreferences({ theme_mode: (['system', 'light', 'dark'] as const)[value] ?? 'system' }) } })
const navIcons = [Link, SettingsIcon, Info]
const settingsNoticeShown = ref(false)
let settingsApplyTimer: ReturnType<typeof setTimeout> | undefined
const watchedSettings = computed(() => [draft.server, draft.auth_url, draft.probe_url, draft.paip, draft.basip, draft.probe_enabled, draft.refresh_policy, draft.poll_jitter, draft.traffic_enabled, draft.credential_store, draft.interface_name, draft.bypass_proxy, draft.reconnect_mode, draft.tray_startup, draft.service_enabled].join('|'))
const advancedSettingsDirty = computed(() => [draft.server, draft.auth_url, draft.probe_url, draft.paip, draft.basip].some((value, index) => value !== [saved.value.server, saved.value.auth_url, saved.value.probe_url, saved.value.paip, saved.value.basip][index]))
watch(watchedSettings, () => {
  if (!ready.value || busy.value || preferencesBusy.value || !hasUnsavedConnectionSettings.value) return
  if (advancedSettingsDirty.value) {
    if (!settingsNoticeShown.value) {
      settingsNoticeShown.value = true
      notice.value = '高级连接参数已修改，请点击顶部保存设置后应用'
    }
    return
  }
  if (serviceRunning.value) {
    if (!settingsNoticeShown.value) {
      settingsNoticeShown.value = true
      notice.value = '认证服务正在运行，设置尚未应用；停止服务后会自动保存，或点击顶部保存设置'
    }
    return
  }
  if (settingsApplyTimer) clearTimeout(settingsApplyTimer)
  settingsApplyTimer = setTimeout(() => { void save() }, 500)
})
watch(serviceRunning, running => {
  if (!running) {
    settingsNoticeShown.value = false
    if (hasUnsavedConnectionSettings.value && !advancedSettingsDirty.value) {
      if (settingsApplyTimer) clearTimeout(settingsApplyTimer)
      settingsApplyTimer = setTimeout(() => { void save() }, 500)
    }
  }
})
onUnmounted(() => { if (settingsApplyTimer) clearTimeout(settingsApplyTimer) })
const interfaceItems = computed(() => [{ text: '自动选择活动网卡', summary: '使用第一个可用 IPv4/MAC 接口' }, ...networkInterfaces.value.map(item => ({ text: item.name, summary: `${item.ipv4 || '无 IPv4'} · ${item.mac || '无 MAC'}`, disabled: !item.ipv4 || !item.mac }))])
const interfaceIndex = computed({ get: () => { const index = networkInterfaces.value.findIndex(item => item.name === draft.interface_name); return index < 0 ? 0 : index + 1 }, set: (value: number) => { draft.interface_name = value === 0 ? '' : (networkInterfaces.value[value - 1]?.name ?? '') } })
const interfaceSummary = computed(() => {
  if (!selectedInterface.value) return demo ? 'Demo 不读取本机网卡' : '未找到带 IPv4 和 MAC 的活动网卡'
  const address = `${selectedInterface.value.ipv4 || '无 IPv4'} · ${selectedInterface.value.mac || '无 MAC'}`
  return draft.interface_name ? `${selectedInterface.value.name} · ${address}` : `自动选择 · ${address}`
})
</script>

<template>
  <MotionConfig :nonce="styleNonce" reduced-motion="user">
  <div class="portal-app">
    <nav class="top-navigation gnome-navigation" aria-label="主导航"><button v-for="(label, index) in ['网络', '设置', '关于']" :key="label" type="button" :class="{ active: page === index }" @click="page = index"><MiuixIcon class="m-navigation-bar__icon" :icon="navIcons[index]" :size="20" /><span>{{ label }}</span></button></nav>
    <div v-if="hasUnsavedConnectionSettings" class="mobile-save-bar"><span>{{ advancedSettingsDirty ? '高级连接参数已修改，请保存后应用' : serviceRunning ? '认证服务正在运行，设置将在下次认证应用' : '设置正在实时保存' }}</span><MiuixButton v-if="advancedSettingsDirty || serviceRunning" type="primary" :disabled="settingsLocked" @click="save">保存设置</MiuixButton></div>

    <main>
      <section v-if="page === 0" class="overview" aria-label="网络概览">
        <MiuixCard class="connection-card">
          <div class="connection-main"><div class="status-icon" :class="{ online: isOnline, authenticating: !!phase }" aria-hidden="true"><MiuixProgressIndicator v-if="phase" type="infinite" :size="25" color="currentColor" /><MiuixIcon v-else :icon="Link" weight="demibold" :size="25" /></div><div class="connection-copy"><div class="status-line"><h2>{{ phase || title }}</h2><span v-if="saved.credential_store === 'memory'" class="privacy-tag">仅一次会话</span></div><p class="status-message" role="status">{{ phase ? '请稍候，等待认证系统返回结果' : snapshot.message }}</p></div></div>
          <div class="connection-actions"><MiuixButton type="primary" :disabled="locked" @click="primaryAction">{{ primaryLabel }}</MiuixButton><MiuixIconButton class="shutdown-icon" :disabled="locked" aria-label="退出并停止后台服务" title="退出并停止后台服务" @click="close"><MiuixIcon :icon="Close" :size="18" /></MiuixIconButton><MiuixButton class="auth-site-icon" :disabled="locked" aria-label="打开认证后台" title="打开认证后台" @click="openSite"><MiuixIcon :icon="Forward" :size="18" /></MiuixButton></div>
        </MiuixCard>
        <div class="traffic-grid" aria-label="后台计费流量">
          <MiuixCard class="metric"><span class="metric-label">↓ 累计下载</span><strong>{{ saved.traffic_enabled && selected ? formatBytes(selected.acctoutputoctets) : '已关闭' }}</strong><span class="metric-note">{{ saved.traffic_enabled ? '所选后台会话' : '设置中开启后台流量统计' }}</span></MiuixCard>
          <MiuixCard class="metric"><span class="metric-label">↑ 累计上传</span><strong>{{ saved.traffic_enabled && selected ? formatBytes(selected.acctinputoctets) : '已关闭' }}</strong><span class="metric-note">{{ saved.traffic_enabled ? '所选后台会话' : '设置中开启后台流量统计' }}</span></MiuixCard>
          <MiuixCard class="metric"><span class="metric-label">↓ 区间下载速率</span><strong :class="{ waiting: !saved.traffic_enabled || rate?.download_bps == null }">{{ saved.traffic_enabled ? formatRate(rate?.download_bps) : '已关闭' }}</strong><span class="metric-note">{{ saved.traffic_enabled ? (rate?.sample_seconds ? `最近 ${rate.sample_seconds} 秒平均` : '等待后台计费更新') : '设置中开启后台流量统计' }}</span></MiuixCard>
          <MiuixCard class="metric"><span class="metric-label">↑ 区间上传速率</span><strong :class="{ waiting: !saved.traffic_enabled || rate?.upload_bps == null }">{{ saved.traffic_enabled ? formatRate(rate?.upload_bps) : '已关闭' }}</strong><span class="metric-note">{{ saved.traffic_enabled ? '按后台计费时间计算' : '设置中开启后台流量统计' }}</span></MiuixCard>
        </div>
        <div v-if="saved.show_sessions" class="section-heading"><h3>管理会话 <span class="count">{{ snapshot.sessions.length }}</span></h3><div class="row-actions"><MiuixButton :disabled="locked" @click="connect(true)">仅登录后台</MiuixButton><MiuixButton :disabled="locked || !snapshot.authenticated" @click="refresh">刷新</MiuixButton></div></div>
        <MiuixCard v-if="saved.show_sessions" class="session-list">
          <div v-if="!snapshot.sessions.length" class="empty-state"><strong>暂无在线会话</strong><span>上线或登录后台后，即可查看和管理连接。</span><MiuixButton :disabled="locked" @click="page = 1">填写登录信息</MiuixButton></div>
          <label v-for="row in snapshot.sessions" v-else :key="row.radacctid" class="session-row" :class="{ selected: row.radacctid === snapshot.selected_id }"><input type="radio" name="session" :checked="row.radacctid === snapshot.selected_id" :disabled="locked" :aria-label="`选择会话 ${row.radacctid}`" @change="select(row.radacctid)"><div class="session-identity"><strong>{{ mask(row.username) }} <span v-if="row.radacctid === snapshot.selected_id" class="selected-tag">已选择</span></strong><span>{{ row.framedipaddress || 'IP 未上报' }} · #{{ row.radacctid }}</span></div><span class="session-duration">{{ formatDuration(row.acctsessiontime) }}</span></label>
        </MiuixCard>
        <div class="overview-footer"><span>{{ demo ? '模拟数据 · 不连接校园网' : '后台计费数据 · 仅读取所选网卡 IP / MAC，不采集网卡流量' }}</span><div class="row-actions"><MiuixButton v-if="demo" :disabled="locked || !snapshot.sessions.length" @click="simulateUpdate">模拟流量更新</MiuixButton><MiuixButton v-if="saved.show_sessions" class="clear-session-button" :disabled="locked" aria-label="清除本地会话" title="清除本地会话" @click="forget"><MiuixIcon :icon="Clear" :size="16" /><span>清除本地会话</span></MiuixButton><MiuixButton v-if="!snapshot.authenticated" :disabled="locked" @click="page = 1">登录设置</MiuixButton></div></div>
      </section>

      <section v-else-if="page === 1" class="settings-page" aria-label="偏好设置">
        <p v-if="settingsLocked && !busy" class="inline-note">连接与账号设置需先结束本地会话后才能修改。</p>
        <h3 class="group-heading">登录项</h3>
        <MiuixCard>
          <form class="credential-fields" autocomplete="off" @submit.prevent="connect(false)"><label class="field">账号<input v-model="username" :disabled="locked" autocomplete="off" placeholder="校园网账号" spellcheck="false"></label><label class="field">密码<input v-model="password" :disabled="locked" type="password" autocomplete="new-password" placeholder="仅当前会话使用"><button type="submit" class="sr-only" :disabled="locked" tabindex="-1">上线</button></label></form>
          <MiuixDropdownPreference v-model="credentialStoreIndex" title="会话账户保存方式" summary="默认使用系统凭据，更安全" :items="credentialStoreItems" :disabled="settingsLocked" />
          <p v-if="draft.credential_store === 'file'" class="security-note">配置文件会以明文保存密码，仅建议用于测试或系统没有可用凭据库的设备。</p>
          <p v-if="draft.credential_store === 'memory'" class="security-note">仅一次会话不会保存密码，结束后释放本地凭据并关闭自动重拨。</p>
        </MiuixCard>
        <h3 class="group-heading">认证</h3>
        <MiuixCard>
          <div class="interface-picker"><div class="interface-picker-heading"><span>使用所选网卡的 IPv4 和 MAC 生成 Portal 参数</span><MiuixButton :disabled="!ready || preferencesBusy || demo" @click="refreshInterfaces">刷新</MiuixButton></div><MiuixDropdownPreference v-model="interfaceIndex" title="认证网卡" :summary="interfaceSummary" :items="interfaceItems" :disabled="settingsLocked" /></div>
          <MiuixSwitchPreference v-model="draft.bypass_proxy" title="绕过程序代理" summary="直连认证服务器；TUN / VPN 路由仍由系统决定" :disabled="settingsLocked" />
          <MiuixSwitchPreference v-model="draft.probe_enabled" title="认证失败后自动探测" summary="默认先按 DGCU-Net-Portal 模板提交，模板失败后再探测公共 HTTP 地址" :disabled="settingsLocked" />
          <MiuixDropdownPreference v-model="reconnectIndex" title="掉线重拨" summary="每 5 秒检测一次；连续 3 次检测不到会话后按所选模式重连" :items="reconnectItems" :disabled="settingsLocked || draft.credential_store === 'memory'" />
          <MiuixDropdownPreference v-model="jitterIndex" title="轮询频率抖动" summary="分别给 5 秒掉线检测和 1 分钟后台刷新增加时间抖动，可降低风控特征" :items="jitterItems" :disabled="settingsLocked" />
        </MiuixCard>
        <h3 class="group-heading">系统</h3>
        <MiuixCard>
          <MiuixSwitchPreference v-model="draft.tray_startup" title="托盘启动" summary="下次启动隐藏窗口，可从托盘打开" :disabled="settingsLocked" />
          <MiuixSwitchPreference v-model="draft.service_enabled" title="写入用户后台服务" summary="当前用户登录系统时启动 portal-cli daemon" :disabled="settingsLocked || draft.credential_store === 'memory'" />
        </MiuixCard>
        <h3 class="group-heading">界面</h3>
        <MiuixCard>
          <MiuixDropdownPreference v-model="themeIndex" title="显示模式" summary="默认跟随系统外观" :items="themeItems" :disabled="!ready || preferencesBusy" />
          <MiuixSwitchPreference :model-value="saved.show_sessions" title="管理会话" summary="在网络页显示会话列表和管理操作，默认隐藏" :disabled="!ready || preferencesBusy" @update:model-value="updatePreferences({ show_sessions: $event })" />
          <MiuixSwitchPreference v-model="refreshEnabled" title="后台刷新" summary="按服务端约 1 分钟的周期读取会话；抖动由认证中的轮询频率设置控制" :disabled="!ready || preferencesBusy" />
          <MiuixSwitchPreference :model-value="saved.traffic_enabled" title="后台流量统计" summary="默认关闭；开启后计算后台累计字节的区间速率" :disabled="!ready || preferencesBusy" @update:model-value="updatePreferences({ traffic_enabled: $event })" />
          <MiuixSwitchPreference :model-value="saved.log_enabled" title="开启日志" summary="只在内存保留最近 300 条脱敏事件，默认关闭" :disabled="!ready || preferencesBusy" @update:model-value="updatePreferences({ log_enabled: $event })" />
          <button v-if="saved.log_enabled" class="log-entry preference-action" :disabled="!ready || preferencesBusy" @click="openLogs"><span><strong>查看 portal-cli 日志</strong><small>{{ demo ? '演示模式显示模拟事件' : '来自 portal-cli daemon 的统一日志' }}</small></span><span aria-hidden="true">›</span></button>
        </MiuixCard>
        <h3 class="group-heading">高级</h3>
        <details class="advanced-settings"><summary>连接地址和探测参数</summary><MiuixCard class="advanced-fields"><label class="field">HTTP 探测地址<input v-model="draft.probe_url" :disabled="settingsLocked" spellcheck="false"></label><label class="field">认证后台<input v-model="draft.auth_url" :disabled="settingsLocked" spellcheck="false"></label><label class="field">认证服务器<input v-model="draft.server" :disabled="settingsLocked" spellcheck="false"></label><label class="field">Portal URL（可选）<input v-model="portalUrl" :disabled="locked" autocomplete="off" spellcheck="false" placeholder="留空使用 DGCU 模板，或粘贴当前网络的认证网址"></label><label class="field">paip（Portal 参数）<input v-model="draft.paip" :disabled="settingsLocked" spellcheck="false" placeholder="172.18.100.65"></label><label class="field">basip 覆盖值（可选）<input v-model="draft.basip" :disabled="settingsLocked" spellcheck="false" placeholder="留空使用认证页返回值"></label><p class="advanced-note">paip 默认是 172.18.100.65，会写入 Portal URL 查询参数。basip 默认留空，程序会使用认证入口表单返回的隐藏值；只有填写覆盖值时才替换服务器返回值。两者都必须是 IP 地址。</p></MiuixCard></details>
      </section>

      <section v-else class="about-page" aria-label="关于"><MiuixCard class="about-card"><div class="about-brand"><img :src="xiaoweiLogo" alt="小薇" class="about-logo"><div><h2>DGCU-Net-Portal</h2><p>v{{ version }} · {{ demo ? '演示模式' : '测试版' }}</p></div></div><MiuixBasicComponent title="GitHub 仓库" summary="miaoermua/DGCU-Net-Portal" :disabled="locked" clickable @click="openRepository"><template #start><svg class="about-entry-icon github-entry-icon" aria-hidden="true" viewBox="0 0 24 24"><path d="M12 2.5a9.5 9.5 0 0 0-3 18.52c.47.09.64-.2.64-.45v-1.6c-2.6.57-3.15-1.1-3.15-1.1-.43-1.1-1.05-1.4-1.05-1.4-.86-.59.07-.58.07-.58.95.07 1.45.98 1.45.98.85 1.45 2.23 1.03 2.78.79.09-.62.33-1.03.6-1.27-2.08-.24-4.27-1.04-4.27-4.63 0-1.02.36-1.85.98-2.5-.1-.24-.42-1.2.09-2.48 0 0 .8-.25 2.62.96a9.1 9.1 0 0 1 4.77 0c1.82-1.21 2.62-.96 2.62-.96.51 1.28.19 2.24.09 2.48.61.65.98 1.48.98 2.5 0 3.6-2.2 4.39-4.28 4.63.34.29.64.84.64 1.7v2.48c0 .25.17.54.65.45A9.5 9.5 0 0 0 12 2.5Z"/></svg></template><template #end><span class="about-entry-arrow" aria-hidden="true">↗</span></template></MiuixBasicComponent><MiuixBasicComponent title="开源软件声明" :summary="`${licenseCount} 个开源项目 · 名称 / 地址 / 许可证`" :disabled="locked" clickable @click="licensesOpen = true"><template #start><MiuixIcon class="about-entry-icon" :icon="File" :size="20" /></template><template #end><span class="about-entry-arrow" aria-hidden="true">›</span></template></MiuixBasicComponent></MiuixCard></section>
    </main>
    <dialog ref="dialog" class="confirm-dialog" aria-labelledby="dialog-title" @cancel.prevent="answer(false)"><template v-if="confirmation"><h2 id="dialog-title">{{ confirmation.title }}</h2><p>{{ confirmation.text }}</p><div class="dialog-actions"><MiuixButton @click="answer(false)">取消</MiuixButton><MiuixButton type="primary" @click="answer(true)">{{ confirmation.label }}</MiuixButton></div></template></dialog>
    <dialog ref="logDialog" class="logs-dialog" aria-labelledby="logs-title" @cancel.prevent="logsOpen = false"><div class="logs-heading"><h2 id="logs-title">portal-cli 日志 <span v-if="demo" class="mode-badge">模拟</span></h2><MiuixIconButton class="dialog-icon-button" aria-label="关闭日志" title="关闭日志" @click="logsOpen = false"><MiuixIcon :icon="Close" :size="18" /></MiuixIconButton></div><p class="dialog-caption">{{ demo ? '仅为 Demo 操作生成的模拟事件。' : '当前 GUI 进程与 CLI 共用认证核心的日志，不读取其他 CLI 进程。' }} 不记录账号、密码、URL 或 Cookie。</p><div class="logs-body" role="log" aria-label="客户端日志" aria-live="off"><p v-if="!logEntries.length" class="logs-empty">暂无日志，开启后执行认证操作即可查看。</p><div v-for="entry in logEntries" :key="entry.sequence" class="log-line"><time>{{ logTime(entry.timestamp_ms) }}</time><span class="log-level" :class="entry.level">{{ entry.level.toUpperCase() }}</span><code>{{ entry.code }}</code><span>{{ entry.message }}</span></div></div><div class="logs-footer"><span>{{ logEntries.length }} / 300 条 · 关闭日志开关即清空</span><MiuixIconButton class="dialog-icon-button" aria-label="清空日志" title="清空日志" @click="clearLogs"><MiuixIcon :icon="Clear" :size="18" /></MiuixIconButton></div></dialog>
    <dialog ref="sessionDialog" class="confirm-dialog session-picker" aria-labelledby="picker-title" @cancel.prevent="sessionPickerOpen = false"><h2 id="picker-title">选择要下线的会话</h2><p>无法唯一确定本次连接，请选择目标；不会自动下线其他设备。</p><div class="picker-list"><button v-for="row in snapshot.sessions" :key="row.radacctid" class="picker-session" :disabled="locked" @click="selectForDisconnect(row.radacctid)"><strong>会话 {{ row.radacctid }}</strong><span>{{ row.framedipaddress || 'IP 未上报' }} · {{ mask(row.username) }}</span></button><p v-if="!snapshot.sessions.length">暂无可选会话，请打开认证后台确认远端状态。</p></div><div class="dialog-actions"><MiuixButton @click="sessionPickerOpen = false">取消</MiuixButton><MiuixButton v-if="!snapshot.sessions.length" @click="openSite">认证后台</MiuixButton></div></dialog>
    <dialog ref="licenseDialog" class="logs-dialog license-dialog" aria-labelledby="licenses-title" @cancel.prevent="licensesOpen = false"><div class="logs-heading"><h2 id="licenses-title">开源软件声明</h2><MiuixIconButton class="dialog-icon-button" aria-label="关闭开源软件声明" title="关闭开源软件声明" @click="licensesOpen = false"><MiuixIcon :icon="Close" :size="18" /></MiuixIconButton></div><p class="dialog-caption">{{ licenseNotice }}</p><div class="license-groups" role="list"><section v-for="group in licenseGroups" :key="group.title" class="license-group"><h3>{{ group.title }}</h3><button v-for="entry in group.entries" :key="entry.name" role="listitem" class="license-row" :disabled="locked" :title="`打开 ${entry.name} 仓库`" @click="openUrl(entry.url)"><span class="license-head"><strong>{{ entry.name }}</strong><span class="license-tag">{{ entry.license }}</span></span><span class="license-summary">{{ entry.summary }}</span></button></section></div></dialog>
    <MiuixSnackbarHost />
  </div>
  </MotionConfig>
</template>
