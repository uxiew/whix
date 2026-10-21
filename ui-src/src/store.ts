// One reactive store for the whole console.
//
// A plain `reactive` module rather than Pinia: there is one store, it is never
// instantiated twice, there is no SSR and no route to hydrate from, and the
// console ships as a file embedded in a binary — so the only thing a store
// library would add here is bytes. What Pinia is actually for (many stores,
// hot-swapped modules, devtools timelines) never comes up in a single window
// with five panes.

import { computed, reactive, ref, shallowRef, watch } from 'vue';
import { api } from './api';
import type { ImportInput } from './api';
import { classifyImport } from './import-file';
import { base64Of, bytesOf, bodyFile } from './bytes';
// The filter grammar, imported for its side effect: the module is plain
// script-shaped JavaScript with no exports, because the same file is evaluated
// by a Rust test in the engine the proxy already carries. One copy, so the
// console and the test cannot drift. See the note at the top of the file.
import './filter/session-filter.js';
import type {
  Composition,
  ExplainQuery,
  Explanation,
  PageLog,
  ProxyStatus,
  ReplayedSession,
  RuleGroup,
  SessionDetail,
  SessionSummary,
  SwitchPatch,
  Switches,
  WsFrame,
  WsPauseStatus,
} from './api';
import { COLUMNS } from './columns';
import { clientOf, fmtBytes } from './format';
import { createDrafts, isDirty, loadDraft, receiveDraft, saveDraft } from './editor/drafts';

export type Pane = 'requests' | 'composer' | 'console' | 'rules' | 'values' | 'test' | 'status';
export type DetailTab =
  | 'general'
  | 'rules'
  | 'req-head'
  | 'res-head'
  | 'req-body'
  | 'res-body'
  | 'frames';
export type Theme = 'light' | 'dark';

// Rules sits second, right after General: "which rules matched this request" is
// the question the proxy exists to answer, and it used to be the one thing the
// console could not tell you.
export const DETAIL_TABS: { key: DetailTab; label: string }[] = [
  { key: 'general', label: 'General' },
  { key: 'rules', label: 'Rules' },
  { key: 'req-head', label: 'Request Header' },
  { key: 'res-head', label: 'Response Header' },
  { key: 'req-body', label: 'Request Body' },
  { key: 'res-body', label: 'Response Body' },
  { key: 'frames', label: 'Frames' },
];

const THEME_KEY = 'whix-theme';
const COMPOSE_KEY = 'whix-composer';
const TEST_KEY = 'whix-test-rules';
const COMPOSE_HISTORY_KEY = 'whix-composer-history';
const CAPTURE_KEY = 'whix-capture-filter';

/** How many sent compositions the source list keeps. Upstream keeps 100. */
const COMPOSE_HISTORY_MAX = 20;

interface State {
  /** What pages under a `log://` rule have written, oldest first. */
  pageLogs: PageLog[];
  /** Every `log://` id the proxy holds entries for. */
  pageLogIds: string[];
  /** The highest sequence number seen — the cursor the next poll asks after. */
  pageLogLast: number;
  /** The group being shown; `null` = all of them. */
  pageLogId: string | null;
  pane: Pane;
  theme: Theme;
  filter: string;
  autoRefresh: boolean;

  sessions: SessionSummary[];
  /** `null` = every client. */
  client: string | null;
  /** The row the detail panel is showing — one of `selection`, or `null`. */
  selected: number | null;
  /** Every row the table has selected, in the order they were picked. */
  selection: number[];
  /** Where a shift-click measures its range from. */
  anchor: number | null;
  /** Rows flagged by hand — see [`toggleMark`]. */
  marked: number[];
  markedOnly: boolean;
  /**
   * The capture filters — `gui/network.md`'s Include/Exclude Filter. They
   * apply to requests that arrive *after* they are set, and a row already on
   * screen stays there, which is what sets them apart from the search box.
   *
   * They decide what **this console shows**, not what the proxy records: a
   * refused request is still in `/sessions.json`, in a HAR export and in the
   * history on disk. Keeping a request out of the record is a rule's job —
   * `enable://hide` — and is not done from here.
   */
  captureInclude: string;
  captureExclude: string;
  detail: SessionDetail | null;
  frames: WsFrame[] | null;
  /** Whether the selected WebSocket is being held — `null` until asked. */
  wsPause: WsPauseStatus | null;
  detailTab: DetailTab;
  sort: { key: string; dir: 'asc' | 'desc' };
  prettyBody: boolean;
  /** Bumped when the selection moved by keyboard and wants scrolling into view. */
  revealSeq: number;
  /** The value being edited, or `null` for the whole store as one JSON object. */
  valueKey: string | null;
  /** A transient message shown where the request count normally is. */
  note: string | null;
  /** True once a call to the proxy failed, until one succeeds. */
  offline: boolean;

  groups: RuleGroup[];
  group: string;

  values: Record<string, string>;

  /** The request being written in the Composer, and the ones already sent. */
  compose: Composition;
  composeStatus: string;
  composeHistory: Composition[];

  status: ProxyStatus | null;
  /** HTTPS, every rule, the plugins — see `api.Switches`. */
  switches: Switches | null;

  /** Test Rules: the question, and the last answer. */
  /** The frame the Frames panel's composer is holding. */
  wsCompose: string;

  test: TestQuery;
  testResult: Explanation | null;
  testStatus: string;
}

/** What the Test Rules pane holds between visits. */
export interface TestQuery {
  rules: string;
  url: string;
  method: string;
  headers: string;
  body: string;
  /** Empty for "ask about the request phase only". */
  status: string;
}

/** What Test Rules opens on. */
function blankTest(): TestQuery {
  return { rules: '', url: '', method: 'GET', headers: '', body: '', status: '' };
}

/** What the Composer opens on, and what "New request" goes back to. */
function blankComposition(): Composition {
  return { method: 'GET', url: '', headers: '', body: '', body_encoding: 'utf8' };
}

/** The method as it will be *sent*: trimmed, uppercased, empty meaning GET. */
export const methodOf = (c: Composition): string => (c.method.trim() || 'GET').toUpperCase();

function readStored<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    // Private mode, or a value written by a version that shaped it differently.
    return fallback;
  }
}

function writeStored(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* private mode */
  }
}

