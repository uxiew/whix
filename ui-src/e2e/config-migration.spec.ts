import { expect, request as http, test } from '@playwright/test';
import type { APIRequestContext, Page } from '@playwright/test';

const editor = (page: Page) => page.locator('.pane:visible .cm-content');
const side = (page: Page, name: string) => page.locator('.side-item').filter({ has: page.locator('.side-name').getByText(name, { exact: true }) });

async function post(request: APIRequestContext, path: string, data: unknown) {
  const result = await request.post(path, { data });
  expect(result.ok()).toBe(true);
  expect((await result.json()).ok).toBe(true);
}

async function choose(page: Page, pane: 'Rules' | 'Values', data: unknown, name = 'whistle.txt') {
  await page.getByRole('tab', { name: pane, exact: true }).click();
  await page.locator('.sidebar input[type=file]').setInputFiles({ name, mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(data)) });
  await expect(page.getByRole('dialog')).toBeVisible();
}

test.beforeEach(async ({ request, page }) => {
  const groups = await (await request.get('/api/rule-groups')).json();
  for (const group of groups) {
    if (group.name !== 'default') expect((await request.delete('/api/rule-group', { data: { name: group.name } })).ok()).toBe(true);
  }
  await post(request, '/api/rules', 'rank.test statusCode://203 resBody://fallback');
  await post(request, '/api/values', { keep: 'untouched' });
  await page.goto('/');
});

test('Whistle exports require review, preserve explicit order and exclude metadata from Values', async ({ page, request }) => {
  await choose(page, 'Values', { reply: 'value payload', whix: 'not a bundle', '': { list: ['reply', 'whix'] } });
  await expect(page.getByRole('dialog').getByText('reply — New', { exact: true })).toBeVisible();
  expect((await (await request.get('/api/values')).json()).reply).toBeUndefined();
  await page.getByRole('button', { name: 'Apply import' }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  expect(await (await request.get('/api/values')).json()).toEqual({ keep: 'untouched', reply: 'value payload', whix: 'not a bundle' });

  await choose(page, 'Rules', {
    Default: 'rank.test statusCode://203 resBody://fallback',
    '2': 'rank.test statusCode://202 resBody://second',
    '10': 'rank.test statusCode://201 resBody://{reply}',
    '\rFolder': '', '': ['Default', '10', '\rFolder', '2'],
  });
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByLabel('Order after').locator('li')).toHaveText(['default', '10', '2']);
  await expect(dialog.getByText('10 — New · disabled', { exact: true })).toBeVisible();
  await dialog.getByRole('checkbox', { name: 'Enable newly imported groups' }).check();
  await expect(dialog.getByText('10 — New · enabled', { exact: true })).toBeVisible();
  await page.screenshot({ path: test.info().outputPath('import-preview.png') });
  await dialog.getByRole('button', { name: 'Apply import' }).click();
  await expect(dialog).toHaveCount(0);
  await expect(side(page, '10')).toBeVisible();
  expect((await (await request.get('/api/rule-groups')).json()).map((g: { name: string }) => g.name)).toEqual(['default', '10', '2']);
  await side(page, '10').click();
  await expect(editor(page)).toContainText('resBody://{reply}');
  const proxy = await http.newContext({ proxy: { server: 'http://127.0.0.1:25198' } });
  try {
    const response = await proxy.get('http://rank.test/');
    expect(response.status()).toBe(201);
    expect(await response.text()).toBe('value payload\r\nsecond\r\nfallback');
  } finally { await proxy.dispose(); }
});

test('merge preview shows overlapping order changes and Cancel performs no writes', async ({ page, request }) => {
  for (const name of ['keep', 'B', 'middle', 'A']) await post(request, '/api/rule-groups', { name, text: `# ${name}`, enabled: true });
  const before = await (await request.get('/api/export')).json();
  const source = { A: '# new A', B: '# new B', '': ['A', 'B'] };
  await choose(page, 'Rules', source);
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText('A — Replace saved text · enabled', { exact: true })).toBeVisible();
  await expect(dialog.getByLabel('Order before').locator('li')).toHaveText(['default', 'keep', 'B', 'middle', 'A']);
  await expect(dialog.getByLabel('Order after').locator('li')).toHaveText(['default', 'keep', 'A', 'B', 'middle']);
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  expect(await (await request.get('/api/export')).json()).toEqual(before);
  await choose(page, 'Rules', source);
  await page.getByRole('button', { name: 'Apply import' }).click();
  await expect(dialog).toHaveCount(0);
  const after = await (await request.get('/api/export')).json();
  expect(after.rules.map((g: { name: string }) => g.name)).toEqual(['default', 'keep', 'A', 'B', 'middle']);
  expect(after.rules.find((g: { name: string }) => g.name === 'keep').text).toBe('# keep');
});

