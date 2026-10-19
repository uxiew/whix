import type { ImportInput } from './api';

/** Classification only. The proxy validates every item and computes the merge. */
export function classifyImport(text: string, filename: string, pane: 'rules' | 'values'): ImportInput {
  const name = filename.replace(/\.(rules|txt|json)$/i, '') || filename;
  let data: unknown;
  try { data = JSON.parse(text); }
  catch {
    if (pane === 'rules' && (/^\s*[{[]/.test(text) || /\.json$/i.test(filename))) {
      throw new Error('Invalid rules JSON; nothing was imported.');
    }
  }
  if (data && typeof data === 'object' && !Array.isArray(data)) {
    const object = data as Record<string, unknown>;
    // A Value named "whix" is not a bundle; Whistle's explicit order marker
    // takes precedence over names that happen to look like our format marker.
    if (Object.hasOwn(object, '')) {
      return { format: pane === 'rules' ? 'whistle-rules' : 'whistle-values', data };
    }
    if (typeof object.whix === 'string' && (Object.hasOwn(object, 'rules') || Object.hasOwn(object, 'values'))) {
      return { format: 'whix', data };
    }
    if (pane === 'rules' || Object.values(object).every((v) => typeof v === 'string')) {
      return { format: pane === 'rules' ? 'whistle-rules' : 'whistle-values', data };
    }
  }
  if (pane === 'rules' && data !== undefined) throw new Error('Rules JSON must be an object of names to rule texts.');
  return { format: pane === 'rules' ? 'rules-text' : 'value-text', data: text, name };
}