export const state = reactive<State>({
  pageLogs: [],
  pageLogIds: [],
  pageLogLast: 0,
  pageLogId: null,
  pane: 'requests',
  theme: 'light',
  filter: '',
  autoRefresh: true,

  sessions: [],
  client: null,
  selected: null,
  selection: [],
  anchor: null,
  marked: [],
  markedOnly: false,
  captureInclude: readStored<{ inc: string; exc: string }>(CAPTURE_KEY, { inc: '', exc: '' }).inc,
  captureExclude: readStored<{ inc: string; exc: string }>(CAPTURE_KEY, { inc: '', exc: '' }).exc,
  detail: null,
  frames: null,
  wsPause: null,
  detailTab: 'general',
  sort: { key: 'id', dir: 'desc' },
  prettyBody: true,
  revealSeq: 0,
  valueKey: null,
  note: null,
  offline: false,

  groups: [],
  group: 'default',

  values: {},

  // A half-written request survives a reload. It has to: the console is served
  // by the proxy you are reconfiguring, so the page is reloaded far more often
  // here than in an application you would merely be using.
  compose: readStored(COMPOSE_KEY, blankComposition()),
  wsCompose: '',
  test: readStored(TEST_KEY, blankTest()),
  testResult: null,
  testStatus: '',
  composeStatus: '',
  composeHistory: readStored<Composition[]>(COMPOSE_HISTORY_KEY, []),

  status: null,
  switches: null,
});

watch(
  () => [state.captureInclude, state.captureExclude],
  ([inc, exc]) => writeStored(CAPTURE_KEY, { inc, exc }),
);
function storedComposition(c: Composition): Composition {
  return c.body_encoding === 'base64' && c.body
    ? { ...c, body: '', body_incomplete: true, allow_partial: false } : c;
}
watch(() => state.compose, (c) => writeStored(COMPOSE_KEY, storedComposition(c)), { deep: true });
watch(() => state.test, (t) => writeStored(TEST_KEY, t), { deep: true });

/** A parsed condition; `remote` ones are answered by the proxy under `key`. */
interface FilterCondition {
  field: string;
  remote?: boolean;
  key?: string;
}

/* eslint-disable @typescript-eslint/no-explicit-any */
const parseFilter = (globalThis as any).whistleParseFilter as (
  q: string,
  opts?: { remote: boolean },
) => { conditions: FilterCondition[]; unsupported: { prefix: string; why: string }[] };
const matchSession = (globalThis as any).whistleMatchSession as (
  s: SessionSummary,
  c: FilterCondition[],
  ctx: { marked: number[]; remote?: Record<string, Set<number>> },
) => boolean;
const matchAny = (globalThis as any).whistleMatchAny as (
  s: SessionSummary,
  c: FilterCondition[],
  ctx: { marked: number[] },
) => boolean;

/**
 * Rows the capture filters have already let through.
 *
 * The filters apply to what *arrives*, not to what is on screen — upstream says
 * so in as many words, and it is the useful behaviour: tightening a filter to
 * silence a poller should not make the request you are reading vanish. Since the
 * console re-reads the whole list on every poll rather than accumulating, the
 * only way to keep that promise is to remember what was already admitted.
 */
const admitted = new Set<number>();

/** How many arriving rows the capture filters have refused. */
const refused = ref(0);

/** The refused count, for the status line — a filter nobody can see is a trap. */
export const captureRefused = computed(() => refused.value);

/** Whether either capture filter has anything in it. */
export const captureFiltering = computed(
  () => !!(state.captureInclude.trim() || state.captureExclude.trim()),
);

/**
 * What the capture filters cannot act on, with the reason. Without this a box
 * holding only `h:cookie` dropped the condition and let everything through,
 * and nothing on screen said the filter was not filtering.
 */
export const captureGaps = computed(() => {
  const gaps = [
    ...parseFilter(state.captureInclude).unsupported,
    ...parseFilter(state.captureExclude).unsupported,
  ];
  return gaps.filter((g, i) => gaps.findIndex((o) => o.prefix === g.prefix) === i);
});

/** Apply the capture filters to a freshly fetched list. */
function admit(list: SessionSummary[]): SessionSummary[] {
  const inc = parseFilter(state.captureInclude).conditions;
  const exc = parseFilter(state.captureExclude).conditions;
  if (!inc.length && !exc.length) {
    for (const s of list) admitted.add(s.id);
    refused.value = 0;
    return list;
  }
  const ctx = { marked: state.marked };
  let dropped = 0;
  const kept = list.filter((s) => {
    if (admitted.has(s.id)) return true;
    // Include is a whitelist, exclude a blacklist, and the two are AND-ed —
    // conditions *within* a box are OR-ed, which `matchAny` does.
    const ok = (!inc.length || matchAny(s, inc, ctx)) && !(exc.length && matchAny(s, exc, ctx));
    if (ok) admitted.add(s.id);
    else dropped++;
    return ok;
  });
  refused.value = dropped;
  return kept;
}

// ── derived ────────────────────────────────────────────────────────────────

/**
 * The filter box's query, parsed.
 *
 * `whistle`'s little language: a bare word matches the URL, `m:` the method,
 * `s:` the status, and so on, with several conditions AND-ed — see
 * `filter/session-filter.js`, which is the one copy of the grammar and is tested
 * from Rust against the same table.
 */
const parsedFilter = computed(() => parseFilter(state.filter, { remote: true }));

/** The prefixes in the box this console has no answer for, with the reason. */
export const filterGaps = computed(() => parsedFilter.value.unsupported);

/** The box's `h:`/`b:` conditions, as the proxy is asked them. */
const remoteKeys = computed(() =>
  parsedFilter.value.conditions.filter((c) => c.remote).map((c) => c.key as string),
);

/**
 * The proxy's answers to the box's `h:`/`b:` — see `/api/sessions/search`.
 *
 * `maybe` is each answer widened by the rows a `b:` could not rule out: no
 * match in what was kept of a body cut short. The rows only the wider answers
 * let through are counted beside the total, because a body search that
 * silently read 16 KB of a 2 MB response would pass for "not in there".
 */
export const remoteSearch = reactive({
  answers: {} as Record<string, Set<number>>,
  maybe: {} as Record<string, Set<number>>,
  scanned: 0,
  asking: false,
  error: null as string | null,
});

let searchSeq = 0;

/** Ask the proxy the box's `h:`/`b:` over everything it holds now. */
async function refreshRemote(): Promise<void> {
  const keys = remoteKeys.value;
  const seq = ++searchSeq;
  if (!keys.length) {
    Object.assign(remoteSearch, { answers: {}, maybe: {}, scanned: 0, asking: false, error: null });
    return;
  }
  remoteSearch.asking = true;
  try {
    const res = await api.searchSessions(keys);
    if (seq !== searchSeq) return;
    const answers: Record<string, Set<number>> = {};
    const maybe: Record<string, Set<number>> = {};
    for (const r of res.results) {
      answers[r.condition] = new Set(r.ids);
      maybe[r.condition] = new Set([...r.ids, ...(r.partly_kept ?? [])]);
    }
    Object.assign(remoteSearch, { answers, maybe, scanned: res.scanned, asking: false, error: null });
  } catch (e) {
    if (seq !== searchSeq) return;
    const error = e instanceof Error ? e.message : String(e);
    Object.assign(remoteSearch, { answers: {}, maybe: {}, asking: false, error });
  }
}

