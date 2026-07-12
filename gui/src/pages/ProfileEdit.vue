<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useToast } from 'primevue/usetoast'
import { getProfile, saveProfile, validateToml } from '../api'

const DEFAULT_TEMPLATE = `instance_name = "my-network"
dhcp = true

[network_identity]
network_name = "my-network"
network_secret = "mysecret"

[[peer]]
uri = "tcp://public.easytier.cn:11010"

[flags]
no_tun = true
`

const route = useRoute()
const router = useRouter()
const toast = useToast()

const routeId = route.params.id as string | undefined
const toml = ref('')
const validationError = ref<string | null>(null)
const validating = ref(false)
const saving = ref(false)
const loading = ref(false)

onMounted(async () => {
  if (routeId) {
    loading.value = true
    try {
      const rec = await getProfile(routeId)
      toml.value = rec.toml
    } catch (e) {
      toast.add({ severity: 'error', summary: '加载配置失败', detail: String(e), life: 5000 })
    } finally {
      loading.value = false
    }
  } else {
    toml.value = DEFAULT_TEMPLATE
  }
})

async function doValidate(): Promise<boolean> {
  validating.value = true
  validationError.value = null
  try {
    await validateToml(toml.value)
    toast.add({ severity: 'success', summary: '配置校验通过', life: 3000 })
    return true
  } catch (e) {
    validationError.value = String(e)
    toast.add({ severity: 'error', summary: '配置校验失败', detail: String(e), life: 5000 })
    return false
  } finally {
    validating.value = false
  }
}

async function doSave() {
  saving.value = true
  try {
    await saveProfile(routeId ?? null, toml.value)
    router.push('/')
  } catch (e) {
    validationError.value = String(e)
    toast.add({ severity: 'error', summary: '保存失败', detail: String(e), life: 5000 })
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <div class="page-header">
    <h1>{{ routeId ? '编辑网络' : '新建网络' }}</h1>
  </div>

  <div v-if="loading" class="empty-state">加载中…</div>

  <div v-else class="stack">
    <Textarea v-model="toml" rows="20" class="mono-textarea" style="width: 100%" />

    <Message v-if="validationError" severity="error" :closable="false">
      {{ validationError }}
    </Message>

    <div class="form-actions">
      <Button label="校验" severity="secondary" :loading="validating" @click="doValidate" />
      <Button label="保存" :loading="saving" @click="doSave" />
      <Button label="取消" severity="secondary" outlined @click="router.push('/')" />
    </div>
  </div>
</template>
