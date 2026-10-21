import { expect, test } from '@playwright/test';
import type { APIRequestContext, Page } from '@playwright/test';

const origin = 'http://127.0.0.1:25197';
const bytes = Buffer.from([0, 255, 128, 13, 10]);
const base64 = bytes.toString('base64');
const editor = (page: Page) => page.locator('.pane:visible .cm-content');
const bodyPicker = (page: Page) => page.locator('.compose input[type=file]');
async function post(request: APIRequestContext, path: string, data: unknown = {}) {
  const response = await request.post(path, { data });
  expect(response.ok()).toBe(true);
  return response.json();
}
async function peers(request: APIRequestContext) {
  return (await request.get(origin + '/state')).json();
}
function har(partial = false) {
  return { log: { version: '1.2', entries: [{
    startedDateTime: '2026-10-19T12:00:00.250+09:00', time: 1,
    request: { method: 'POST', url: 'http://traffic.test/upload-har', headers: [{ name: ':authority', value: 'traffic.test' }, { name: ':method', value: 'POST' }], bodySize: partial ? 100 : bytes.length,
      postData: { mimeType: 'application/octet-stream', text: 'Whistle display preview', base64 } },
    response: { status: 200, headers: [], content: { size: 2, text: 'ok' } },
  }] } };
}
async function chooseHar(page: Page, source: unknown, accept: boolean) {
  page.once('dialog', (d) => accept ? d.accept() : d.dismiss());
  await page.getByLabel('HAR file').setInputFiles({ name: 'fixture.har', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(source)) });
}
test.beforeEach(async ({ request, page }) => {
  await post(request, origin + '/reset');
  await post(request, '/api/sessions/clear');
  await post(request, '/api/rules', 'traffic.test host://127.0.0.1:25197');
  await page.goto('/');
});

