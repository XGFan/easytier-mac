<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { networkStatus, type PeerRow } from '../api'
import { formatNumber, formatPercent, humanizeBytes } from '../format'

const route = useRoute()
const router = useRouter()

const id = route.params.id as string
const rows = ref<PeerRow[]>([])
const error = ref<string | null>(null)

let pollTimer: ReturnType<typeof setInterval> | undefined

async function refresh() {
  try {
    const status = await networkStatus(id)
    rows.value = status.rows
    error.value = null
  } catch (e) {
    error.value = String(e)
  }
}

onMounted(() => {
  refresh()
  pollTimer = setInterval(refresh, 2000)
})

onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer)
})
</script>

<template>
  <div class="page-header">
    <h1>网络状态</h1>
    <Button label="返回" severity="secondary" @click="router.push('/')" />
  </div>

  <Message v-if="error" severity="warn" :closable="false" class="mb-3">
    获取状态失败：{{ error }}（网络可能已停止，继续尝试中…）
  </Message>

  <DataTable :value="rows" dataKey="peer_id" size="small">
    <Column field="hostname" header="主机名">
      <template #body="{ data }">
        {{ data.hostname }}
        <Tag v-if="data.is_local" severity="info" value="本机" class="ml-2" />
      </template>
    </Column>
    <Column field="ipv4" header="IPv4" />
    <Column field="cost" header="开销" />
    <Column header="延迟ms">
      <template #body="{ data }">{{ formatNumber(data.latency_ms, 2) }}</template>
    </Column>
    <Column header="丢包">
      <template #body="{ data }">{{ formatPercent(data.loss_rate, 1) }}</template>
    </Column>
    <Column header="接收">
      <template #body="{ data }">{{ humanizeBytes(data.rx_bytes) }}</template>
    </Column>
    <Column header="发送">
      <template #body="{ data }">{{ humanizeBytes(data.tx_bytes) }}</template>
    </Column>
    <Column field="nat_type" header="NAT" />
    <Column field="version" header="版本" />
    <template #empty>
      <div class="empty-state">暂无节点信息</div>
    </template>
  </DataTable>
</template>
