<script setup lang="ts">
// A WebSocket conversation, oldest first. A frame `enable://ignoreSend` threw
// away is shown dimmed rather than omitted — a dropped frame is not a gap.
//
// A frame `enable://pauseSend` is *holding* is shown too, marked, above the
// control that lets it go: this is the half of a pause that makes it a pause
// rather than a stall, and without it there would be no reason to have the flag.

import { computed, onUnmounted, ref } from 'vue';
import type { WsFrame } from '../api';
import { poll } from '../poll';
import { bodyFile } from '../bytes';
import { disconnectWs, loadFrames, releaseWsDir, sendWsFrame, setWsMode, state, wsBusy, wsStatus } from '../store';

const binary = ref(false);
const picker = ref<HTMLInputElement | null>(null);
async function picked(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  const id = state.selected;
  if (!file) return;
  try {
    const data = await bodyFile(file);
    if (state.selected !== id) return;
    state.wsCompose = data; binary.value = true;
  } catch (error) { wsStatus.value = error instanceof Error ? error.message : 'Could not read frame file'; }
}
async function modeChanged(event: Event, dir: 'send' | 'receive'): Promise<void> {
  const select = event.target as HTMLSelectElement;
  await setWsMode(dir, select.value as 'pass' | 'pause' | 'ignore');
  select.value = state.wsPause?.[dir].mode || (state.wsPause?.[dir].paused ? 'pause' : 'pass');
}

defineProps<{ frames: WsFrame[] | null }>();

/** Is the connection still open? Only then can a frame be sent into it. */
const live = computed(() => !!state.wsPause?.live);

/** The directions this connection is currently holding, if it is still live. */
const holds = computed(() => {
  const p = state.wsPause;
  if (!p?.live) return [];
  return ([['send', p.send] as const, ['receive', p.receive] as const])
    .filter(([, d]) => d.paused)
    .map(([dir, d]) => ({ dir, held: d.held }));
});

// A frame list is not a snapshot: a held connection grows while you watch it,
// and so does an event stream — a body cut into frames arrives for as long as
// the server keeps writing, which for SSE is often minutes. So this polls for
// as long as the tab is open, and stops the moment it is not. The cost is one
// small request every two seconds while somebody is looking at exactly this —
// less often while the proxy does not answer, none while the tab is hidden
// (`poll.ts`).
const stopPolling = poll(async () => {
  if (state.selected !== null) await loadFrames(state.selected);
});
onUnmounted(stopPolling);
</script>

<template>
  <!-- The Frames composer: a message to either end of a connection that is
       still open. It is the one thing a capture cannot answer on its own —
       what the *other* side does with something it has not been sent yet. -->
  <div v-if="live" class="ws-compose">
    <select v-model="binary" aria-label="Frame format"><option :value="false">Text</option><option :value="true">Binary · Base64</option></select>
    <input
      v-model="state.wsCompose"
      class="ws-input"
      spellcheck="false"
      placeholder="a frame to send…"
      aria-label="Frame to send"
      @keydown.enter="sendWsFrame('send', state.wsCompose, binary)"
    />
    <button class="btn tiny" :disabled="wsBusy" @click="sendWsFrame('send', state.wsCompose, binary)">
      ▲ To server
    </button>
    <button class="btn tiny" :disabled="wsBusy" @click="sendWsFrame('receive', state.wsCompose, binary)">
      ▼ To client
    </button>
    <button class="btn tiny" :disabled="wsBusy" @click="picker?.click()">Load frame file</button>
    <input ref="picker" type="file" hidden @change="picked" />
  </div>
  <div v-if="live" class="ws-compose">
    <label v-for="dir in (['send', 'receive'] as const)" :key="dir">
      {{ dir === 'send' ? 'To server' : 'To client' }}
      <select :aria-label="`${dir} mode`" :disabled="wsBusy" :value="state.wsPause?.[dir].mode || (state.wsPause?.[dir].paused ? 'pause' : 'pass')" @change="modeChanged($event, dir)">
        <option value="pass">Pass</option><option value="pause">Pause</option><option value="ignore">Ignore</option>
      </select>
    </label>
    <button class="btn tiny" @click="disconnectWs()">Disconnect</button>
  </div>
  <p v-if="wsStatus" class="hint" role="status">{{ wsStatus }}</p>

  <div v-if="holds.length" class="holds">
    <div v-for="h in holds" :key="h.dir" class="hold">
      <span class="dir" :class="h.dir">{{ h.dir === 'send' ? '▲ send' : '▼ recv' }}</span>
      <span class="what">
        paused · {{ h.held }} frame{{ h.held === 1 ? '' : 's' }} held
      </span>
      <button class="btn tiny" :disabled="wsBusy" @click="releaseWsDir(h.dir)">Release</button>
    </div>
  </div>

  <div v-if="frames === null" class="empty">loading frames…</div>
  <div v-else-if="!frames.length" class="empty">no frames captured yet</div>
  <div v-else class="frames">
    <div
      v-for="(f, i) in frames"
      :key="i"
      class="frame"
      :class="[f.dir, { ignored: f.ignored, held: f.held }]"
    >
      <span class="dir">{{ f.dir === 'send' ? '▲ send' : '▼ recv' }}</span>
      <span class="op">{{ f.opcode }}</span>
      <span class="len">{{ f.len }} B</span>
      <span class="pv">{{ f.preview }}</span>
      <span v-if="f.held" class="waiting">held</span>
    </div>
  </div>
</template>

<style scoped>
.ws-compose {
  display: flex;
  gap: 0.4rem;
  align-items: center;
  padding: 0.4rem 0.5rem;
  border-bottom: 1px solid var(--line);
}
.ws-input {
  flex: 1;
  min-width: 6rem;
  font-family: var(--mono);
  font-size: 0.8rem;
}
</style>