test('Composer raw multipart and Edit & Resend preserve bytes and boundary; reload cannot silently send an empty body', async ({ page, request }) => {
  const raw = Buffer.concat([Buffer.from('--fixture\r\nContent-Disposition: form-data; name="file"; filename="x.bin"\r\nContent-Type: application/octet-stream\r\n\r\n'), bytes, Buffer.from('\r\n--fixture--\r\n')]);
  await page.getByRole('tab', { name: 'Composer', exact: true }).click();
  await page.getByLabel('Method', { exact: true }).fill('POST');
  await page.getByLabel('URL', { exact: true }).fill('http://traffic.test/upload');
  await page.getByLabel('Headers', { exact: true }).fill('Content-Type: multipart/form-data; boundary=fixture\nContent-Length: 1');
  await bodyPicker(page).setInputFiles({ name: 'raw-multipart.bin', mimeType: 'application/octet-stream', buffer: raw });
  await expect(page.getByLabel('Body encoding')).toHaveValue('base64');
  await expect(editor(page)).toHaveText(raw.toString('base64'));
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  await expect.poll(async () => (await peers(request)).uploads.length).toBe(1);
  let received = (await peers(request)).uploads[0];
  expect(received.base64).toBe(raw.toString('base64'));
  expect(received.headers['content-type']).toBe('multipart/form-data; boundary=fixture');
  expect(received.headers['content-length']).toBe(String(raw.length));
  await page.getByRole('button', { name: 'Edit & Resend', exact: true }).click();
  await expect(editor(page)).toHaveText(raw.toString('base64'));
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  await expect.poll(async () => (await peers(request)).uploads.length).toBe(2);
  received = (await peers(request)).uploads[1];
  expect(received.base64).toBe(raw.toString('base64'));
  expect(received.headers['content-type']).toBe('multipart/form-data; boundary=fixture');
  await page.getByRole('button', { name: 'Edit & Resend', exact: true }).click();
  await expect(editor(page)).toHaveText(raw.toString('base64'));
  await page.reload();
  await page.getByRole('tab', { name: 'Composer', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Send', exact: true })).toBeDisabled();
  await expect(page.getByLabel('Send retained bytes anyway')).toBeVisible();
});

test('HAR Cancel has no effects; confirmed import stays offline and replays Whistle base64 exactly', async ({ page, request }) => {
  await chooseHar(page, har(), false);
  await expect(page.locator('.requests tbody tr')).toHaveCount(0);
  expect((await peers(request)).uploads).toHaveLength(0);
  await chooseHar(page, har(), true);
  await expect(page.getByText('Imported 1; 1 retained, 0 evicted.', { exact: true })).toBeVisible();
  await expect(page.locator('.requests tbody tr')).toHaveCount(1);
  expect((await peers(request)).uploads).toHaveLength(0);
  await page.locator('.requests tbody tr').click();
  await expect(page.getByText('Imported HAR · offline', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Edit & Resend', exact: true }).click();
  await expect(editor(page)).toHaveText(base64);
  await expect(page.getByLabel('Headers', { exact: true })).not.toHaveValue(/:authority|:method/);
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  await expect.poll(async () => (await peers(request)).uploads.length).toBe(1);
  expect((await peers(request)).uploads[0].base64).toBe(base64);
});

test('incomplete HAR is visibly blocked until the complete body file is loaded; malformed import is atomic', async ({ page, request }) => {
  const malformed = har();
  malformed.log.entries.push({ ...malformed.log.entries[0], request: { ...malformed.log.entries[0].request, postData: { mimeType: 'application/octet-stream', text: 'bad', base64: '!' } } });
  await chooseHar(page, malformed, true);
  await expect(page.locator('.har-result')).toContainText('invalid base64');
  await expect(page.locator('.requests tbody tr')).toHaveCount(0);
  await chooseHar(page, har(true), true);
  await expect(page.locator('.har-result')).toContainText('body is missing, truncated');
  await page.locator('.requests tbody tr').click();
  await page.getByRole('button', { name: 'Edit & Resend', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Send', exact: true })).toBeDisabled();
  await expect(page.getByLabel('Send retained bytes anyway')).toBeVisible();
  await bodyPicker(page).setInputFiles({ name: 'complete.bin', mimeType: 'application/octet-stream', buffer: bytes });
  await expect(page.getByLabel('Send retained bytes anyway')).toHaveCount(0);
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  await expect.poll(async () => (await peers(request)).uploads.length).toBe(1);
  expect((await peers(request)).uploads[0].base64).toBe(base64);
});

test('New cancels a delayed captured-body load rather than overwriting the new composition', async ({ page }) => {
  await chooseHar(page, har(), true);
  await page.locator('.requests tbody tr').click();
  let release!: () => void, entered!: () => void;
  const held = new Promise<void>((r) => { release = r; });
  const started = new Promise<void>((r) => { entered = r; });
  await page.route('**/body.bin?**', async (route) => {
    const response = await route.fetch();
    entered(); await held;
    await route.fulfill({ response });
  });
  await page.getByRole('button', { name: 'Edit & Resend', exact: true }).click();
  await started;
  await expect(page.getByRole('button', { name: 'Send', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: 'New', exact: true }).click();
  await page.getByLabel('URL', { exact: true }).fill('http://traffic.test/new');
  await editor(page).fill('new draft');
  const finished = page.waitForResponse((r) => r.url().includes('/body.bin?'));
  release(); await (await finished).finished();
  await expect(editor(page)).toHaveText('new draft');
  await expect(page.getByLabel('URL', { exact: true })).toHaveValue('http://traffic.test/new');
});

async function openFrames(page: Page, request: APIRequestContext) {
  await post(request, origin + '/ws/open');
  await expect.poll(async () => (await (await request.get('/sessions.json')).json()).length).toBe(1);
  await page.reload();
  await page.locator('.requests tbody tr').click();
  await page.getByRole('button', { name: 'Frames', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  return Number(await page.locator('.requests tbody tr').getAttribute('data-id'));
}

test('live WebSocket pause, ignore, pass, binary injection and disconnect affect both real peers', async ({ page, request }) => {
  const id = await openFrames(page, request);
  await page.getByLabel('send mode').selectOption('pause');
  await post(request, origin + '/ws/client-send', { base64 });
  await expect.poll(async () => (await (await request.get('/api/ws/status?id=' + id)).json()).send.held).toBe(1);
  expect((await peers(request)).fromClient).toHaveLength(0);
  await page.locator('.ws-compose input[type=file]').setInputFiles({ name: 'frame.bin', mimeType: 'application/octet-stream', buffer: bytes });
  await page.getByRole('button', { name: '▲ To server', exact: true }).click();
  await expect.poll(async () => (await peers(request)).fromClient.length).toBe(1);
  expect((await peers(request)).fromClient[0]).toEqual({ opcode: 2, masked: true, base64 });
  await page.getByLabel('send mode').selectOption('ignore');
  await expect.poll(async () => (await (await request.get('/api/ws/status?id=' + id)).json()).send.held).toBe(0);
  await post(request, origin + '/ws/client-send', { base64: 'ZHJvcA==' });
  await expect.poll(async () => (await (await request.get('/frames.json?id=' + id)).json()).filter((f: { ignored: boolean }) => f.ignored).length).toBe(2);
  await page.getByLabel('send mode').selectOption('pass');
  await post(request, origin + '/ws/client-send', { base64: 'cGFzcw==' });
  await expect.poll(async () => (await peers(request)).fromClient.length).toBe(2);
  expect((await peers(request)).fromClient[1].base64).toBe('cGFzcw==');

  await page.getByLabel('receive mode').selectOption('pause');
  await post(request, origin + '/ws/server-send', { base64 });
  await expect.poll(async () => (await (await request.get('/api/ws/status?id=' + id)).json()).receive.held).toBe(1);
  expect((await peers(request)).fromServer).toHaveLength(0);
  await page.getByLabel('Frame to send').fill(base64);
  await page.getByRole('button', { name: '▼ To client', exact: true }).click();
  await expect.poll(async () => (await peers(request)).fromServer.length).toBe(1);
  expect((await peers(request)).fromServer[0]).toEqual({ opcode: 2, masked: false, base64 });
  await page.getByLabel('receive mode').selectOption('ignore');
  await expect.poll(async () => (await (await request.get('/api/ws/status?id=' + id)).json()).receive.held).toBe(0);
  await page.getByLabel('receive mode').selectOption('pause');
  await post(request, origin + '/ws/server-send', { base64: 'cmVsZWFzZQ==' });
  await expect(page.getByRole('button', { name: 'Release', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Release', exact: true }).click();
  await expect.poll(async () => (await peers(request)).fromServer.length).toBe(2);
  expect((await peers(request)).fromServer[1].base64).toBe('cmVsZWFzZQ==');
  await expect(page.getByLabel('receive mode')).toHaveValue('pass');
  await page.screenshot({ path: test.info().outputPath('live-controls.png') });
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  await expect.poll(async () => { const s = await peers(request); return [s.clientLive, s.serverLive, s.serverReadEnded, s.serverWriteEnded]; }).toEqual([false, false, true, true]);
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toHaveCount(0);
  expect(await post(request, '/api/ws/abort', { id })).toEqual({ ok: true, closed: false });
});

test('WebSocket malformed binary never sends, empty binary is a valid frame, and closed sessions refuse sends', async ({ page, request }) => {
  const id = await openFrames(page, request);
  await page.getByLabel('Frame format').selectOption({ label: 'Binary · Base64' });
  await page.getByLabel('Frame to send').fill('not base64!');
  await page.getByRole('button', { name: '▲ To server', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('base64');
  expect((await peers(request)).fromClient).toHaveLength(0);
  for (const fields of [{ data: 2 }, { type: false }, { encoding: 4 }, { encoding: 'base64', data: '/w==' }]) {
    const response = await request.post('/api/ws/send', { data: { id, dir: 'send', ...fields } });
    expect(response.status()).toBe(400);
  }
  await page.getByLabel('Frame to send').fill('');
  await page.getByRole('button', { name: '▲ To server', exact: true }).click();
  await expect.poll(async () => (await peers(request)).fromClient.length).toBe(1);
  expect((await peers(request)).fromClient[0]).toEqual({ opcode: 2, masked: true, base64: '' });
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  expect((await request.post('/api/ws/send', { data: { id, dir: 'send', data: 'x' } })).status()).toBe(400);
});

test('a delayed status poll cannot undo a newly applied WebSocket mode', async ({ page, request }) => {
  await openFrames(page, request);
  let release!: () => void, entered!: () => void;
  const held = new Promise<void>((r) => { release = r; });
  const started = new Promise<void>((r) => { entered = r; });
  let first = true;
  await page.route('**/api/ws/status?**', async (route) => {
    const response = await route.fetch();
    if (first) { first = false; entered(); await held; }
    await route.fulfill({ response });
  });
  await started;
  await page.getByLabel('send mode').selectOption('pause');
  await expect(page.getByText('send: pause', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Release', exact: true })).toBeVisible();
  const finished = page.waitForResponse((r) => r.url().includes('/api/ws/status?') && r.request().method() === 'GET');
  release(); await (await finished).finished();
  await expect(page.getByLabel('send mode')).toHaveValue('pause');
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
});
