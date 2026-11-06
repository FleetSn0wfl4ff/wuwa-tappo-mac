import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const script = await readFile(new URL('../scripts/mac-platform.js', import.meta.url), 'utf8');
const drain = () => new Promise(resolve => setImmediate(resolve));

function webview({ profile, readyState = 'loading', label = 'main', rejectLog = false, throwLog = false, inputStatus = false, pendingInput = false, resumeStatus, rejectStatus = false } = {}) {
  const events = new Map();
  const documentEvents = new Map();
  const calls = [];
  const errors = [];
  const nodes = new Map();
  let currentStatus = typeof inputStatus === 'boolean' ? { granted: inputStatus, running: inputStatus, error: null } : inputStatus;
  let interval;
  let inputListener;
  function invoke(command, payload) {
    calls.push({ command, payload });
    if (command === 'write_app_log') {
      if (throwLog) throw new Error('synthetic-log-secret');
      return rejectLog ? Promise.reject(new Error('synthetic-log-secret')) : Promise.resolve();
    }
    if (command === 'resume_input_monitor') return Promise.resolve(resumeStatus ?? currentStatus);
    if (command === 'set_notice_region') return Promise.resolve();
    assert.equal(command, 'get_input_status');
    return rejectStatus ? Promise.reject(new Error('unavailable')) : Promise.resolve(currentStatus);
  }
  const storage = new Map();
  const window = {
    addEventListener(type, callback) { events.set(type, callback); },
    setTimeout(callback) { return 1; },
    clearTimeout() {},
    setInterval(callback, milliseconds) { assert.equal(milliseconds, 2000); interval = callback; return 1; },
    clearInterval(id) { assert.equal(id, 1); interval = undefined; },
    __WUWATAPPO_PROFILE__: profile,
    localStorage: {
      getItem(key) { return storage.has(key) ? storage.get(key) : null; },
      setItem(key, value) { storage.set(key, String(value)); },
    },
    __TAURI_INTERNALS__: { metadata: { currentWindow: { label } }, invoke },
    __TAURI__: {
      core: { invoke },
      event: {
        listen(type, callback) {
          assert.equal(type, 'mac-input-status');
          inputListener = callback;
          return pendingInput ? new Promise(() => {}) : Promise.resolve();
        },
      },
    },
  };
  const document = {
    readyState,
    addEventListener(type, callback) { documentEvents.set(type, callback); },
    getElementById(id) { return nodes.get(id); },
    createElement(tag) {
      const node = {
        tagName: tag, style: {}, children: [], handlers: {}, textContent: '', className: '',
        setAttribute() {},
        append(...children) { node.children.push(...children); },
        addEventListener(type, callback) { node.handlers[type] = callback; },
        querySelector(selector) { return node.children.find(child => child.className === selector.replace(/^\./, '')) ?? null; },
        getBoundingClientRect() { return { left: 16, top: 52, width: 460, height: 44 }; },
        remove() { nodes.delete(node.id); },
      };
      return node;
    },
    body: { append(node) { nodes.set(node.id, node); } },
  };
  vm.runInNewContext(script, { window, document, Error, TypeError, console: { error: (...args) => errors.push(args), warn: (...args) => errors.push(args) } });
  return {
    calls, errors, events, documentEvents,
    emit(type, event) { events.get(type)(event); },
    updateInput(running) { inputListener({ payload: { running } }); },
    setStatus(value) { currentStatus = value; },
    tick() { return interval?.(); },
    warning() { return nodes.get('wuwatappo-input-status')?.querySelector('.wuwatappo-input-status-text')?.textContent; },
    notice() { return nodes.get('wuwatappo-input-status'); },
    dismissed() { return storage.get('wuwatappo.input-notice-dismissed'); },
    noticeRegions() { return calls.filter(call => call.command === 'set_notice_region').map(call => call.payload.region); },
    closeNotice() {
      const notice = nodes.get('wuwatappo-input-status');
      notice.handlers.click ? notice.handlers.click() : notice.children.find(child => child.className === 'wuwatappo-input-status-close').handlers.click({
        preventDefault() {}, stopPropagation() {},
      });
    },
    logs() { return calls.filter(call => call.command === 'write_app_log' && call.payload.message.startsWith('[webview')).map(call => call.payload.message); },
  };
}

