<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useToast } from 'primevue/usetoast'
import {
  detectConflicts,
  getSettings,
  quitApp,
  setAutoRestart,
  setAutostart,
  supervisorStatus,
  uninstallPrivileged,
  type Conflicts,
  type Settings,
  type SupervisorStatus,
} from '../api'

const router = useRouter()
const toast = useToast()

const settings = ref<Settings>({ autostart: false, auto_restart: true })
const status = ref<SupervisorStatus>({
  connected: false,
  core_running: false,
  rpc_port: null,
  installed: false,
})
const conflicts = ref<Conflicts | null>(null)
const showUninstallDialog = ref(false)
const uninstalling = ref(false)
const loading = ref(true)

async function refresh() {
  try {
    const [s, st, c] = await Promise.all([getSettings(), supervisorStatus(), detectConflicts()])
    settings.value = s
    status.value = st
    conflicts.value = c
  } catch (e) {
    toast.add({ severity: 'error', summary: '加载设置失败', detail: String(e), life: 4000 })
  } finally {
    loading.value = false
  }
}

onMounted(refresh)

async function onAutostartChange(value: boolean) {
  try {
    await setAutostart(value)
    settings.value.autostart = value
  } catch (e) {
    toast.add({ severity: 'error', summary: '设置开机自启动失败', detail: String(e), life: 4000 })
    await refresh()
  }
}

async function onAutoRestartChange(value: boolean) {
  try {
    await setAutoRestart(value)
    settings.value.auto_restart = value
  } catch (e) {
    toast.add({ severity: 'error', summary: '设置自动重启失败', detail: String(e), life: 4000 })
    await refresh()
  }
}

async function doUninstall() {
  uninstalling.value = true
  try {
    await uninstallPrivileged()
    showUninstallDialog.value = false
    toast.add({ severity: 'success', summary: '已卸载', life: 3000 })
    await refresh()
  } catch (e) {
    toast.add({ severity: 'error', summary: '卸载失败', detail: String(e), life: 5000 })
  } finally {
    uninstalling.value = false
  }
}

async function doQuit() {
  try {
    await quitApp()
  } catch (e) {
    toast.add({ severity: 'error', summary: '退出失败', detail: String(e), life: 4000 })
  }
}
</script>

<template>
  <div class="page-header">
    <h1>设置</h1>
  </div>

  <div v-if="loading" class="empty-state">加载中…</div>

  <div v-else class="stack">
    <Card>
      <template #title>常规</template>
      <template #content>
        <div class="stack">
          <div class="row">
            <span>开机自启动</span>
            <ToggleSwitch
              :modelValue="settings.autostart"
              @update:modelValue="onAutostartChange"
            />
          </div>
          <div class="row">
            <span>核心异常自动重启</span>
            <ToggleSwitch
              :modelValue="settings.auto_restart"
              @update:modelValue="onAutoRestartChange"
            />
          </div>
        </div>
      </template>
    </Card>

    <Card>
      <template #title>Supervisor 状态</template>
      <template #content>
        <div class="stack">
          <div class="row">
            <span>连接状态</span>
            <Tag :severity="status.connected ? 'success' : 'danger'">
              {{ status.connected ? '已连接' : '未连接' }}
            </Tag>
          </div>
          <div class="row">
            <span>核心运行状态</span>
            <Tag :severity="status.core_running ? 'success' : 'secondary'">
              {{ status.core_running ? '运行中' : '未运行' }}
            </Tag>
          </div>
          <div class="row">
            <span>RPC 端口</span>
            <span>{{ status.rpc_port ?? '—' }}</span>
          </div>
          <div class="row">
            <span>安装状态</span>
            <Tag :severity="status.installed ? 'success' : 'warn'">
              {{ status.installed ? '已安装' : '未安装' }}
            </Tag>
          </div>
          <div class="form-actions">
            <Button
              v-if="status.installed"
              label="卸载"
              severity="danger"
              outlined
              @click="showUninstallDialog = true"
            />
            <Button v-else label="前往安装" @click="router.push('/install')" />
          </div>
        </div>
      </template>
    </Card>

    <Card v-if="conflicts && (conflicts.unmanaged_core || conflicts.tun_vpn)">
      <template #title>冲突检测</template>
      <template #content>
        <Message severity="warn" :closable="false">
          <div v-if="conflicts.unmanaged_core">
            检测到未受管理的 easytier-core 进程：
            <ul>
              <li v-for="cmd in conflicts.unmanaged_core_cmds" :key="cmd">{{ cmd }}</li>
            </ul>
          </div>
          <div v-if="conflicts.tun_vpn">
            检测到可能冲突的 TUN VPN 进程：
            <ul>
              <li v-for="cmd in conflicts.tun_vpn_cmds" :key="cmd">{{ cmd }}</li>
            </ul>
          </div>
        </Message>
      </template>
    </Card>

    <Card>
      <template #title>应用</template>
      <template #content>
        <Button label="退出应用" severity="danger" @click="doQuit" />
      </template>
    </Card>
  </div>

  <Dialog v-model:visible="showUninstallDialog" header="确认卸载" modal :style="{ width: '360px' }">
    <p>确定要卸载特权 supervisor 吗？卸载后所有网络将无法启动，直到重新安装。</p>
    <template #footer>
      <Button label="取消" severity="secondary" @click="showUninstallDialog = false" />
      <Button label="卸载" severity="danger" :loading="uninstalling" @click="doUninstall" />
    </template>
  </Dialog>
</template>
