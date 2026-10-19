<script setup lang="ts">
// The named values: one key at a time, or the whole store as the JSON object it
// is stored as.
//
// Whole-object editing was the only way there was, which made every save a
// rewrite of every key — one mistyped brace and the lot goes. The per-key
// editor writes only the key it is showing.

import CodeEditor from '../components/CodeEditor.vue';
import { deleteValue, renameValue, saveValue, state, valueDirty, valuesEditor, valuesSaving } from '../store';
</script>

<template>
  <section class="pane">
    <div class="editor-wrap">
      <!-- Keyed on the selection so the editor is rebuilt, and its undo history
           with it: stepping back through a *previous* value's edits into this
           one would write the wrong content under this name. -->
      <CodeEditor
        :key="JSON.stringify(state.valueKey)"
        v-model="valuesEditor.text"
        :read-only="valuesEditor.saved === undefined"
        language="json"
        :placeholder="state.valueKey === null ? 'A JSON object of named values' : 'the value, referenced as {name}'"
        @save="saveValue()"
      />
      <p v-if="state.valueKey !== null" class="hint">
        Editing <code>{{ state.valueKey }}</code>. Reference it from any operator as
        <code>{{ '{' + state.valueKey + '}' }}</code>, or pull it in as a whole rules
        text with <code>rule://{{ state.valueKey }}</code>.
      </p>
      <p v-else class="hint">
        A JSON object of named values. Reference one from any operator as
        <code>{name}</code>, or pull a whole rules text in with <code>rule://name</code>.
      </p>
    </div>
    <div class="actions">
      <button
        class="btn primary"
        :disabled="valuesEditor.saved === undefined || valuesSaving"
        @click="saveValue()"
      >Save</button>
      <span v-if="valueDirty(state.valueKey)" class="status">Unsaved changes</span>
      <template v-if="state.valueKey !== null">
        <button class="btn" :disabled="valuesSaving" @click="renameValue(state.valueKey)">Rename</button>
        <button class="btn" :disabled="valuesSaving" @click="deleteValue(state.valueKey)">Delete</button>
      </template>
      <span class="spacer"></span>
      <span class="status">{{ valuesEditor.saving ? 'Saving…' : valuesEditor.status }}</span>
    </div>
  </section>
</template>
