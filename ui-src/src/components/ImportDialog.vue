<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { api } from '../api';
import type { ImportInput, ImportPreview } from '../api';

const props = defineProps<{ filename: string; input: ImportInput; unsaved: boolean }>();
const emit = defineEmits<{ cancel: []; applied: [groups: number, values: number] }>();
const dialog = ref<HTMLDialogElement | null>(null);
const preview = ref<ImportPreview | null>(null);
const error = ref('');
const enableNew = ref(false);
const loading = ref(false);
const applying = ref(false);
let version = 0;

async function refresh(): Promise<void> {
  const request = ++version;
  preview.value = null;
  loading.value = true;
  error.value = '';
  try {
    const result = await api.previewImport({ ...props.input, enable_new: enableNew.value });
    if (request !== version) return;
    if (!result.ok) throw new Error(result.error || 'Preview failed');
    preview.value = result;
  } catch (failure) {
    if (request === version) error.value = failure instanceof Error ? failure.message : 'Preview failed';
  } finally {
    if (request === version) loading.value = false;
  }
}

async function apply(): Promise<void> {
  if (!preview.value || loading.value || applying.value) return;
  applying.value = true;
  error.value = '';
  try {
    const result = await api.applyImport({ ...props.input, enable_new: enableNew.value, expected: preview.value.token });
    if (!result.ok) throw new Error(result.error || 'Import failed');
    emit('applied', result.groups, result.values);
  } catch (failure) {
    error.value = failure instanceof Error ? failure.message : 'Import failed';
    preview.value = null;
  } finally {
    applying.value = false;
  }
}

watch(enableNew, refresh);
onMounted(() => { dialog.value?.showModal(); void refresh(); });
onBeforeUnmount(() => { version++; });
</script>

<template>
  <dialog ref="dialog" class="import-dialog" aria-labelledby="import-title" @cancel.prevent="!applying && emit('cancel')">
    <h2 id="import-title">Review import</h2>
    <p>{{ filename }} · {{ input.format }}</p>
    <p>Merge only: unrelated items are kept. Named groups resolve top to bottom; Default always resolves last.</p>
    <p v-if="unsaved" class="hint">Unsaved editor drafts will be kept, not saved or discarded by this import.</p>
    <label v-if="input.format === 'whistle-rules' || input.format === 'rules-text'">
      <input v-model="enableNew" type="checkbox" :disabled="applying" /> Enable newly imported groups
    </label>
    <p v-if="loading" role="status">Loading preview…</p>
    <p v-if="error" role="alert">{{ error }}</p>
    <template v-if="preview">
      <p v-for="warning in preview.warnings" :key="warning" class="hint">{{ warning }}</p>
      <h3>Rules</h3>
      <p v-if="!preview.groups.length" class="hint">No rule groups in this file.</p>
      <ul><li v-for="group in preview.groups" :key="group.name">
        {{ group.name }} — {{ group.replace ? 'Replace saved text' : 'New' }} · {{ group.enabled ? 'enabled' : 'disabled' }}
      </li></ul>
      <h3>Values</h3>
      <p v-if="!preview.values.length" class="hint">No values in this file.</p>
      <ul><li v-for="value in preview.values" :key="value.name">{{ value.name }} — {{ value.replace ? 'Replace saved value' : 'New' }}</li></ul>
      <div class="order-columns">
        <section><h3>Order before</h3><ol aria-label="Order before"><li v-for="name in preview.before" :key="name">{{ name }}</li></ol></section>
        <section><h3>Order after</h3><ol aria-label="Order after"><li v-for="name in preview.after" :key="name">{{ name }}</li></ol></section>
      </div>
    </template>
    <div class="actions">
      <a href="/api/export" download>Download backup</a>
      <span class="spacer"></span>
      <button class="btn" :disabled="loading || applying" @click="refresh()">Refresh preview</button>
      <button class="btn" :disabled="applying" @click="emit('cancel')">Cancel</button>
      <button class="btn primary" :disabled="!preview || loading || applying" @click="apply()">{{ applying ? 'Importing…' : 'Apply import' }}</button>
    </div>
  </dialog>
</template>

<style scoped>
.import-dialog { width: min(720px, 90vw); max-height: 85vh; overflow: auto; color: var(--fg); background: var(--bg); border: 1px solid var(--line); border-radius: 7px; padding: 20px; }
.import-dialog::backdrop { background: #0008; }
.import-dialog p, .import-dialog li { overflow-wrap: anywhere; }
.order-columns { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
.actions { position: sticky; bottom: -20px; background: var(--bg); padding: 12px 0; }
[role='alert'] { font-weight: 600; }
</style>
