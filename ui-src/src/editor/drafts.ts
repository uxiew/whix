import { reactive, shallowReactive } from 'vue';

export interface Draft {
  text: string;
  /** Undefined until the first successful read: an empty editor is not data. */
  saved: string | undefined;
  loading: boolean;
  saving: boolean;
  status: string;
  readVersion: number;
}

export const isDirty = (draft: Draft): boolean =>
  draft.saved !== undefined && draft.text !== draft.saved;

/** In-memory only: rules and values can contain credentials. */
export function createDrafts<Key>() {
  const entries = shallowReactive(new Map<Key, Draft>());
  return {
    entries,
    get(key: Key): Draft {
      let draft = entries.get(key);
      if (!draft) {
        draft = reactive<Draft>({
          text: '', saved: undefined, loading: false, saving: false, status: '', readVersion: 0,
        });
        entries.set(key, draft);
      }
      return draft;
    },
    remove(key: Key): void {
      const draft = entries.get(key);
      if (draft) draft.readVersion++;
      entries.delete(key);
    },
    rename(from: Key, to: Key): void {
      const draft = entries.get(from);
      if (!draft) return;
      draft.readVersion++;
      entries.delete(from);
      entries.set(to, draft);
    },
  };
}

export function receiveDraft(draft: Draft, text: string): void {
  if (draft.saving) return;
  if (!isDirty(draft)) draft.text = text;
  draft.saved = text;
  if (draft.status === 'Could not reach the proxy') draft.status = '';
}

export async function loadDraft(draft: Draft, read: () => Promise<string>): Promise<void> {
  if (draft.saving) return;
  const version = ++draft.readVersion;
  draft.loading = true;
  try {
    const text = await read();
    if (version !== draft.readVersion) return;
    receiveDraft(draft, text);
    draft.status = '';
  } catch {
    if (version === draft.readVersion) draft.status = 'Could not reach the proxy';
  } finally {
    if (version === draft.readVersion) draft.loading = false;
  }
}

/** Confirm the submitted snapshot, not whatever the user typed while awaiting it. */
export async function saveDraft(
  draft: Draft,
  write: (text: string) => Promise<string>,
): Promise<boolean> {
  if (draft.saved === undefined || draft.saving) return false;
  const text = draft.text;
  draft.readVersion++;
  draft.loading = false;
  draft.saving = true;
  draft.status = '';
  try {
    draft.status = await write(text);
    draft.saved = text;
    return true;
  } catch (error) {
    draft.status = error instanceof Error ? error.message : 'Save failed';
    return false;
  } finally {
    draft.saving = false;
  }
}