let searchTimer: number | undefined;
// Typing asks once the typing stops, not at every keystroke.
watch(remoteKeys, () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => void refreshRemote(), 250) as unknown as number;
});

/** The sessions the source list and the filter box agree on. */
const visibleSessions = computed(() => shownUnder(remoteSearch.answers));

/**
 * Rows hidden only because a `b:` read part of their body: every other
 * condition holds, and the part not kept could hold the match.
 */
const notRuledOut = computed(() => {
  if (!Object.keys(remoteSearch.maybe).length) return 0;
  return shownUnder(remoteSearch.maybe).length - visibleSessions.value.length;
});

/** The rows the list shows, given these answers for the remote conditions. */
function shownUnder(remote: Record<string, Set<number>>): SessionSummary[] {
  const { conditions } = parsedFilter.value;
  const ctx = { marked: state.marked, remote };
  return state.sessions.filter((s) => {
    if (state.client && clientOf(s) !== state.client) return false;
    if (state.markedOnly && !state.marked.includes(s.id)) return false;
    if (!conditions.length) return true;
    return matchSession(s, conditions, ctx);
  });
}

/** The rows as currently shown, which is what the arrow keys move through. */
export const shownRows = computed(() => {
  const col = COLUMNS.find((c) => c.key === state.sort.key) || COLUMNS[0];
  const sign = state.sort.dir === 'asc' ? 1 : -1;
  return visibleSessions.value.slice().sort((a, b) => {
    const x = col.get(a);
    const y = col.get(b);
    if (x === y) return (a.id - b.id) * sign;
    if (typeof x === 'number' && typeof y === 'number') return (x - y) * sign;
    return String(x).localeCompare(String(y)) * sign;
  });
});

export const selectedSession = computed(
  () => state.sessions.find((s) => s.id === state.selected) || null,
);

/** How many requests are in view, and out of how many — or a transient note. */
export const countLabel = computed(() => {
  if (state.note) return state.note;
  // Worth saying plainly: a console polling a proxy that has stopped answering
  // otherwise looks exactly like a proxy through which nothing is passing.
  if (state.offline) return 'proxy not answering';
  const shown = shownRows.value.length;
  const total = state.sessions.length;
  const count = shown === total ? `${total} requests` : `${shown} of ${total} requests`;
  return count + remoteCount();
});

/**
 * What the count adds about the box's `h:`/`b:`: that the proxy is still being
 * asked, or how many rows a `b:` could not rule out. Beside the count, where
 * it qualifies the number, rather than over the table.
 */
function remoteCount(): string {
  if (!remoteKeys.value.length || remoteSearch.error) return '';
  if (remoteSearch.asking && !Object.keys(remoteSearch.answers).length) return ' · asking the proxy';
  const n = notRuledOut.value;
  if (!n) return '';
  return ` · ${n} not ruled out: ${n === 1 ? 'its body was' : 'their bodies were'} only partly kept`;
}

/** One entry per client seen, newest capture included, sorted by address. */
export const clientCounts = computed(() => {
  const counts = new Map<string, number>();
  for (const s of state.sessions) {
    const c = clientOf(s);
    counts.set(c, (counts.get(c) || 0) + 1);
  }
  return [...counts.keys()].sort().map((name) => ({ name, count: counts.get(name)! }));
});

/**
 * Whether a detail tab has anything behind it.
 *
 * A tab that would open on an empty panel is disabled instead: with the header
 * and body tabs sitting side by side, "did this request have a body" is a
 * question the tab strip can answer without being clicked.
 *
 * **Rules is the exception, deliberately.** "No rule matched" is an answer, and
 * a greyed-out tab does not give it — it reads the same as a tab whose contents
 * have not loaded. The panel says it in words instead.
 */
export function tabEnabled(key: DetailTab): boolean {
  const s = selectedSession.value;
  const d = state.detail;
  if (!s) return false;
  switch (key) {
    case 'req-head': return !!d?.req_headers?.length;
    case 'res-head': return !!d?.res_headers?.length;
    case 'req-body': return !!d?.req_body?.len;
    case 'res-body': return !!d?.res_body?.len;
    case 'frames': return s.status === 101 || !!s.has_frames;
    default: return true;
  }
}

// ── theme ──────────────────────────────────────────────────────────────────

export function applyTheme(mode: Theme): void {
  state.theme = mode;
  document.documentElement.setAttribute('data-theme', mode);
  try {
    localStorage.setItem(THEME_KEY, mode);
  } catch {
    /* private mode */
  }
}

export function initTheme(): void {
  let saved: string | null = null;
  try {
    saved = localStorage.getItem(THEME_KEY);
  } catch {
    /* private mode */
  }
  const dark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
  applyTheme(saved === 'dark' || saved === 'light' ? saved : dark ? 'dark' : 'light');
}

// ── talking to the proxy ───────────────────────────────────────────────────

/**
 * Make one call, and remember whether the proxy answered.
 *
 * Every read goes through here so that a proxy that has gone away is reported
 * once, in the console's own furniture, rather than as a stack trace in the
 * browser's — this page polls every two seconds, and it is the one page you
 * cannot open the devtools of without leaving what you were looking at.
 */
async function reach<T>(call: () => Promise<T>): Promise<T | undefined> {
  try {
    const value = await call();
    state.offline = false;
    return value;
  } catch {
    state.offline = true;
    return undefined;
  }
}

// ── requests ───────────────────────────────────────────────────────────────

export async function loadSessions(): Promise<void> {
  const fetched = await reach(api.sessions);
  if (!fetched) return;
  const list = admit(fetched);
  state.sessions = list;
  if (state.client && !list.some((s) => clientOf(s) === state.client)) state.client = null;
  // A session that has fallen out of the proxy's ring takes its selection and
  // its mark with it: both name a row by an id that now belongs to nothing.
  const live = new Set(list.map((s) => s.id));
  // A row that has left the proxy's ring takes its admission with it, so the
  // set cannot grow without bound.
  for (const id of [...admitted]) if (!live.has(id)) admitted.delete(id);
  state.selection = state.selection.filter((id) => live.has(id));
  state.marked = state.marked.filter((id) => live.has(id));
  if (state.markedOnly && !state.marked.length) state.markedOnly = false;
  if (state.anchor !== null && !live.has(state.anchor)) state.anchor = null;
  if (state.selected !== null && !live.has(state.selected)) {
    state.selected = null;
    state.detail = null;
    state.frames = null;
    state.wsPause = null;
  }
  // New rows have not been searched yet: ask again over what is held now.
  if (remoteKeys.value.length) void refreshRemote();
}

