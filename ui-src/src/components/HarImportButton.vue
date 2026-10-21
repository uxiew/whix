<script setup lang="ts">
import { ref } from 'vue';
import { api } from '../api';
import { loadSessions } from '../store';

const picker = ref<HTMLInputElement | null>(null);
const busy = ref(false);
const status = ref('');
async function picked(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (!file || busy.value) return;
  busy.value = true;
  try {
    if (file.size > 16 * 1024 * 1024) throw new Error('HAR file exceeds 16 MiB');
    const har = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(await file.arrayBuffer()).replace(/^\uFEFF/, ''));
    if (!Array.isArray(har?.log?.entries)) throw new Error('Expected HAR log.entries');
    if (!confirm(`Import ${har.log.entries.length} offline sessions? No requests will be sent. Existing history/preview limits apply; the oldest in-memory rows may be evicted. Export first if you need a backup.`)) return;
    const result = await api.importHar(har);
    if (!result.ok) throw new Error(result.error || 'Import failed');
    status.value = `Imported ${result.imported}; ${result.retained} retained, ${result.evicted} evicted. ${result.warnings.join('; ')}`;
    await loadSessions();
  } catch (error) { status.value = error instanceof Error ? error.message : 'HAR import failed'; }
  finally { busy.value = false; }
}
</script>

<template>
  <button class="btn" :disabled="busy" @click="picker?.click()">{{ busy ? 'Importing…' : 'Import HAR' }}</button>
  <input ref="picker" type="file" accept=".har,.json,application/json" aria-label="HAR file" hidden @change="picked" />
  <span v-if="status" class="status har-result" role="status" :title="status">{{ status }}</span>
</template>

<style scoped>
.har-result { max-width: 25rem; overflow-wrap: anywhere; max-height: 5rem; overflow: auto; }
</style>
