<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { useToast } from 'primevue/usetoast'
import type { UnlistenFn } from '@tauri-apps/api/event'
import { onEvent, supervisorStatus, takeoverSupervisor, type SupervisorStatus } from './api'

const toast = useToast()

const status = ref<SupervisorStatus>({
  connected: false,
  core_running: false,
  rpc_port: null,
  installed: false,
})

// Shown when the supervisor reports another owner already holds the lease.
const busyVisible = ref(false)

async function confirmTakeover() {
  busyVisible.value = false
  try {
    await takeoverSupervisor()
    toast.add({ severity: 'info', summary: '正在接管控制连接…', life: 3000 })
  } catch (e: any) {
    toast.add({ severity: 'error', summary: '接管失败', detail: String(e), life: 5000 })
  }
}

let pollTimer: ReturnType<typeof setInterval> | undefined
const unlisteners: UnlistenFn[] = []

async function refreshStatus() {
  try {
    status.value = await supervisorStatus()
  } catch {
    // Supervisor status is best-effort for the header; ignore transient errors.
  }
}

onMounted(async () => {
  await refreshStatus()
  pollTimer = setInterval(refreshStatus, 3000)

  unlisteners.push(
    await onEvent<{ version: string; core: string; rpc_port: number | null }>(
      'supervisor://connected',
      () => {
        toast.add({ severity: 'info', summary: '已连接 supervisor', life: 3000 })
        refreshStatus()
      },
    ),
  )
  unlisteners.push(
    await onEvent('supervisor://disconnected', () => {
      toast.add({ severity: 'warn', summary: 'supervisor 连接断开', life: 4000 })
      refreshStatus()
    }),
  )
  unlisteners.push(
    await onEvent<{ pid: number; rpc_port: number }>('core://started', () => {
      refreshStatus()
    }),
  )
  unlisteners.push(
    await onEvent<{ reason: string }>('core://stopped', () => {
      refreshStatus()
    }),
  )
  unlisteners.push(
    await onEvent<{ code: number | null; signal: number | null }>('core://exited', () => {
      toast.add({ severity: 'error', summary: '核心进程退出', life: 5000 })
      refreshStatus()
    }),
  )
  unlisteners.push(
    await onEvent('supervisor://kicked', () => {
      toast.add({ severity: 'warn', summary: '控制权已被其他实例接管', life: 5000 })
    }),
  )
  unlisteners.push(
    await onEvent<{ owner: boolean }>('supervisor://busy', () => {
      // Ask the user before taking over (DESIGN §8); the driver has paused
      // auto-reconnect until we send a takeover.
      busyVisible.value = true
    }),
  )
  unlisteners.push(
    await onEvent<{ code: string; msg: string }>('supervisor://error', (payload) => {
      toast.add({
        severity: 'error',
        summary: 'supervisor 错误',
        detail: payload?.msg || payload?.code,
        life: 5000,
      })
    }),
  )
  unlisteners.push(
    await onEvent<{ attempt: number; count: number }>('network://restarted', () => {
      toast.add({ severity: 'info', summary: '已自动重启网络', life: 4000 })
    }),
  )
  unlisteners.push(
    await onEvent<{ error: string; attempt: number }>('network://restart_failed', (payload) => {
      toast.add({
        severity: 'error',
        summary: `自动重启失败(第 ${payload?.attempt ?? ''} 次),将继续重试`,
        detail: payload?.error,
        life: 5000,
      })
    }),
  )
  unlisteners.push(
    await onEvent<{ attempts: number }>('network://restart_gaveup', (payload) => {
      toast.add({
        severity: 'error',
        summary: '自动重启已停止',
        detail: `连续 ${payload?.attempts ?? ''} 次重启失败,已停止尝试`,
        life: 6000,
      })
    }),
  )
  unlisteners.push(
    await onEvent<{ reason: string }>('network://restart_skipped', () => {
      toast.add({ severity: 'info', summary: '核心已退出,自动重启已关闭', life: 4000 })
    }),
  )
})

onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer)
  for (const unlisten of unlisteners) unlisten()
})
</script>

<template>
  <Toast />
  <Dialog v-model:visible="busyVisible" modal header="检测到已有会话" :style="{ width: '26rem' }">
    <p>检测到另一个 EasyTier 会话正持有 supervisor 控制连接。是否接管?接管会断开对方的控制连接。</p>
    <template #footer>
      <Button label="取消" severity="secondary" text @click="busyVisible = false" />
      <Button label="接管" severity="danger" @click="confirmTakeover" />
    </template>
  </Dialog>
  <header class="app-header">
    <div class="app-header__title">EasyTier</div>
    <div class="app-header__status">
      <Tag :severity="status.connected ? 'success' : 'danger'">
        {{ status.connected ? 'supervisor 已连接' : 'supervisor 未连接' }}
      </Tag>
      <Tag :severity="status.core_running ? 'success' : 'secondary'">
        {{ status.core_running ? '核心运行中' : '核心未运行' }}
      </Tag>
    </div>
  </header>
  <div class="app-body">
    <nav class="app-nav">
      <router-link to="/">网络</router-link>
      <router-link to="/settings">设置</router-link>
    </nav>
    <main class="app-content">
      <router-view />
    </main>
  </div>
</template>