test('renaming and sorting keep an unsaved draft attached to its group', async ({ page, request }) => {
  await post(request, '/api/rule-groups', { name: 'alpha', text: '# saved alpha', enabled: true });
  await post(request, '/api/rule-groups', { name: 'beta', text: '# saved beta', enabled: true });
  await page.getByRole('tab', { name: 'Rules', exact: true }).click();
  await side(page, 'alpha').click();
  await expect(editor(page)).toHaveText('# saved alpha');
  await editor(page).fill('# unsaved alpha');
  page.once('dialog', (dialog) => dialog.accept('中文 / A+%'));
  await page.getByRole('button', { name: 'Rename', exact: true }).click();
  await expect(side(page, '中文 / A+%')).toHaveAttribute('aria-selected', 'true');
  await expect(editor(page)).toHaveText('# unsaved alpha');
  await page.getByRole('button', { name: 'Move down', exact: true }).click();
  await expect(page.locator('.sidebar .side-name')).toHaveText(['Default', 'beta', '中文 / A+%']);
  await expect(page.getByRole('button', { name: 'Move down', exact: true })).toBeDisabled();
  await expect(editor(page)).toHaveText('# unsaved alpha');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText('Unsaved changes', { exact: true })).toHaveCount(0);
  expect((await (await request.get(`/api/rule-group?name=${encodeURIComponent('中文 / A+%')}`)).json()).text).toBe('# unsaved alpha');
  await side(page, 'Default').click();
  await expect(page.getByRole('button', { name: 'Move up' })).toHaveCount(0);
});

test('a stale preview cannot overwrite an intervening save; malformed exports cannot apply', async ({ page, request }) => {
  await choose(page, 'Rules', { A: '# from import', '': ['A'] });
  await expect(page.getByRole('button', { name: 'Apply import' })).toBeEnabled();
  await post(request, '/api/value', { name: 'changed', value: 'after preview' });
  await page.getByRole('button', { name: 'Apply import' }).click();
  await expect(page.getByRole('alert')).toContainText('configuration changed');
  await expect(page.getByRole('button', { name: 'Apply import' })).toBeDisabled();
  await page.getByRole('button', { name: 'Refresh preview' }).click();
  await expect(page.getByRole('button', { name: 'Apply import' })).toBeEnabled();
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await choose(page, 'Rules', { A: { bad: 'not text' }, '': ['A'] });
  await expect(page.getByRole('alert')).toContainText('must contain text');
  await expect(page.getByRole('button', { name: 'Apply import' })).toBeDisabled();
  expect((await (await request.get('/api/rule-groups')).json()).map((g: { name: string }) => g.name)).toEqual(['default']);
});

test('a delayed preview cannot undo the newly reviewed activation choice', async ({ page, request }) => {
  let release!: () => void;
  let entered!: () => void;
  const held = new Promise<void>((resolve) => { release = resolve; });
  const started = new Promise<void>((resolve) => { entered = resolve; });
  let first = true;
  await page.route('**/api/import/preview', async (route) => {
    const response = await route.fetch();
    if (first) {
      first = false;
      entered();
      await held;
    }
    await route.fulfill({ response });
  });
  await choose(page, 'Rules', { A: '# imported', '': ['A'] });
  await started;
  await page.getByRole('checkbox', { name: 'Enable newly imported groups' }).check();
  await expect(page.getByText('A — New · enabled', { exact: true })).toBeVisible();
  const received = page.waitForResponse((r) => r.url().endsWith('/api/import/preview') && r.request().postDataJSON().enable_new === false);
  release();
  await (await received).finished();
  await page.evaluate(() => new Promise<void>((done) => requestAnimationFrame(() => done())));
  await expect(page.getByText('A — New · enabled', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Apply import' }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  expect((await (await request.get('/api/rule-groups')).json()).find((g: { name: string }) => g.name === 'A').enabled).toBe(true);
});

test('importing saved content never discards the corresponding unsaved editor draft', async ({ page, request }) => {
  await post(request, '/api/rule-groups', { name: 'alpha', text: '# original', enabled: true });
  await page.getByRole('tab', { name: 'Rules', exact: true }).click();
  await side(page, 'alpha').click();
  await expect(editor(page)).toHaveText('# original');
  await editor(page).fill('# my unsaved draft');
  await choose(page, 'Rules', { alpha: '# imported saved content', '': ['alpha'] });
  await expect(page.getByRole('dialog').getByText('Unsaved editor drafts will be kept, not saved or discarded by this import.')).toBeVisible();
  await page.getByRole('button', { name: 'Apply import' }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(editor(page)).toHaveText('# my unsaved draft');
  await expect(page.getByText('Unsaved changes', { exact: true })).toBeVisible();
  expect((await (await request.get('/api/rule-group?name=alpha')).json()).text).toBe('# imported saved content');
});
