<script setup lang="ts">
import { computed, nextTick, onUnmounted, ref, watch } from 'vue'
import { MotionConfig } from 'motion-v'
import { MiuixButton, MiuixCard, MiuixSwitchPreference, MiuixTabRow, MiuixSnackbarHost, showSnackbar, useTheme, type ThemeMode } from 'miuix-vue'
import { usePortal, formatBytes, formatRate, formatDuration, mask } from './usePortal'

const { demo, busy, ready, page, draft, saved, snapshot, username, password, portalUrl, selected, rate, isOnline, title, phase, notice, confirmation, answer, connect, refresh, select, disconnect, forget, save, openSite, simulateUpdate } = usePortal()
const { mode, setThemeMode } = useTheme()
const themeMode = computed({ get: () => mode.value, set: (value: ThemeMode) => setThemeMode(value) })
const styleNonce = document.querySelector<HTMLStyleElement>('#motion-csp')?.nonce || undefined
const locked = computed(() => busy.value || !ready.value)
const settingsLocked = computed(() => locked.value || (snapshot.value.authenticated && !demo.value))
const dialog = ref<HTMLDialogElement>()
let previousFocus: HTMLElement | null = null
watch(notice, message => { if (message) { void showSnackbar({ message, withDismissAction: true, duration: 6000 }); notice.value = '' } })
watch(confirmation, async value => {
  if (value) { previousFocus = document.activeElement as HTMLElement; await nextTick(); dialog.value?.showModal(); dialog.value?.querySelector<HTMLButtonElement>('button')?.focus() }
  else { dialog.value?.close(); previousFocus?.focus() }
})
onUnmounted(() => dialog.value?.close())
</script>