/** What a click carried, as the table's modifier keys mean it. */
export interface Pick {
  /** ⌘/Ctrl: add this row to the selection, or take it out. */
  toggle?: boolean;
  /** Shift: select everything between the anchor and this row. */
  extend?: boolean;
}

export async function selectRow(id: number, pick: Pick = {}): Promise<void> {
  const rows = shownRows.value.map((s) => s.id);
  const from = state.anchor === null ? -1 : rows.indexOf(state.anchor);
  if (pick.extend && from >= 0 && rows.includes(id)) {
    // The range runs through the rows *as shown*, not by id: the table sorts,
    // and a shift-click means "these, between here and there".
    const to = rows.indexOf(id);
    state.selection = rows.slice(Math.min(from, to), Math.max(from, to) + 1);
  } else if (pick.toggle) {
    state.selection = state.selection.includes(id)
      ? state.selection.filter((x) => x !== id)
      : [...state.selection, id];
    state.anchor = id;
  } else {
    state.selection = [id];
    state.anchor = id;
  }
  // Un-picking the row being shown moves the panel to whatever is still picked,
  // rather than leaving it on a row the table no longer highlights.
  await showDetail(state.selection.includes(id) ? id : (state.selection.at(-1) ?? null));
}

/** Fill the detail panel from one session, or empty it. */
async function showDetail(id: number | null): Promise<void> {
  if (id === state.selected) return;
  state.selected = id;
  state.detail = null;
  state.frames = null;
  state.wsPause = null;
  if (id === null) return;
  const detail = await reach(() => api.session(id));
  if (state.selected !== id || detail === undefined) return;
  state.detail = detail;
}

export function clearSelection(): void {
  state.selection = [];
  state.anchor = null;
  // `showDetail(null)` is what empties the panel — including the pause banner,
  // which is read from the session being shown.
  void showDetail(null);
}

/**
 * Move the selection `delta` rows through the list, and keep it in view.
 * With `extend`, the anchor stays put and the range grows — shift-arrow, the
 * keyboard's spelling of a shift-click.
 */
export function moveSelection(delta: number, extend = false): void {
  const list = shownRows.value;
  if (!list.length) return;
  const at = list.findIndex((s) => s.id === state.selected);
  const next =
    at < 0
      ? delta > 0
        ? 0
        : list.length - 1
      : Math.max(0, Math.min(list.length - 1, at + delta));
  void selectRow(list[next].id, { extend });
  state.revealSeq++;
}

/**
 * Flag rows, or clear the flag — all of them at once, so a marked selection
 * un-marks and a mixed one marks.
 *
 * Marks live in this window and nowhere else, deliberately. A mark names a
 * session by the id the proxy gave it, and those ids start again at 1 every
 * time the proxy restarts — kept in `localStorage`, a mark would come back
 * attached to whatever request took its number next, which is worse than not
 * keeping it. Nor is there anywhere on the proxy to put it: the session ring
 * records what crossed the wire, not what someone thought about it.
 */
export function toggleMark(ids: number[]): void {
  if (!ids.length) return;
  state.marked = ids.every((id) => state.marked.includes(id))
    ? state.marked.filter((id) => !ids.includes(id))
    : [...new Set([...state.marked, ...ids])];
  if (!state.marked.length) state.markedOnly = false;
}

/** The rows the bulk actions act on: the selection, or the one row shown. */
export const actingOn = computed(() =>
  state.selection.length ? state.selection : state.selected === null ? [] : [state.selected],
);

/** Forget the selected sessions, and only those. */
export async function clearSelected(): Promise<void> {
  const ids = actingOn.value.slice();
  if (!ids.length) return;
  if (!(await reach(() => api.clearSessions(ids)))) return;
  clearSelection();
  await loadSessions();
}

export function toggleSort(key: string): void {
  state.sort =
    state.sort.key === key
      ? { key, dir: state.sort.dir === 'asc' ? 'desc' : 'asc' }
      : { key, dir: key === 'id' || key === 'time_ms' ? 'desc' : 'asc' };
}

/**
 * Send a frame into the selected live WebSocket session.
 *
 * `send` puts it on its way to the server, as if the client had sent it;
 * `receive` on its way to the client. It is recorded like any other frame,
 * because it is one — the peer cannot tell it from traffic.
 */
export const wsBusy = ref(false);
export const wsStatus = ref('');
export async function sendWsFrame(dir: 'send' | 'receive', data: string, binary = false): Promise<void> {
  const id = state.selected;
  if (id === null || wsBusy.value) return;
  wsBusy.value = true;
  const answer = await reach(() => api.wsSend(id, dir, data, binary));
  wsBusy.value = false;
  if (state.selected !== id) return;
  wsStatus.value = answer?.ok ? 'Frame sent' : answer?.error || 'Could not reach the proxy';
  if (answer?.ok) {
    if (state.wsCompose === data) state.wsCompose = '';
    await loadFrames(id);
  }
}

export async function setWsMode(dir: 'send' | 'receive', mode: 'pass' | 'pause' | 'ignore'): Promise<void> {
  const id = state.selected;
  if (id === null || wsBusy.value) return;
  framesLoadVersion++;
  wsBusy.value = true;
  const answer = await reach(() => api.controlWs(id, dir, mode));
  wsBusy.value = false;
  if (state.selected !== id) return;
  wsStatus.value = answer?.ok ? `${dir}: ${mode}` : answer?.error || 'Control failed';
  await loadFrames(id);
}

export async function disconnectWs(): Promise<void> {
  const id = state.selected;
  if (id === null) return;
  framesLoadVersion++;
  const answer = await reach(() => api.abortWs(id));
  if (state.selected !== id) return;
  wsStatus.value = answer?.ok ? 'Disconnected' : answer?.error || 'Disconnect failed';
  await loadFrames(id);
}

/**
 * Ask the proxy which rules a request would hit — whistle's **Test Rules**.
 *
 * The rules under test are whatever is in the editor, not what the proxy is
 * running: that is the point of the panel. Everything else is optional, and the
 * status field turns the question into one about the *response* phase, where a
 * `resHeaders://` guarded by `includeFilter://s:404` finally has an answer.
 */
