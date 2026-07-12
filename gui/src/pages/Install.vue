<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useToast } from 'primevue/usetoast'
import { installPrivileged, installationStatus, type InstallationStatus } from '../api'

const router = useRouter()
const toast = useToast()

const status = ref<InstallationStatus>({
  plist_exists: false,
  supervisor_bin_exists: false,
  core_bin_exists: false,
  installed: false,
})
const installing = ref(false)
const loading = ref(true)

async function refresh() {
  try {
    status.value = await installationStatus()
  } catch (e) {
    toast.add({ severity: 'error', summary: '获取安装状态失败', detail: String(e), life: 4000 })
  } finally {
    loading.value = false
  }
}

onMounted(refresh)

async function doInstall() {
  installing.value = true
  try {
    await installPrivileged()
    toast.add({ severity: 'success', summary: '安装成功', life: 3000 })
    router.push('/')
  } catch (e) {
    toast.add({ severity: 'error', summary: '安装失败', detail: String(e), life: 6000 })
  } finally {
    installing.value = false
    await refresh()
  }
}
</script>

<template>
  <div class="page-header">
    <h1>安装引导</h1>
  </div>

  <div class="stack">
    <Message severity="info" :closable="false">
      EasyTier 需要一次性安装受特权控制的 launchd supervisor 服务，用于在后台管理核心进程。
      安装过程会触发一次 macOS 管理员权限确认。
    </Message>

    <Card>
      <template #title>安装状态</template>
      <template #content>
        <div v-if="loading" class="empty-state">加载中…</div>
        <div v-else class="stack">
          <div class="row">
            <span>launchd 配置文件</span>
            <Tag :severity="status.plist_exists ? 'success' : 'secondary'">
              {{ status.plist_exists ? '已存在' : '未安装' }}
            </Tag>
          </div>
          <div class="row">
            <span>Supervisor 可执行文件</span>
            <Tag :severity="status.supervisor_bin_exists ? 'success' : 'secondary'">
              {{ status.supervisor_bin_exists ? '已存在' : '未安装' }}
            </Tag>
          </div>
          <div class="row">
            <span>核心可执行文件</span>
            <Tag :severity="status.core_bin_exists ? 'success' : 'secondary'">
              {{ status.core_bin_exists ? '已存在' : '未安装' }}
            </Tag>
          </div>
          <div class="row">
            <span>总体状态</span>
            <Tag :severity="status.installed ? 'success' : 'warn'">
              {{ status.installed ? '已安装' : '未安装' }}
            </Tag>
          </div>
        </div>
      </template>
    </Card>

    <div class="form-actions">
      <Button label="安装" :loading="installing" @click="doInstall" />
      <Button label="返回" severity="secondary" outlined @click="router.push('/')" />
    </div>
  </div>
</template>
