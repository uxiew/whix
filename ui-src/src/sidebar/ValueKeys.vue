<script setup lang="ts">
// The values, as a source list. Picking a key and picking what to edit are the
// same act, as on the Rules pane — "All values" is to this list what "Default"
// is to the group list: the whole store, edited as the one JSON object it is
// stored as.

import SideItem from '../components/SideItem.vue';
import SideTransfer from '../components/SideTransfer.vue';
import { addValue, exportValues, selectValue, state, valueDirty, valueNames } from '../store';
</script>

<template>
  <div class="side-title">Values</div>
  <SideItem
    label="All values"
    :dirty="valueDirty(null)"
    :selected="state.valueKey === null"
    @click="selectValue(null)"
  />

  <SideItem
    v-for="name in valueNames"
    :key="name"
    :label="name"
    :dirty="valueDirty(name)"
    :count="String(state.values[name] ?? '').length"
    :selected="state.valueKey === name"
    @click="selectValue(name)"
  />

  <div class="side-add" @click="addValue()">+ New value</div>
  <SideTransfer
    :export-title="state.valueKey === null ? 'Save every value as one JSON file' : `Save ${state.valueKey} as a file`"
    @export="exportValues()"
  />
</template>