export async function runTest(): Promise<void> {
  const t = state.test;
  if (!t.url.trim()) {
    state.testStatus = 'a URL to test against';
    return;
  }
  const headers: Record<string, string> = {};
  for (const line of t.headers.split('\n')) {
    const at = line.indexOf(':');
    if (at > 0) headers[line.slice(0, at).trim()] = line.slice(at + 1).trim();
  }
  const status = parseInt(t.status, 10);
  const query: ExplainQuery = {
    rules: t.rules,
    url: t.url.trim(),
    method: t.method.trim() || 'GET',
    headers,
    body: t.body || undefined,
    response: status > 0 ? { status, headers: {} } : undefined,
  };
  state.testStatus = 'testing…';
  const answer = await reach(() => api.explain(query));
  if (!answer) {
    state.testStatus = 'the proxy did not answer';
    return;
  }
  if (answer.error) {
    state.testResult = null;
    state.testStatus = answer.error;
    return;
  }
  state.testResult = answer;
  const n = answer.ops.length;
  state.testStatus = n ? `${n} operator${n === 1 ? '' : 's'}` : 'no rule matched';
}

/** Fill the editor with the rules the proxy is running. */
export async function testCurrentRules(): Promise<void> {
  const text = await reach(() => api.rules());
  if (typeof text === 'string') state.test.rules = text;
}

let framesLoadVersion = 0;
export async function loadFrames(id: number): Promise<void> {
  const version = ++framesLoadVersion;
  const list = await reach(() => api.frames(id));
  if (state.selected !== id || version !== framesLoadVersion || !list) return;
  // `/frames.json` answers newest-first; a conversation reads oldest-first.
  state.frames = list.slice().reverse();
  // Whether a direction is being held has to be asked for separately: a pause
  // that has caught nothing yet is invisible in the frames themselves.
  const pause = await reach(() => api.wsPause(id));
  if (state.selected === id && version === framesLoadVersion && pause) state.wsPause = pause;
}

/**
 * Let one held direction of the selected session go.
 *
 * All of it at once, because that is the only granularity whistle has: its own
 * console sets the direction's status back to normal and everything held goes
 * out together. A session that has closed in the meantime answers plainly
 * rather than silently doing nothing — its frames are never getting out now.
 */
export async function releaseWsDir(dir: 'send' | 'receive'): Promise<void> {
  const id = state.selected;
  if (id === null || wsBusy.value) return;
  framesLoadVersion++;
  wsBusy.value = true;
  const res = await reach(() => api.releaseWs(id, dir));
  wsBusy.value = false;
  if (!res || state.selected !== id) return;
  wsStatus.value = res.ok ? `${dir}: pass` : res.error || 'Release failed';
  flashNote(
    res.ok ? `Released ${res.released ?? 0} held ${dir} frame(s)` : res.error || 'Release failed',
  );
  await loadFrames(id);
}

export async function clearSessions(): Promise<void> {
  if (!(await reach(api.clearSessions))) return;
  clearSelection();
  await loadSessions();
}

/** Clear, and delete the saved history on disk — the one that does not come back. */
export async function purgeSessions(): Promise<void> {
  const res = await reach(api.purgeSessions);
  if (!res) return;
  clearSelection();
  await loadSessions();
  flashNote(`Deleted the session history (${res.files_deleted ?? 0} file(s) on disk)`);
}

export async function replaySelected(): Promise<void> {
  const ids = actingOn.value.slice();
  if (!ids.length) return;
  const res = await reach(() => api.replay(ids));
  if (!res) return;
  if (res.error) {
    // Refused — the row has left the proxy's list, say. It was read as a
    // replay of nothing and announced as "Replayed".
    flashNote(`Not replayed: ${res.error}`);
    return;
  }
  // One replay is reported in full — whether its body survived the capture is
  // the thing worth saying. A batch reports the count, and how many of them
  // did not go out with the body they were captured with: naming which of
  // twenty belongs on the rows, but "all fine" must not be implied.
  flashNote(ids.length > 1 ? batchNote(res) : replayNote(res.sessions?.[0]));
  // The replay is fired off asynchronously by the proxy; give it a moment to
  // come back around through the capture before asking for the list again.
  setTimeout(() => void loadSessions(), 400);
}

/**
 * What to say about a replay that went out.
 *
 * A replay is rebuilt from the *captured* body — a decoded, capped preview —
 * so it is not always the request it was made from. Saying so is the point:
 * a replay that silently dropped 190 KB of a 200 KB upload and came back 200
 * would be read as proof the endpoint works.
 */
function replayNote(r: ReplayedSession | undefined): string {
  switch (r?.body) {
    case 'partial':
      return `Replayed · body cut to ${fmtBytes(r.sent)} of ${fmtBytes(r.captured)}`;
    case 'undecodable':
      return 'Replayed without its body · the capture would not decode';
    case 'whole':
      return `Replayed with its ${fmtBytes(r.sent)} body`;
    default:
      return 'Replayed';
  }
}

/** A batch replay's note: the count, and the ones whose body was not whole. */
function batchNote(res: { replayed: number; sessions?: ReplayedSession[] }): string {
  const count = (kind: string) => (res.sessions ?? []).filter((r) => r.body === kind).length;
  const cut = count('partial');
  const lost = count('undecodable');
  const said = [`Replayed ${res.replayed} requests`];
  if (cut) said.push(`${cut} with the body cut short`);
  if (lost) said.push(`${lost} without a body that would not decode`);
  return said.join(' · ');
}

let noteTimer: number | undefined;

/** Say something where the request count normally is, briefly. */
export function flashNote(text: string): void {
  state.note = text;
  clearTimeout(noteTimer);
  noteTimer = setTimeout(() => {
    state.note = null;
  }, 1600) as unknown as number;
}

export async function copyText(text: string, note: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // No clipboard permission (or no clipboard API): fall back to the old way.
    const ta = document.createElement('textarea');
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try {
      document.execCommand('copy');
    } finally {
      ta.remove();
    }
  }
  flashNote(note);
}

// ── composer ───────────────────────────────────────────────────────────────

/**
 * Open a captured request in the Composer, ready to be edited and sent again.
 *
 * The most valuable thing the Composer does: almost nothing anyone composes is
 * written from nothing — it is a request that already happened, with one header
 * changed. `curl.ts` renders the same request for a terminal; this is the same
 * journey without leaving the console.
 *
 * `content-length` and `host` are left out on purpose. Both are recomputed by
 * the proxy from the URL and the body as they stand when Send is pressed, so
 * seeding them would only offer a stale number to edit.
 */
