import { expect, test } from '@playwright/test';
import type { Page, Route } from '@playwright/test';

const editor = (page: Page) => page.locator('.pane:visible .cm-content');
const save = (page: Page) => page.locator('.pane:visible').getByRole('button', { name: 'Save', exact: true });
const side = (page: Page, name: string) => page.locator('.side-item').filter({
  has: page.locator('.side-name').getByText(name, { exact: true }),
});

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => { resolve = done; });
  return { promise, resolve };
}

/** Each browser gets its own server state; no shared Vite mock mutations. */
async function fixtures(page: Page) {
  const groups = new Map([
    ['default', 'default.test host://127.0.0.1'],
    ['staging', 'staging.test host://127.0.0.2'],
    ['archive', 'archive.test host://127.0.0.3'],
  ]);
  let values: Record<string, string> = { alpha: 'alpha saved', beta: 'beta saved' };
  const writes: { path: string; body: string }[] = [];
  const next = new Map<string, (route: Route, json: unknown) => Promise<void>>();
  const failures = new Map<string, number | 'network'>();

  await page.route('**/api/**', async (route) => {
    const request = route.request();
    const { pathname, search } = new URL(request.url());
    const path = pathname + search;
    const method = request.method();
    const hook = next.get(`${method} ${path}`);
    next.delete(`${method} ${path}`);
    const body = request.postData() || '';
    if (method === 'POST' || method === 'DELETE') writes.push({ path, body });
    const failure = failures.get(`${method} ${path}`);
    failures.delete(`${method} ${path}`);
    if (failure === 'network') return route.abort('failed');
    if (failure !== undefined) return route.fulfill({ status: failure, json: { ok: false, error: 'write refused' } });
    let json: unknown;
    let text: string | undefined;
    if (pathname === '/api/rules') {
      if (method === 'POST') { groups.set('default', body); json = { ok: true, rules: 1 }; }
      else text = groups.get('default')!;
    } else if (pathname === '/api/rule-groups') {
      json = [...groups.keys()].map((name) => ({ name, enabled: true, rules: 1 }));
    } else if (pathname === '/api/rule-group' && method === 'GET') {
      const name = new URL(request.url()).searchParams.get('name')!;
      json = { name, text: groups.get(name), enabled: true, rules: 1 };
    } else if (pathname === '/api/rule-group/update') {
      const data = JSON.parse(body);
      groups.set(data.name, data.text);
      json = { ok: true };
    } else if (pathname === '/api/rule-group/toggle') {
      json = { ok: true, enabled: false };
    } else if (pathname === '/api/values') {
      if (method === 'POST') values = JSON.parse(body);
      json = method === 'POST' ? { ok: true } : { ...values };
    } else if (pathname === '/api/value/rename') {
      const data = JSON.parse(body);
      values[data.to] = values[data.name];
      delete values[data.name];
      json = { ok: true };
    } else if (pathname === '/api/value' && method === 'DELETE') {
      delete values[JSON.parse(body).name];
      json = { ok: true };
    } else if (pathname === '/api/value' && method === 'POST') {
      const data = JSON.parse(body);
      values[data.name] = data.value;
      json = { ok: true };
    } else {
      return route.fallback();
    }
    if (hook) return hook(route, text ?? json);
    if (text !== undefined) await route.fulfill({ contentType: 'text/plain', body: text });
    else await route.fulfill({ json });
  });

  return {
    groups,
    writes,
    values: () => values,
    fail(path: string, status: number | 'network' = 200, method = 'POST') {
      failures.set(`${method} ${path}`, status);
    },
    hold(path: string, method = 'GET') {
      const entered = deferred();
      const released = deferred();
      next.set(`${method} ${path}`, async (route, answer) => {
        entered.resolve();
        await released.promise;
        await route.fulfill(typeof answer === 'string'
          ? { contentType: 'text/plain', body: answer } : { json: answer });
      });
      return {
        entered: entered.promise,
        async release() {
          const received = page.waitForResponse((res) => new URL(res.url()).pathname + new URL(res.url()).search === path
            && res.request().method() === method);
          released.resolve();
          await (await received).finished();
          // Let the response's async continuation and Vue's DOM update finish.
          await page.evaluate(() => new Promise<void>((done) => requestAnimationFrame(() => done())));
        },
      };
    },
  };
}

async function open(page: Page, pane: 'Rules' | 'Values') {
  await page.goto('/');
  await page.getByRole('tab', { name: pane, exact: true }).click();
  await expect(editor(page)).toContainText(pane === 'Rules' ? 'default.test' : 'alpha saved');
}