<template>
  <MotionConfig :nonce="styleNonce" reduced-motion="user">
  <div class="portal-app">
    <header class="app-header">
      <div class="brand"><span class="brand-mark" aria-hidden="true">D</span><div><h1>DGCU Portal</h1><span class="subtitle">校园网连接</span></div><span v-if="demo" class="mode-badge">演示</span></div>
      <div class="header-actions"><label class="theme-picker"><span class="sr-only">外观</span><select v-model="themeMode" aria-label="外观"><option value="system">跟随系统</option><option value="light">浅色</option><option value="dark">深色</option></select></label><MiuixButton :disabled="locked" @click="openSite">认证网站 ↗</MiuixButton></div>
    </header>
    <nav class="top-navigation" aria-label="主导航"><MiuixTabRow v-model="page" :tabs="['网络', '设置', '关于']" contour /></nav>

    <main>
      <section v-if="page === 0" class="overview" aria-label="网络概览">
        <MiuixCard class="connection-card">
          <div class="connection-main"><div class="status-icon" :class="{ online: isOnline }" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="M3 8.5a15 15 0 0 1 18 0M6 12a10 10 0 0 1 12 0m-9 3.5a5 5 0 0 1 6 0"/><circle cx="12" cy="19" r="1"/></svg></div><div class="connection-copy"><div class="status-line"><h2>{{ phase || title }}</h2><span v-if="saved.one_session" class="privacy-tag">仅一次会话</span></div><p class="status-message" role="status">{{ phase ? '请稍候，等待认证系统返回结果' : snapshot.message }}</p></div></div>
          <div class="connection-actions"><MiuixButton type="primary" :disabled="locked" @click="connect(false)">{{ busy ? '处理中…' : '上线' }}</MiuixButton><MiuixButton class="disconnect-button" :disabled="locked || !selected" @click="disconnect">下线</MiuixButton></div>
        </MiuixCard>
        <div class="traffic-grid" aria-label="后台计费流量">
          <MiuixCard class="metric"><span class="metric-label">↓ 累计下载</span><strong>{{ selected ? formatBytes(selected.acctoutputoctets) : '—' }}</strong><span class="metric-note">所选后台会话</span></MiuixCard>
          <MiuixCard class="metric"><span class="metric-label">↑ 累计上传</span><strong>{{ selected ? formatBytes(selected.acctinputoctets) : '—' }}</strong><span class="metric-note">所选后台会话</span></MiuixCard>
          <MiuixCard class="metric"><span class="metric-label">↓ 区间下载速率</span><strong :class="{ waiting: rate?.download_bps == null }">{{ formatRate(rate?.download_bps) }}</strong><span class="metric-note">{{ rate?.sample_seconds ? `最近 ${rate.sample_seconds} 秒平均` : '等待后台计费更新' }}</span></MiuixCard>
          <MiuixCard class="metric"><span class="metric-label">↑ 区间上传速率</span><strong :class="{ waiting: rate?.upload_bps == null }">{{ formatRate(rate?.upload_bps) }}</strong><span class="metric-note">按后台计费时间计算</span></MiuixCard>
        </div>
        <div class="section-heading"><h3>在线会话 <span class="count">{{ snapshot.sessions.length }}</span></h3><div class="row-actions"><MiuixButton :disabled="locked" @click="connect(true)">仅登录后台</MiuixButton><MiuixButton :disabled="locked || !snapshot.authenticated" @click="refresh">刷新</MiuixButton></div></div>
        <MiuixCard class="session-list">
          <div v-if="!snapshot.sessions.length" class="empty-state"><strong>暂无在线会话</strong><span>上线或登录后台后，即可查看和管理连接。</span><MiuixButton :disabled="locked" @click="page = 1">填写登录信息</MiuixButton></div>
          <label v-for="row in snapshot.sessions" v-else :key="row.radacctid" class="session-row" :class="{ selected: row.radacctid === snapshot.selected_id }"><input type="radio" name="session" :checked="row.radacctid === snapshot.selected_id" :disabled="locked" :aria-label="`选择会话 ${row.radacctid}`" @change="select(row.radacctid)"><div class="session-identity"><strong>{{ mask(row.username) }} <span v-if="row.radacctid === snapshot.selected_id" class="selected-tag">已选择</span></strong><span>{{ row.framedipaddress || 'IP 未上报' }} · #{{ row.radacctid }}</span></div><span class="session-duration">{{ formatDuration(row.acctsessiontime) }}</span></label>
        </MiuixCard>
        <div class="overview-footer"><span>{{ demo ? '模拟数据 · 不连接校园网' : '后台计费数据 · 不采集网卡' }}</span><div class="row-actions"><MiuixButton v-if="demo" :disabled="locked || !snapshot.sessions.length" @click="simulateUpdate">模拟流量更新</MiuixButton><MiuixButton :disabled="locked" @click="forget">清除本地会话</MiuixButton></div></div>
      </section>

      <section v-else-if="page === 1" class="settings-page" aria-label="偏好设置">
        <p v-if="settingsLocked && !busy" class="inline-note">连接与账号设置需先结束本地会话后才能修改。</p>
        <h3 class="group-heading">登录项</h3>
        <MiuixCard>
          <form class="credential-fields" autocomplete="off" @submit.prevent="connect(false)"><label class="field">账号<input v-model="username" :disabled="locked" autocomplete="off" placeholder="校园网账号" spellcheck="false"></label><label class="field">密码<input v-model="password" :disabled="locked" type="password" autocomplete="new-password" placeholder="仅当前会话使用"><button type="submit" class="sr-only" :disabled="locked" tabindex="-1">上线</button></label></form>
          <MiuixSwitchPreference v-model="draft.one_session" title="仅一次会话" summary="结束后释放凭据与 Cookie，适合公共电脑" :disabled="settingsLocked" />
          <MiuixSwitchPreference v-model="draft.remember_account" title="记住账号密码" summary="密码保存在系统凭据库" :disabled="settingsLocked || draft.one_session" />
        </MiuixCard>
        <h3 class="group-heading">系统</h3>
        <MiuixCard>
          <MiuixSwitchPreference v-model="draft.bypass_proxy" title="绕过程序代理" summary="直连认证服务器；TUN / VPN 路由仍由系统决定" :disabled="settingsLocked" />
          <MiuixSwitchPreference v-model="draft.auto_redial" title="掉线重拨" summary="会话连续消失后重拨，手动下线后暂停" :disabled="settingsLocked || draft.one_session" />
          <MiuixSwitchPreference v-model="draft.tray_startup" title="托盘启动" summary="下次启动隐藏窗口，可从托盘打开" :disabled="settingsLocked" />
          <MiuixSwitchPreference v-model="draft.service_enabled" title="写入用户后台服务" summary="当前用户登录系统时启动客户端" :disabled="settingsLocked || draft.one_session" />
        </MiuixCard>
        <details class="advanced-settings"><summary>高级连接设置</summary><MiuixCard class="advanced-fields"><label class="field">认证服务器<input v-model="draft.server" :disabled="settingsLocked" spellcheck="false"></label><label class="field">认证网站<input v-model="draft.auth_url" :disabled="settingsLocked" spellcheck="false"></label><label class="field">Portal URL（可选）<input v-model="portalUrl" :disabled="locked" autocomplete="off" spellcheck="false" placeholder="留空自动探测，或粘贴当前网络的认证网址"></label><label class="field">HTTP 探测地址<input v-model="draft.probe_url" :disabled="settingsLocked" spellcheck="false"></label></MiuixCard></details>
        <div class="settings-footer"><span>临时模式不保存账号密码，不启用后台自动认证。</span><div class="row-actions"><MiuixButton :disabled="settingsLocked" @click="save">保存设置</MiuixButton><MiuixButton type="primary" :disabled="locked" @click="connect(false)">上线</MiuixButton></div></div>
      </section>

      <section v-else class="about-page" aria-label="连接说明"><MiuixCard class="about-card"><div class="about-brand"><span class="brand-mark">D</span><div><h2>DGCU Portal</h2><p>Tauri 2 · Vue 3 · miuix-vue</p></div></div><dl><div><dt>认证方式</dt><dd>CMCC Portal 1.0 / PAP</dd></div><div><dt>代拨等待</dt><dd>每 3 秒查询，20 秒截止</dd></div><div><dt>流量来源</dt><dd>校园网后台 onlinelog</dd></div><div><dt>速率说明</dt><dd>计费区间平均值，与套餐限速不同</dd></div><div><dt>本机网卡</dt><dd>不读取、不采集</dd></div></dl><p class="about-note">认证成功表示系统已返回成功；后台会话在线不保证所有外部网站都可访问。强制关机时无法保证远端已下线。</p></MiuixCard></section>
    </main>
    <dialog ref="dialog" class="confirm-dialog" aria-labelledby="dialog-title" @cancel.prevent="answer(false)"><template v-if="confirmation"><h2 id="dialog-title">{{ confirmation.title }}</h2><p>{{ confirmation.text }}</p><div class="dialog-actions"><MiuixButton @click="answer(false)">取消</MiuixButton><MiuixButton type="primary" @click="answer(true)">{{ confirmation.label }}</MiuixButton></div></template></dialog>
    <MiuixSnackbarHost />
  </div>
  </MotionConfig>
</template>