export const composeLoading = ref(false);
export const composeSending = ref(false);
let compositionVersion = 0;

export async function composeFrom(s: SessionSummary, d: SessionDetail | null): Promise<void> {
  const version = ++compositionVersion;
  state.compose = {
    method: s.method, url: s.url,
    headers: '', body: '', body_encoding: 'base64',
    // Until the matching detail arrives, never seed another row's headers.
    body_incomplete: true,
  };
  state.composeStatus = `Loading body from #${s.id}…`;
  composeLoading.value = true;
  showPane('composer');
  try {
    const detail = d?.id === s.id ? d : await api.session(s.id);
    if (version !== compositionVersion) return;
    if (!detail) throw new Error('Session unavailable');
    state.compose.headers = (detail.req_headers ?? [])
      .filter(([name]) => !name.startsWith(':') && !/^(content-length|host|transfer-encoding|content-encoding)$/i.test(name))
      .map(([name, value]) => `${name}: ${value}`).join('\n');
    state.compose.body_incomplete = !!detail.req_body && (!!detail.req_body.truncated || !!detail.req_body.undecodable || !!detail.open);
    if (detail.req_body) {
      const body = await api.bodyBytes(s.id, 'req');
      if (version !== compositionVersion) return;
      state.compose.body = base64Of(body.bytes);
      state.compose.body_incomplete ||= body.truncated;
    }
    state.composeStatus = `Seeded from #${s.id} · exact retained body bytes; framing headers recalculated`;
  } catch {
    if (version === compositionVersion) {
      state.compose.body_incomplete = true;
      state.composeStatus = 'Could not load the body; load a complete body file before sending';
    }
  } finally { if (version === compositionVersion) composeLoading.value = false; }
}

export async function loadCompositionFile(file: File): Promise<void> {
  const version = ++compositionVersion;
  composeLoading.value = true;
  try {
    const body = await bodyFile(file);
    if (version !== compositionVersion) return;
    state.compose.body = body;
    state.compose.body_encoding = 'base64';
    state.compose.body_incomplete = false;
    state.compose.allow_partial = false;
    if (!/^content-type:/im.test(state.compose.headers)) state.compose.headers += `${state.compose.headers ? '\n' : ''}Content-Type: ${file.type || 'application/octet-stream'}`;
    state.composeStatus = `Loaded ${file.name} · ${fmtBytes(file.size)}`;
  } catch (e) {
    if (version === compositionVersion) state.composeStatus = e instanceof Error ? e.message : 'File load failed';
  } finally { if (version === compositionVersion) composeLoading.value = false; }
}

export function compositionEncoding(encoding: 'utf8' | 'base64'): void {
  if (composeLoading.value || (state.compose.body_encoding || 'utf8') === encoding) return;
  try {
    state.compose.body = encoding === 'base64'
      ? base64Of(new TextEncoder().encode(state.compose.body))
      : new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytesOf(state.compose.body));
    state.compose.body_encoding = encoding;
  } catch { state.composeStatus = 'Cannot convert this body to the selected encoding; keep Base64 for binary or raw multipart'; }
}

/** Load one of the sent compositions back into the editor. */
export function useComposition(c: Composition): void {
  compositionVersion++; composeLoading.value = false;
  state.compose = { ...c };
  state.composeStatus = '';
}

export function newComposition(): void {
  compositionVersion++; composeLoading.value = false;
  state.compose = blankComposition();
  state.composeStatus = '';
}

/** Put a composition at the head of the history, deduplicated as upstream's is. */
function remember(c: Composition): void {
  const same = (h: Composition) =>
    h.method === c.method && h.url === c.url && h.headers === c.headers && h.body === c.body && (h.body_encoding || 'utf8') === (c.body_encoding || 'utf8') && !!h.body_incomplete === !!c.body_incomplete;
  state.composeHistory = [c, ...state.composeHistory.filter((h) => !same(h))].slice(
    0,
    COMPOSE_HISTORY_MAX,
  );
  writeStored(COMPOSE_HISTORY_KEY, state.composeHistory.map(storedComposition));
}

/**
 * Send what is in the Composer through the proxy.
 *
 * The proxy sends it to *itself*, so the composition is matched, rewritten and
 * captured like any other request rather than being a private conversation
 * between the console and an origin — which is what makes the Composer a way to
 * test rules and not merely a second curl.
 */
export async function sendComposition(): Promise<void> {
  if (composeLoading.value || composeSending.value) return;
  if (state.compose.body_incomplete && !state.compose.allow_partial) { state.composeStatus = 'Load a complete body or explicitly allow the captured prefix'; return; }
  const sending = { ...state.compose };
  if (!sending.url.trim()) {
    state.composeStatus = 'A URL is required';
    return;
  }
  // Everything already captured, so the request this one becomes can be told
  // apart from the traffic that was there before it.
  const before = state.sessions.reduce((max, s) => Math.max(max, s.id), 0);
  composeSending.value = true;
  const res = await reach(() => api.compose(sending));
  composeSending.value = false;
  if (!res) {
    state.composeStatus = 'Could not reach the proxy';
    return;
  }
  if (!res.ok) {
    state.composeStatus = res.error || 'The proxy refused it';
    return;
  }
  remember(sending);
  const url = res.url || sending.url;
  state.composeStatus = `Sent · ${url}`;
  flashNote(`Sent ${methodOf(sending)} ${url}`);
  // Fire-and-forget on the proxy's side, as a replay is: the answer arrives in
  // the session list, not here, so give it the same moment to come back around.
  setTimeout(() => void revealComposed(sending, url, before), 400);
}

/**
 * Show what the composition became.
 *
 * The Composer's result is a row in the request list, so that is where this
 * goes — and it selects the row when it can recognise it. It often cannot: a
 * rule that rewrote the URL makes the captured request a different one, which
 * is precisely the case where you most want the list rather than a claim.
 */
async function revealComposed(sent: Composition, url: string, after: number): Promise<void> {
  await loadSessions();
  const method = methodOf(sent);
  const hit = state.sessions.find((s) => s.id > after && s.url === url && s.method === method);
  showPane('requests');
  if (hit) void selectRow(hit.id);
}

// ── rules ──────────────────────────────────────────────────────────────────