test('rule drafts survive group switches, pane refresh and a group toggle', async ({ page }) => {
  const data = await fixtures(page);
  await open(page, 'Rules');
  await editor(page).fill('default.test resBody://draft');
  await side(page, 'staging').click();
  await expect(editor(page)).toContainText('staging.test');
  await editor(page).fill('staging.test resBody://draft');
  await side(page, 'Default').click();
  await expect(editor(page)).toHaveText('default.test resBody://draft');
  await page.getByRole('tab', { name: 'Requests', exact: true }).click();
  await page.getByRole('tab', { name: 'Rules', exact: true }).click();
  await expect(editor(page)).toHaveText('default.test resBody://draft');
  await side(page, 'staging').dblclick();
  await expect(editor(page)).toHaveText('staging.test resBody://draft');
  expect(data.writes.filter((w) => w.path !== '/api/rule-group/toggle')).toEqual([]);
});

test('a late group response never becomes another group’s saved content', async ({ page }) => {
  const data = await fixtures(page);
  await open(page, 'Rules');
  const slow = data.hold('/api/rule-group?name=staging');
  await side(page, 'staging').click();
  await slow.entered;
  await side(page, 'archive').click();
  await expect(editor(page)).toContainText('archive.test');
  await slow.release();
  await expect(editor(page)).toHaveText('archive.test host://127.0.0.3');
  await save(page).click();
  await expect.poll(() => data.writes.at(-1)?.body).toBe(JSON.stringify({
    name: 'archive', text: 'archive.test host://127.0.0.3',
  }));
});

test('named value and whole-store drafts survive switching and pane reloads', async ({ page }) => {
  await fixtures(page);
  await open(page, 'Values');
  const all = '{"alpha":"all draft","beta":"beta saved"}';
  await editor(page).fill(all);
  await side(page, 'alpha').click();
  await editor(page).fill('alpha draft');
  await side(page, 'beta').click();
  await editor(page).fill('beta draft');
  await side(page, 'alpha').click();
  await expect(editor(page)).toHaveText('alpha draft');
  await side(page, 'All values').click();
  await expect(editor(page)).toHaveText(all);
  await page.getByRole('tab', { name: 'Requests', exact: true }).click();
  await page.getByRole('tab', { name: 'Values', exact: true }).click();
  await expect(editor(page)).toHaveText(all);
  await side(page, 'beta').click();
  await expect(editor(page)).toHaveText('beta draft');
});

test('a late values refresh cannot erase edits to the newly selected value', async ({ page }) => {
  const data = await fixtures(page);
  await open(page, 'Values');
  await page.getByRole('tab', { name: 'Requests', exact: true }).click();
  const slow = data.hold('/api/values');
  await page.getByRole('tab', { name: 'Values', exact: true }).click();
  await slow.entered;
  await side(page, 'beta').click();
  await editor(page).fill('beta newest draft');
  await slow.release();
  await expect(editor(page)).toHaveText('beta newest draft');
  await save(page).click();
  await expect.poll(() => data.values().beta).toBe('beta newest draft');
});

const targets = [
  { name: 'Default', pane: 'Rules', path: '/api/rules', other: 'staging',
    text: 'default.test resBody://submitted', newer: 'default.test resBody://newest', otherText: 'staging.test resBody://draft' },
  { name: 'staging', pane: 'Rules', path: '/api/rule-group/update', other: 'Default',
    text: 'staging.test resBody://submitted', newer: 'staging.test resBody://newest', otherText: 'default.test resBody://draft' },
  { name: 'alpha', pane: 'Values', path: '/api/value', other: 'beta',
    text: 'alpha submitted', newer: 'alpha newest', otherText: 'beta draft' },
  { name: 'All values', pane: 'Values', path: '/api/values', other: 'alpha',
    text: '{"alpha":"submitted","beta":"beta saved"}', newer: '{"alpha":"newest","beta":"beta saved"}', otherText: 'alpha draft' },
] as const;