test('error and rejection diagnostics register before DOM and native input readiness', async () => {
  const view = webview();
  assert.equal(view.events.has('error'), true);
  assert.equal(view.events.has('unhandledrejection'), true);
  assert.equal(view.calls.length, 0);
  const reason = new TypeError('undefined is not an object (evaluating this.persistence.getOptTracking)');
  reason.stack = 'TypeError: detail\nhandleTrackData@https://sdk.example/pro/kr-sdk.js:4:123\nopenLoginView@tauri://localhost/assets/useLogin.js:2:6633';
  view.emit('unhandledrejection', { reason });
  await drain();
  assert.equal(view.logs()[0], '[webview:unhandledrejection] TypeError: undefined is not an object (evaluating this.persistence.getOptTracking) at kr-sdk.js:4:123 <- useLogin.js:2:6633');
  assert.equal(view.calls.some(call => call.command === 'get_input_status'), false);

  const pending = webview({ readyState: 'complete', pendingInput: true });
  pending.emit('unhandledrejection', { reason: new Error('initialization failed') });
  await drain();
  assert.match(pending.logs()[0], /initialization failed/);
});

test('diagnostics omit credentials, URL query values and high-entropy values', async () => {
  const view = webview();
  const reason = new Error('request failed at https://sdk.example/login?credential=synthetic-query-value');
  reason.stack = 'Error: synthetic-query-value.js:99:1\n at call (https://sdk.example/pro/kr-sdk.js?token=synthetic-stack-secret.js:2:9)';
  view.emit('unhandledrejection', { reason });
  view.emit('unhandledrejection', { reason: new Error('Authorization: Bearer synthetic-authorization-value') });
  view.emit('unhandledrejection', { reason: new Error('cookie=kuro_infoToken=synthetic-cookie-value') });
  view.emit('unhandledrejection', { reason: new Error('token=synthetic-token-value') });
  view.emit('unhandledrejection', { reason: new Error('failed value 0123456789abcdefghijklmnopqrstuvwxyz0123456789') });
  await drain();
  const logs = view.logs().join('\n');
  assert.match(logs, /\[url omitted\]/);
  assert.match(logs, /Sensitive error message omitted/);
  assert.match(logs, /\[value omitted\]/);
  for (const value of ['sdk.example', 'synthetic-query-value', 'synthetic-stack-secret', 'synthetic-authorization-value', 'synthetic-cookie-value', 'synthetic-token-value', '0123456789abcdefghijklmnopqrstuvwxyz0123456789']) {
    assert.equal(logs.includes(value), false);
  }
});

test('non-Error rejections never inspect payloads or stringify arbitrary values', async () => {
  const view = webview();
  const payload = {};
  for (const key of ['message', 'name', 'stack', 'toString']) {
    Object.defineProperty(payload, key, { get() { assert.fail(`Unexpected rejection property read: ${key}`); } });
  }
  view.emit('unhandledrejection', { reason: payload });
  view.emit('unhandledrejection', { reason: 'synthetic-string-credential' });
  await drain();
  assert.deepEqual(view.logs(), [
    '[webview:unhandledrejection] Non-Error rejection',
    '[webview:unhandledrejection] Non-Error rejection',
  ]);
});

test('unknown Error names are not logged and errors keep browser propagation', async () => {
  const view = webview();
  const reason = new Error('ordinary failure');
  reason.name = 'synthetic-private-name';
  reason.stack = '';
  const event = {
    reason,
    preventDefault() { assert.fail('The original rejection must remain visible'); },
    stopImmediatePropagation() { assert.fail('The original rejection must propagate'); },
  };
  view.emit('unhandledrejection', event);
  view.emit('error', {
    message: 'script failed', filename: 'https://cdn.example/assets/index.js', lineno: 7, colno: 11,
    preventDefault: event.preventDefault,
  });
  await drain();
  assert.deepEqual(view.logs(), [
    '[webview:unhandledrejection] Error: ordinary failure',
    '[webview] script failed at index.js:7:11',
  ]);
});

test('diagnostic write failures are handled without leaking their rejection payload', async () => {
  for (const options of [{ rejectLog: true }, { throwLog: true }]) {
    const view = webview(options);
    view.emit('unhandledrejection', { reason: new Error('SDK initialization failed') });
    await drain();
    assert.deepEqual(view.errors, [['[macOS] Could not write diagnostic log']]);
    assert.equal(JSON.stringify(view.errors).includes('synthetic-log-secret'), false);
  }
});

test('native input suppression still follows trusted main-window status', async () => {
  const view = webview({ readyState: 'complete', inputStatus: true });
  await drain();
  let prevented = 0;
  let stopped = 0;
  const input = isTrusted => ({ isTrusted, preventDefault() { prevented += 1; }, stopImmediatePropagation() { stopped += 1; } });
  view.documentEvents.get('mousedown')(input(true));
  view.documentEvents.get('mousedown')(input(false));
  assert.equal(prevented, 1);
  assert.equal(stopped, 1);
  view.updateInput(false);
  view.documentEvents.get('mousedown')(input(true));
  assert.equal(prevented, 1);
  assert.equal(webview({ label: 'panel' }).documentEvents.has('mousedown'), false);
});

