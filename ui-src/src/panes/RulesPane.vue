<script setup lang="ts">
// The rules of the selected group, with the highlighting that says which token
// the proxy will match on.

import { onBeforeUnmount, onMounted } from 'vue';
import CodeEditor from '../components/CodeEditor.vue';
import { deleteGroup, moveGroup, namedGroupIndex, namedGroups, renameGroup, ruleDirty, ruleManagementBusy, rulesEditor, saveRules, state } from '../store';

// Delete the selected group with the keyboard, since the source list has no
// room for a per-row button without crowding the name.
function onKeydown(e: KeyboardEvent): void {
  if (document.querySelector('dialog[open]')) return;
  if (state.pane !== 'rules' || state.group === 'default') return;
  if (e.key !== 'Backspace' && e.key !== 'Delete') return;
  const active = document.activeElement;
  if (active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA')) return;
  if (active?.closest('.cm-editor')) return;
  e.preventDefault();
  void deleteGroup(state.group);
}

onMounted(() => document.addEventListener('keydown', onKeydown));
onBeforeUnmount(() => document.removeEventListener('keydown', onKeydown));
</script>

<template>
  <section class="pane">
    <div class="editor-wrap">
      <CodeEditor
        :key="state.group"
        v-model="rulesEditor.text"
        :read-only="rulesEditor.saved === undefined"
        language="whistle"
        placeholder="pattern  operator1  operator2  …"
        @save="saveRules()"
      />
      <p class="hint">
        One rule per line. <code>example.com http://localhost:5173</code> forwards a
        site; <code>host://1.2.3.4</code> only moves the socket. Named groups resolve top to bottom; Default always resolves last.
      </p>
    </div>
    <div class="actions">
      <button
        class="btn primary"
        :disabled="rulesEditor.saved === undefined || rulesEditor.saving || ruleManagementBusy"
        @click="saveRules()"
      >Save</button>
      <template v-if="state.group !== 'default'">
        <button class="btn" :disabled="ruleManagementBusy || rulesEditor.saving" @click="renameGroup(state.group)">Rename</button>
        <button class="btn" :disabled="ruleManagementBusy || rulesEditor.saving || namedGroupIndex <= 0" @click="moveGroup(-1)">Move up</button>
        <button class="btn" :disabled="ruleManagementBusy || rulesEditor.saving || namedGroupIndex >= namedGroups.length - 1" @click="moveGroup(1)">Move down</button>
      </template>
      <span v-if="ruleDirty(state.group)" class="status">Unsaved changes</span>
      <span class="spacer"></span>
      <span class="status">{{ rulesEditor.saving ? 'Saving…' : rulesEditor.loading ? 'Loading…' : rulesEditor.status }}</span>
    </div>
  </section>
</template>