for (const target of targets) {
  test(`${target.pane}/${target.name}: saving confirms only the submitted snapshot and ignores duplicate saves`, async ({ page }) => {
    const data = await fixtures(page);
    await open(page, target.pane);
    await side(page, target.name).click();
    await expect(editor(page)).toBeEditable();
    await editor(page).fill(target.text);
    const pending = data.hold(target.path, 'POST');
    await save(page).click();
    await pending.entered;
    await expect(save(page)).toBeDisabled();
    await editor(page).press('ControlOrMeta+s');
    await editor(page).press('ControlOrMeta+s');
    await editor(page).fill(target.newer);
    await side(page, target.other).click();
    await expect(editor(page)).toBeEditable();
    await editor(page).fill(target.otherText);
    await pending.release();
    expect(data.writes).toHaveLength(1);
    await expect(editor(page)).toHaveText(target.otherText);
    await expect(page.locator('.pane:visible .status').filter({ hasText: /^Saved/ })).toHaveCount(0);
    await side(page, target.name).click();
    await expect(editor(page)).toHaveText(target.newer);
    await expect(page.locator('.pane:visible').getByText('Unsaved changes', { exact: true })).toBeVisible();
    await save(page).click();
    await expect(page.locator('.pane:visible').getByText('Unsaved changes', { exact: true })).toHaveCount(0);
    await expect.poll(() => data.writes.length).toBe(2);
    const expected = target.path === '/api/rule-group/update'
      ? JSON.stringify({ name: target.name, text: target.newer })
      : target.path === '/api/value' ? JSON.stringify({ name: target.name, value: target.newer }) : target.newer;
    expect(data.writes.at(-1)?.body).toBe(expected);
    await side(page, target.other).click();
    await expect(editor(page)).toHaveText(target.otherText);
    await expect(page.locator('.pane:visible').getByText('Unsaved changes', { exact: true })).toBeVisible();
  });

  test(`${target.pane}/${target.name}: a refused save keeps the draft and can be retried`, async ({ page }) => {
    const data = await fixtures(page);
    await open(page, target.pane);
    await side(page, target.name).click();
    await expect(editor(page)).toBeEditable();
    await editor(page).fill(target.text);
    data.fail(target.path);
    await save(page).click();
    await expect(page.locator('.pane:visible').getByText('write refused', { exact: true })).toBeVisible();
    await expect(editor(page)).toHaveText(target.text);
    await side(page, target.other).click();
    await side(page, target.name).click();
    await expect(editor(page)).toHaveText(target.text);
    await expect(page.locator('.pane:visible').getByText('Unsaved changes', { exact: true })).toBeVisible();
    await save(page).click();
    await expect(page.locator('.pane:visible').getByText('Unsaved changes', { exact: true })).toHaveCount(0);
    await expect.poll(() => data.writes.length).toBe(2);
  });
}

test('an unloaded or failed group is read-only and cannot save the previous group’s text', async ({ page }) => {
  const data = await fixtures(page);
  await open(page, 'Rules');
  const pending = data.hold('/api/rule-group?name=staging');
  await side(page, 'staging').click();
  await pending.entered;
  await expect(editor(page)).not.toContainText('default.test');
  await expect(editor(page)).not.toBeEditable();
  await expect(save(page)).toBeDisabled();
  await editor(page).press('ControlOrMeta+s');
  expect(data.writes).toEqual([]);
  await pending.release();
  await expect(editor(page)).toContainText('staging.test');

  data.fail('/api/rule-group?name=archive', 503, 'GET');
  await side(page, 'archive').click();
  await expect(page.getByText('Could not reach the proxy', { exact: true })).toBeVisible();
  await expect(save(page)).toBeDisabled();
  await side(page, 'archive').click();
  await expect(editor(page)).toContainText('archive.test');
  await expect(save(page)).toBeEnabled();
});

test('A → B → A discards the older response for A, even when the new draft is clean', async ({ page }) => {
  const data = await fixtures(page);
  await open(page, 'Rules');
  const first = data.hold('/api/rule-group?name=staging');
  await side(page, 'staging').click();
  await first.entered;
  await side(page, 'archive').click();
  data.groups.set('staging', 'staging.test resBody://latest-server');
  await side(page, 'staging').click();
  await expect(editor(page)).toHaveText('staging.test resBody://latest-server');
  await first.release();
  await expect(editor(page)).toHaveText('staging.test resBody://latest-server');
});

test('a read started before a save cannot revert that save', async ({ page }) => {
  const data = await fixtures(page);
  await open(page, 'Rules');
  await side(page, 'staging').click();
  await expect(editor(page)).toContainText('staging.test');
  const pending = data.hold('/api/rule-group?name=staging');
  await side(page, 'staging').click();
  await pending.entered;
  await editor(page).fill('staging.test resBody://saved-new');
  await save(page).click();
  await expect(page.locator('.pane:visible').getByText('Saved', { exact: true })).toBeVisible();
  await pending.release();
  await expect(editor(page)).toHaveText('staging.test resBody://saved-new');
  await expect(page.locator('.pane:visible').getByText('Unsaved changes', { exact: true })).toHaveCount(0);
});