test('an Enter that only commits an IME candidate never reaches the app shortcut', () => {
  const view = webview({ readyState: 'complete' });
  const keydown = view.documentEvents.get('keydown');
  assert.equal(typeof keydown, 'function', 'the capture-phase keyboard guard must be installed');
  const probe = (event) => { let stopped = false; keydown({ ...event, stopImmediatePropagation() { stopped = true; } }); return stopped; };
  assert.equal(probe({ key: 'Enter', keyCode: 13 }), false, 'a plain Enter still submits');
  assert.equal(probe({ key: 'Enter', keyCode: 229 }), true, 'the IME confirm key is swallowed');
  assert.equal(probe({ key: 'Enter', keyCode: 13, isComposing: true }), true, 'composing Enter is swallowed');
  view.documentEvents.get('compositionstart')({});
  assert.equal(probe({ key: 'Enter', keyCode: 13 }), true, 'Enter during a composition session is swallowed');
  assert.equal(probe({ key: 'a', keyCode: 65 }), false, 'other keys are untouched');
});

test('granting input access resumes once and removes the paused warning', async () => {
  const view = webview({ readyState: 'complete', resumeStatus: { granted: true, running: true, error: null } });
  await drain();
  assert.match(view.warning(), /输入监控/);
  assert.equal(view.calls.filter(call => call.command === 'resume_input_monitor').length, 0);
  view.setStatus({ granted: true, running: false, error: 'input_access_granted_waiting_for_resume' });
  await view.tick();
  assert.equal(view.warning(), undefined);
  assert.equal(view.calls.filter(call => call.command === 'resume_input_monitor').length, 1);
  view.setStatus({ granted: true, running: true, error: null });
  await view.tick();
  assert.equal(view.calls.filter(call => call.command === 'resume_input_monitor').length, 1);
  view.emit('pagehide', {});
  assert.equal(view.tick(), undefined);
});

test('a failed input recovery stays visible without repeatedly rebuilding the tap', async () => {
  const status = { granted: true, running: false, error: 'input_event_tap_disabled' };
  const view = webview({ readyState: 'complete', inputStatus: status, resumeStatus: status });
  await drain();
  assert.match(view.warning(), /监听尚未启动/);
  await view.tick();
  await view.tick();
  assert.equal(view.calls.filter(call => call.command === 'resume_input_monitor').length, 1);
  view.setStatus({ granted: false, running: false, error: 'input_monitoring_permission_required' });
  await view.tick();
  view.setStatus(status);
  await view.tick();
  assert.equal(view.calls.filter(call => call.command === 'resume_input_monitor').length, 2);
});

test('the input notice registers a clickable rectangle and dismisses for good', async () => {
  const view = webview({ readyState: 'complete' });
  await drain();
  assert.match(view.warning(), /输入监控/);
  const region = view.noticeRegions().at(-1);
  assert.deepEqual([region.left, region.top, region.width, region.height], [16, 52, 460, 44]);
  view.closeNotice();
  assert.equal(view.notice(), undefined);
  assert.equal(view.dismissed(), '1');
  assert.equal(view.noticeRegions().at(-1), null, 'closing the notice releases its screen rectangle');
  await view.tick();
  await view.tick();
  assert.equal(view.warning(), undefined, 'the 2s permission poll must not resurrect a dismissed notice');
});

test('a dismissed notice stays hidden across restarts and permission changes', async () => {
  const view = webview({ readyState: 'complete' });
  await drain();
  view.closeNotice();
  await view.tick();
  view.setStatus({ granted: false, running: false, error: 'input_monitoring_permission_revoked' });
  await view.tick();
  assert.equal(view.warning(), undefined);
  assert.equal(view.calls.filter(call => call.command === 'set_notice_region').at(-1).payload.region, null);
});

test('granting access after the notice closed does not re-open it', async () => {
  const view = webview({ readyState: 'complete' });
  await drain();
  view.closeNotice();
  view.setStatus({ granted: true, running: true, error: null });
  await view.tick();
  view.setStatus({ granted: false, running: false, error: 'input_monitoring_permission_required' });
  await view.tick();
  assert.equal(view.warning(), undefined);
});

test('unreadable input status is not reported as granted or running', async () => {
  const view = webview({ readyState: 'complete', rejectStatus: true });
  await drain();
  assert.match(view.warning(), /无法读取/);
  assert.equal(view.calls.filter(call => call.command === 'resume_input_monitor').length, 0);
});

test('input permission guidance names the current GLOBAL application', async () => {
  const view = webview({ readyState: 'complete', profile: { productName: 'Wuwa Tappo Global Mac' } });
  await drain();
  assert.match(view.warning(), /Wuwa Tappo Global Mac/);
  assert.doesNotMatch(view.warning(), /CN Mac/);
});
