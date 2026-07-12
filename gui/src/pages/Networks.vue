<script setup lang="ts">
import { onMounted, onUnmounted, reactive, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useToast } from 'primevue/usetoast'
import type { UnlistenFn } from '@tauri-apps/api/event'
import {
  deleteProfile,
  detectConflicts,
  installationStatus,
  listProfiles,
  onEvent,
  runningIds,
  startNetwork,
  stopNetwork,
  type Conflicts,
  type ProfileMeta,
} from '../api'

const router = useRouter()
const toast = useToast()

const profiles = ref<ProfileMeta[]>([])
const running = ref<Set<string>>(new Set())
const busy = reactive<Record<string, boolean>>({})
const installed = ref(true)
const conflicts = ref<Conflicts | null>(null)

const deleteTarget = ref<ProfileMeta | null>(null)
const showDeleteDialog = ref(false)

let pollTimer: ReturnType<typeof setInterval> | undefined
let unlisten: UnlistenFn | undefined

async function refreshList() {
  try {
    const [list, ids] = await Promise.all([listProfiles(), runningIds()])
    profiles.value = list
    running.value = new Set(ids)
  } catch (e) {
    toast.add({ severity: 'error', summary: '加载网络列表失败', detail: String(e), life: 4000 })
  }
}

async function refreshInstallStatus() {
  try {
    const s = await installationStatus()
    installed.value = s.installed
  } catch {
    // Best-effort; leave last known state.
  }
}

async function refreshConflicts() {
  try {
    conflicts.value = await detectConflicts()
  } catch {
    conflicts.value = null
  }
}

async function toggleRunning(profile: ProfileMeta, next: boolean) {
  busy[profile.id] = true
  try {
    if (next) {
      await startNetwork(profile.id)
    } else {
      await stopNetwork(profile.id)
    }
    await refreshList()
  } catch (e) {
    toast.add({
      severity: 'error',
      summary: next ? '启动网络失败' : '停止网络失败',
      detail: String(e),
      life: 5000,
    })
  } finally {
    busy[profile.id] = false
  }
}

function confirmDelete(profile: ProfileMeta) {
  deleteTarget.value = profile
  showDeleteDialog.value = true
}

async function doDelete() {
  if (!deleteTarget.value) return
  try {
    await deleteProfile(deleteTarget.value.id)
    showDeleteDialog.value = false
    deleteTarget.value = null
    await refreshList()
  } catch (e) {
    toast.add({ severity: 'error', summary: '删除失败', detail: String(e), life: 5000 })
  }
}

onMounted(async () => {
  await Promise.all([refreshList(), refreshInstallStatus(), refreshConflicts()])
  pollTimer = setInterval(refreshList, 2000)
  unlisten = await onEvent('network://changed', () => {
    refreshList()
  })
})

onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer)
  if (unlisten) unlisten()
})
</script>

<template>
  <div class="page-header">
    <h1>网络</h1>
    <Button label="新建网络" icon="pi pi-plus" @click="router.push('/edit')" />
  </div>

  <Message v-if="!installed" severity="warn" class="mb-3" :closable="false">
    <div class="row">
      <span>尚未安装特权 supervisor，网络无法启动。</span>
      <Button label="前往安装" size="small" @click="router.push('/install')" />
    </div>
  </Message>

  <Message
    v-if="conflicts && (conflicts.unmanaged_core || conflicts.tun_vpn)"
    severity="warn"
    class="mb-3"
    :closable="false"
  >
    <div>
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
    </div>
  </Message>

  <div v-if="profiles.length === 0" class="empty-state">暂无网络，点击“新建网络”创建一个。</div>

  <div class="card-grid">
    <Card v-for="profile in profiles" :key="profile.id">
      <template #content>
        <div class="network-card">
          <div>
            <div class="network-card__name">{{ profile.name }}</div>
            <Tag :severity="running.has(profile.id) ? 'success' : 'secondary'">
              {{ running.has(profile.id) ? '运行中' : '已停止' }}
            </Tag>
          </div>
          <div class="network-card__actions">
            <ToggleSwitch
              :modelValue="running.has(profile.id)"
              :disabled="busy[profile.id]"
              @update:modelValue="(v: boolean) => toggleRunning(profile, v)"
            />
            <Button
              label="编辑"
              size="small"
              severity="secondary"
              @click="router.push(`/edit/${profile.id}`)"
            />
            <Button
              v-if="running.has(profile.id)"
              label="状态"
              size="small"
              severity="secondary"
              @click="router.push(`/status/${profile.id}`)"
            />
            <Button
              label="删除"
              size="small"
              severity="danger"
              outlined
              @click="confirmDelete(profile)"
            />
          </div>
        </div>
      </template>
    </Card>
  </div>

  <Dialog v-model:visible="showDeleteDialog" header="确认删除" modal :style="{ width: '360px' }">
    <p>确定要删除网络 “{{ deleteTarget?.name }}” 吗？此操作不可撤销。</p>
    <template #footer>
      <Button label="取消" severity="secondary" @click="showDeleteDialog = false" />
      <Button label="删除" severity="danger" @click="doDelete" />
    </template>
  </Dialog>
</template>