const ruleDrafts = createDrafts<string>();
const valueDrafts = createDrafts<string | null>();
export const rulesEditor = computed(() => ruleDrafts.get(state.group));
export const ruleManagementBusy = ref(false);
export const namedGroups = computed(() => state.groups.filter((g) => g.name !== 'default'));
export const namedGroupIndex = computed(() => namedGroups.value.findIndex((g) => g.name === state.group));
export const valuesEditor = computed(() => valueDrafts.get(state.valueKey));
// Whole-store and per-key saves write the same Values store; do not race them.
export const valuesSaving = computed(() => [...valueDrafts.entries.values()].some((d) => d.saving));
export const ruleDirty = (name: string): boolean => isDirty(ruleDrafts.get(name));
export const valueDirty = (name: string | null): boolean => isDirty(valueDrafts.get(name));
export const hasUnsavedEdits = computed(() =>
  [...ruleDrafts.entries.values(), ...valueDrafts.entries.values()].some((d) => isDirty(d) || d.saving),
);
let rulesLoadVersion = 0;
let valuesLoadVersion = 0;

export async function loadRules(): Promise<void> {
  const version = ++rulesLoadVersion;
  void loadSwitches();
  const groups = await reach(api.ruleGroups);
  if (version !== rulesLoadVersion) return;
  if (!groups) {
    rulesEditor.value.status = 'Could not reach the proxy';
    return;
  }
  state.groups = groups;
  await selectGroup(state.group);
}

export async function selectGroup(name: string): Promise<void> {
  state.group = name;
  await loadDraft(ruleDrafts.get(name), async () => {
    const text = await reach(async () =>
      name === 'default' ? await api.rules() : (await api.ruleGroup(name)).text || '',
    );
    if (text === undefined) throw new Error('Could not reach the proxy');
    return text;
  });
}

export async function saveRules(): Promise<void> {
  if (ruleManagementBusy.value) return;
  const name = state.group;
  const saved = await saveDraft(ruleDrafts.get(name), async (text) => {
    if (name === 'default') {
      const res = await api.saveRules(text);
      if (!res.ok) throw new Error(res.error || 'Save failed');
      return res.rules === undefined ? 'Saved' : `Saved · ${res.rules} rules active`;
    }
    const res = await api.updateRuleGroup(name, text);
    if (!res.ok) throw new Error(res.error || 'Save failed');
    return 'Saved';
  });
  if (saved) {
    const version = ++rulesLoadVersion;
    const groups = await reach(api.ruleGroups);
    if (groups && version === rulesLoadVersion) state.groups = groups;
  }
}

export async function addGroup(): Promise<void> {
  if (ruleManagementBusy.value) return;
  const name = prompt('Group name:');
  if (!name || !name.trim()) return;
  const res = await api.addRuleGroup(name.trim());
  if (!res.ok) {
    alert(res.error || 'Failed');
    return;
  }
  state.group = name.trim();
  await loadRules();
}

export async function toggleGroup(name: string): Promise<void> {
  if (ruleManagementBusy.value) return;
  await api.toggleRuleGroup(name);
  await loadRules();
}

export async function deleteGroup(name: string): Promise<void> {
  if (ruleManagementBusy.value) return;
  if (ruleDrafts.get(name).saving) return;
  if (!confirm(`Delete group "${name}"?`)) return;
  const res = await api.deleteRuleGroup(name);
  if (!res.ok) {
    alert(res.error || 'Failed');
    return;
  }
  if (state.group === name) state.group = 'default';
  ruleDrafts.remove(name);
  await loadRules();
}

export async function renameGroup(name: string): Promise<void> {
  if (name === 'default' || ruleManagementBusy.value || ruleDrafts.get(name).saving) return;
  const to = prompt('Rename group to:', name)?.trim();
  if (!to || to === name) return;
  if (state.groups.some((g) => g.name === to) || (ruleDrafts.entries.has(to) && ruleDirty(to))) {
    alert('A group or unsaved draft by that name already exists');
    return;
  }
  ruleManagementBusy.value = true;
  rulesLoadVersion++;
  try {
    const result = await api.renameRuleGroup(name, to);
    if (!result.ok) throw new Error(result.error || 'Rename failed');
    ruleDrafts.rename(name, to);
    if (state.group === name) state.group = to;
    await loadRules();
  } catch (error) { alert(error instanceof Error ? error.message : 'Rename failed'); }
  finally { ruleManagementBusy.value = false; }
}

export async function moveGroup(direction: -1 | 1): Promise<void> {
  if (ruleManagementBusy.value || rulesEditor.value.saving) return;
  const names = namedGroups.value.map((g) => g.name);
  const from = names.indexOf(state.group);
  const to = from + direction;
  if (from < 0 || to < 0 || to >= names.length) return;
  [names[from], names[to]] = [names[to], names[from]];
  ruleManagementBusy.value = true;
  rulesLoadVersion++;
  try {
    const result = await api.reorderRuleGroups(names);
    if (!result.ok) throw new Error(result.error || 'Reorder failed; reload the group list');
    await loadRules();
  } catch (error) { alert(error instanceof Error ? error.message : 'Reorder failed'); }
  finally { ruleManagementBusy.value = false; }
}

// ── values ─────────────────────────────────────────────────────────────────

export async function loadValues(): Promise<void> {
  if (valuesSaving.value) return;
  const version = ++valuesLoadVersion;
  const values = await reach(api.values);
  if (version !== valuesLoadVersion) return;
  if (!values) {
    valuesEditor.value.status = 'Could not reach the proxy';
    return;
  }
  state.values = values;
  receiveDraft(valueDrafts.get(null), JSON.stringify(values, null, 2));
  for (const [name, text] of Object.entries(values)) receiveDraft(valueDrafts.get(name), text);
  // A remote deletion must not hide a local unsaved draft.
  for (const [name, draft] of valueDrafts.entries) {
    if (name !== null && !(name in values) && !isDirty(draft)) valueDrafts.remove(name);
  }
  if (state.valueKey !== null && !(state.valueKey in values) && !valueDirty(state.valueKey)) {
    state.valueKey = null;
  }
}

export const valueNames = computed(() => {
  const names = new Set(Object.keys(state.values));
  for (const [name, draft] of valueDrafts.entries) {
    if (name !== null && isDirty(draft)) names.add(name);
  }
  return [...names].sort();
});

/** Edit one value, or `null` for the whole store as one JSON object. */
export function selectValue(name: string | null): void {
  state.valueKey = name;
}

/** Save whichever of the two the Values pane is showing. */
export async function saveValue(): Promise<void> {
  await persistValue(state.valueKey);
}

export async function saveValues(): Promise<void> {
  await persistValue(null);
}

