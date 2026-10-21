<script setup lang="ts">
// A request written by hand: method, URL, headers, body.
//
// It is sent through the proxy rather than from the console, so the rules apply
// to it and it lands in the capture like any other request. That is the whole
// difference between this and a terminal: `curl` tells you what the origin
// answers, the Composer tells you what your *proxy* does with the request.

import CodeEditor from '../components/CodeEditor.vue';
import { ref } from 'vue';
import { compositionEncoding, composeLoading, composeSending, loadCompositionFile, newComposition, sendComposition, state } from '../store';

const picker = ref<HTMLInputElement | null>(null);
function encodingChanged(event: Event): void {
  const select = event.target as HTMLSelectElement;
  compositionEncoding(select.value as 'utf8' | 'base64');
  select.value = state.compose.body_encoding || 'utf8';
}
async function picked(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (file) await loadCompositionFile(file);
}

// The ones worth a click, in a datalist rather than a <select>: whistle's
// composer takes whatever method you give it (`getMethod` merely uppercases
// it), and a proxy is exactly where you go to try a verb nothing else sends.
const METHODS = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS'];
</script>

<template>
  <section class="pane">
    <div class="compose">
      <div class="compose-line">
        <input
          v-model="state.compose.method"
          :disabled="composeLoading"
          class="compose-method"
          list="composer-methods"
          spellcheck="false"
          aria-label="Method"
          placeholder="GET"
        />
        <datalist id="composer-methods">
          <option v-for="m in METHODS" :key="m" :value="m"></option>
        </datalist>
        <input
          v-model="state.compose.url"
          :disabled="composeLoading"
          class="compose-url"
          spellcheck="false"
          aria-label="URL"
          placeholder="https://example.com/api/items"
          @keydown.enter="sendComposition()"
        />
      </div>

      <div class="compose-fields">
        <div class="compose-field">
          <label for="composer-headers">Headers</label>
          <textarea
            id="composer-headers"
            v-model="state.compose.headers"
            :disabled="composeLoading"
            spellcheck="false"
            placeholder="Content-Type: application/json&#10;Authorization: Bearer …"
          ></textarea>
        </div>
        <div class="compose-field">
          <label>Body
            <select aria-label="Body encoding" :value="state.compose.body_encoding || 'utf8'" :disabled="composeLoading" @change="encodingChanged">
              <option value="utf8">UTF-8 text</option><option value="base64">Base64 · exact bytes</option>
            </select>
            <button class="btn tiny" :disabled="composeLoading" @click="picker?.click()">Load body file</button>
          </label>
          <input ref="picker" type="file" hidden @change="picked" />
          <!-- The editor is the JSON one because bodies you retype by hand
               almost always are; nothing lints, so anything else is merely
               unhighlighted rather than wrong. -->
          <CodeEditor
            v-model="state.compose.body"
            :read-only="composeLoading"
            language="json"
            placeholder='{ "name": "third" }'
            @save="sendComposition()"
          />
        </div>
      </div>

      <p class="hint">
        Keep Base64 for binary or raw multipart; retain its Content-Type boundary. Files are limited to 8 MiB.
        Binary bodies stay in this page only; reload them after refreshing.
        One <code>Name: value</code> per line. The request goes out through this proxy,
        so your rules apply to it and <code>from:composer</code> matches it.
        <code>⌘↩</code> sends.
      </p>
      <label v-if="state.compose.body_incomplete" class="hint">
        <input v-model="state.compose.allow_partial" type="checkbox" aria-label="Send retained bytes anyway" />
        Body is incomplete or unavailable. Send retained bytes anyway (not a faithful replay).
      </label>
    </div>

    <div class="actions">
      <button class="btn primary" :disabled="composeLoading || composeSending || (state.compose.body_incomplete && !state.compose.allow_partial)" @click="sendComposition()">Send</button>
      <button class="btn" @click="newComposition()">New</button>
      <span class="spacer"></span>
      <span class="status">{{ state.composeStatus }}</span>
    </div>
  </section>
</template>