test('a network failure keeps a value draft; malformed whole-store JSON is never submitted', async ({ page }) => {
  const data = await fixtures(page);
  await open(page, 'Values');
  await side(page, 'alpha').click();
  await editor(page).fill('recoverable');
  data.fail('/api/value', 'network');
  await save(page).click();
  await expect(save(page)).toBeEnabled();
  await expect(editor(page)).toHaveText('recoverable');
  await side(page, 'All values').click();
  await editor(page).fill('{');
  await save(page).click();
  await expect(page.locator('.pane:visible .status').filter({ hasText: 'Invalid JSON:' })).toBeVisible();
  expect(data.writes.filter((w) => w.path === '/api/values')).toEqual([]);
  await side(page, 'alpha').click();
  await expect(editor(page)).toHaveText('recoverable');
});

test('renaming moves the unsaved value draft; deleting it leaves other drafts intact', async ({ page }) => {
  await fixtures(page);
  await open(page, 'Values');
  await side(page, 'beta').click();
  await editor(page).fill('keep beta');
  await side(page, 'alpha').click();
  await editor(page).fill('rename me unsaved');
  page.once('dialog', (dialog) => dialog.accept('renamed'));
  await page.getByRole('button', { name: 'Rename', exact: true }).click();
  await expect(side(page, 'renamed')).toHaveAttribute('aria-selected', 'true');
  await expect(editor(page)).toHaveText('rename me unsaved');
  await side(page, 'beta').click();
  await side(page, 'renamed').click();
  await expect(editor(page)).toHaveText('rename me unsaved');
  page.once('dialog', (dialog) => dialog.accept());
  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  await expect(side(page, 'renamed')).toHaveCount(0);
  await side(page, 'beta').click();
  await expect(editor(page)).toHaveText('keep beta');
});

test('closing warns about dirty drafts but a successful save removes the warning', async ({ page }) => {
  await fixtures(page);
  await open(page, 'Rules');
  await editor(page).fill('example.test resBody://draft');
  const warned = page.waitForEvent('dialog');
  const closing = page.close({ runBeforeUnload: true });
  const dialog = await warned;
  expect(dialog.type()).toBe('beforeunload');
  await dialog.dismiss();
  await closing;
  expect(page.isClosed()).toBe(false);
  await expect(editor(page)).toHaveText('example.test resBody://draft');
  await save(page).click();
  await expect(page.locator('.pane:visible').getByText('Unsaved changes', { exact: true })).toHaveCount(0);
  const dialogs: string[] = [];
  page.on('dialog', async (next) => { dialogs.push(next.type()); await next.dismiss(); });
  await page.close({ runBeforeUnload: true });
  await expect.poll(() => page.isClosed()).toBe(true);
  expect(dialogs).toEqual([]);
});

test('empty rule and value drafts are kept rather than replaced with server content', async ({ page }) => {
  const data = await fixtures(page);
  await open(page, 'Rules');
  await editor(page).fill('');
  await side(page, 'staging').click();
  await side(page, 'Default').click();
  await expect(editor(page)).not.toContainText('default.test');
  await save(page).click();
  await expect.poll(() => data.groups.get('default')).toBe('');
  await page.getByRole('tab', { name: 'Values', exact: true }).click();
  await side(page, 'alpha').click();
  await editor(page).fill('');
  await side(page, 'beta').click();
  await side(page, 'alpha').click();
  await expect(editor(page)).not.toContainText('alpha saved');
  await save(page).click();
  await expect.poll(() => data.values().alpha).toBe('');
});

test('an older values snapshot cannot revert a successful value save', async ({ page }) => {
  const data = await fixtures(page);
  await open(page, 'Values');
  await side(page, 'alpha').click();
  await page.getByRole('tab', { name: 'Requests', exact: true }).click();
  const pending = data.hold('/api/values');
  await page.getByRole('tab', { name: 'Values', exact: true }).click();
  await pending.entered;
  await editor(page).fill('new saved value');
  await save(page).click();
  await expect(page.locator('.pane:visible').getByText('Unsaved changes', { exact: true })).toHaveCount(0);
  await pending.release();
  await expect(editor(page)).toHaveText('new saved value');
  await side(page, 'All values').click();
  await expect(editor(page)).toContainText('new saved value');
});

test('remote deletion keeps a dirty value reachable until it is saved or explicitly deleted', async ({ page }) => {
  const data = await fixtures(page);
  await open(page, 'Values');
  await side(page, 'alpha').click();
  await editor(page).fill('recover alpha');
  delete data.values().alpha;
  await page.getByRole('tab', { name: 'Requests', exact: true }).click();
  await page.getByRole('tab', { name: 'Values', exact: true }).click();
  await expect(editor(page)).toHaveText('recover alpha');
  await side(page, 'beta').click();
  await side(page, 'alpha').click();
  await expect(editor(page)).toHaveText('recover alpha');
  await save(page).click();
  await expect.poll(() => data.values().alpha).toBe('recover alpha');
});