async function persistValue(name: string | null): Promise<void> {
  if (valuesSaving.value) return;
  valuesLoadVersion++;
  const saved = await saveDraft(valueDrafts.get(name), async (text) => {
    if (name === null) {
      try {
        JSON.parse(text);
      } catch (e) {
        throw new Error('Invalid JSON: ' + (e as Error).message);
      }
    }
    const res = await reach(() => name === null ? api.saveValues(text) : api.setValue(name, text));
    if (!res?.ok) throw new Error(res?.error || 'Save failed');
    return 'Saved';
  });
  if (saved) await loadValues();
}

export async function addValue(): Promise<void> {
  const name = prompt('Value name:');
  if (!name || !name.trim()) return;
  const res = await api.setValue(name.trim(), '');
  if (!res.ok) {
    alert(res.error || 'Failed');
    return;
  }
  await loadValues();
  selectValue(name.trim());
}

export async function renameValue(name: string): Promise<void> {
  if (valuesSaving.value) return;
  const to = prompt('Rename value to:', name);
  if (!to || !to.trim() || to.trim() === name) return;
  const res = await api.renameValue(name, to.trim());
  if (!res.ok) {
    alert(res.error || 'Failed');
    return;
  }
  valueDrafts.rename(name, to.trim());
  if (state.valueKey === name) state.valueKey = to.trim();
  await loadValues();
}

export async function deleteValue(name: string): Promise<void> {
  if (valuesSaving.value) return;
  if (!confirm(`Delete value "${name}"?`)) return;
  const res = await api.deleteValue(name);
  if (!res.ok) {
    alert(res.error || 'Failed');
    return;
  }
  valueDrafts.remove(name);
  if (state.valueKey === name) selectValue(null);
  await loadValues();
}

// ── import / export ────────────────────────────────────────────────────────

/** Hand `text` to the browser as a file, without leaving the page. */
export function downloadText(name: string, text: string, type = 'text/plain'): void {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  a.click();
  URL.revokeObjectURL(url);
}

/** The selected group, as the rules file it is — the editor's text, as shown. */
export function exportGroup(): void {
  downloadText(`${state.group}.rules`, rulesEditor.value.text);
}

/** The selected value, or the whole store as the JSON object it is kept as. */
export function exportValues(): void {
  if (state.valueKey === null) downloadText('values.json', valuesEditor.value.text, 'application/json');
  else downloadText(state.valueKey, valuesEditor.value.text);
}

export const pendingImport = shallowRef<{ filename: string; input: ImportInput } | null>(null);

/** Capture the destination before reading the file; switching panes must not
 * turn a rules export into Values (or vice versa). No writes before review. */
export async function importFile(file: File): Promise<void> {
  const pane = state.pane === 'rules' ? 'rules' : 'values';
  try {
    const text = await file.text();
    if (ruleManagementBusy.value || valuesSaving.value || [...ruleDrafts.entries.values()].some((d) => d.saving)) {
      throw new Error('Wait for pending saves before importing');
    }
    pendingImport.value = { filename: file.name, input: classifyImport(text, file.name, pane) };
  } catch (error) { alert(error instanceof Error ? error.message : 'Could not read the import file'); }
}

export async function finishImport(groups: number, values: number): Promise<void> {
  pendingImport.value = null;
  await Promise.all([loadRules(), loadValues()]);
  const note = `Imported ${groups} groups and ${values} values; unsaved drafts were kept`;
  rulesEditor.value.status = note;
  valuesEditor.value.status = note;
}

// ── status ─────────────────────────────────────────────────────────────────

export async function loadStatus(): Promise<void> {
  void loadSwitches();
  const status = await reach(api.status);
  if (status) state.status = status;
}

// ── switches: HTTPS, every rule, the plugins ───────────────────────────────

export async function loadSwitches(): Promise<void> {
  const switches = await reach(api.switches);
  if (switches) state.switches = switches;
}

/** Set some switches; a refusal is said, and nothing is changed. */
export async function setSwitches(patch: SwitchPatch): Promise<void> {
  const res = await reach(() => api.setSwitches(patch));
  if (!res) return;
  if (!res.ok) alert(res.error || 'Refused');
  await loadSwitches();
  // The status card's "Intercept HTTPS" row reads the same switch.
  if ('intercept_https' in patch) void loadStatus();
}

export async function togglePlugin(name: string): Promise<void> {
  const on = !(state.switches?.plugins_off ?? []).includes(name);
  const res = await reach(() => api.switchPlugin(name, !on));
  if (!res) return;
  if (!res.ok) alert(res.error || 'Refused');
  await loadSwitches();
}

// ── panes ──────────────────────────────────────────────────────────────────

// ── the Console pane: what pages wrote to their consoles ───────────────────

/** How many entries the pane keeps. The proxy keeps 2000; so does this. */
const PAGE_LOGS_MAX = 2000;

/**
 * Fetch what arrived since the last call and append it.
 *
 * Incremental on purpose: the pane polls, and re-fetching two thousand entries
 * every two seconds to show the three that are new would make the console the
 * busiest client the proxy has.
 */
export async function loadPageLogs(): Promise<void> {
  const answer = await reach(() => api.logs(state.pageLogLast));
  if (!answer) return;
  // The proxy restarted: its numbering began again below what this has seen.
  if (answer.last < state.pageLogLast) {
    state.pageLogs = [];
    state.pageLogLast = 0;
    return void loadPageLogs();
  }
  state.pageLogIds = answer.ids;
  state.pageLogLast = answer.last;
  if (answer.logs.length) {
    const all = state.pageLogs.concat(answer.logs);
    state.pageLogs = all.length > PAGE_LOGS_MAX ? all.slice(all.length - PAGE_LOGS_MAX) : all;
  }
  if (state.pageLogId !== null && !answer.ids.includes(state.pageLogId)) state.pageLogId = null;
}

/** Forget the group being shown, or everything when all groups are. */
export async function clearPageLogs(): Promise<void> {
  const id = state.pageLogId;
  if (!(await reach(() => api.clearLogs(id === null ? undefined : id)))) return;
  state.pageLogs = id === null ? [] : state.pageLogs.filter((log) => log.id !== id);
  await loadPageLogs();
}

export function showPane(name: Pane): void {
  state.pane = name;
  if (name === 'console') void loadPageLogs();
  if (name === 'rules') void loadRules();
  if (name === 'values') void loadValues();
  if (name === 'status') void loadStatus();
}

/** ⌘S saves whichever pane is showing, and nothing else has one. */
export function saveCurrentPane(): void {
  if (pendingImport.value) return;
  if (state.pane === 'rules') void saveRules();
  else if (state.pane === 'values') void saveValue();
}
