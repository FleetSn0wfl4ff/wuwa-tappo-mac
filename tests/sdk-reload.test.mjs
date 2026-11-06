import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';
import { patchDomesticLoginReload, locateDomesticSdkLoader } from '../scripts/patch-frontend.mjs';
import { findSource } from './frontend-context.mjs';

// The bundled loader is minified, so its identifiers differ per release
// (`we`/`$`/`E` in 1.0.3, `ge`/`b`/`y` in 1.0.4). Both captured shapes run
// through the same structural patch and the same behavioural harness.
const shapes = [
  ['1.0.3', await readFile(new URL('./fixtures/domestic-sdk-loader.js', import.meta.url), 'utf8')],
  ['1.0.4', await readFile(new URL('./fixtures/domestic-sdk-loader-1.0.4.js', import.meta.url), 'utf8')],
];
const renderer = await readFile(new URL('./fixtures/kuro-sdk-preact.js', import.meta.url), 'utf8');

function harness(original) {
  const patched = patchDomesticLoginReload(original);
  const { start, end, name, forceVar } = locateDomesticSdkLoader(patched);
  const loader = patched.slice(start, end);
  return { patched, loader, name, forceVar };
}

function setup(loader, name, { loaded = true, root = { childNodes: [], firstChild: null } } = {}) {
  const actions = [];
  let currentRoot = root;
  const outer = { id: 'app', intact: true };
  const oldScript = { src: 'https://pro-cdn-sdk.kurogame.com/pro/web-sdk/1.x/kr-sdk.js', remove() { actions.push('old-sdk-script-removed'); } };
  const unrelated = { src: 'https://example.com/unrelated.js', remove() { actions.push('unrelated-script-removed'); } };
  if (currentRoot) currentRoot.remove = () => { actions.push('sdk-root-removed'); currentRoot = null; };
  const appended = [];
  const document = {
    scripts: [oldScript, unrelated],
    createElement() { return { dataset: {}, remove() { actions.push('new-script-removed'); } }; },
    getElementById(id) { return id === 'KrSDK' ? currentRoot : id === 'app' ? outer : null; },
    head: { appendChild(element) { actions.push('new-script-appended'); appended.push(element); } },
  };
  // Every identifier the two captured loaders touch in their forced-reload
  // path, with the meaning it has upstream. Unused names are inert.
  const context = vm.createContext({
    document, console,
    window: { setTimeout() { return 1; }, clearTimeout() {} },
    E: null, y: null, S: null, f: null, P: null, R: null, D: null,
    x: 0, U: 0, u: 'ready', c() {}, te: null, Q: null, q: -1,
    W() { actions.push('existing-sdk-initialized'); },
    re() { actions.push('existing-sdk-initialized'); },
    $: () => loaded, b: () => loaded,
    tn: () => oldScript.src, Ze: () => oldScript.src,
    l() {}, a() {}, We: 8000, Be: 8000,
  });
  vm.runInContext(loader, context);
  return { actions, outer, appended, document, context, reload: force => context[name](force) };
}

for (const [version, original] of shapes) {
  const { patched, loader, name } = harness(original);

  test(`[${version}] only forced reload removes the exact old SDK script and SDK root before insertion`, async () => {
    const env = setup(loader, name);
    const pending = env.reload(true);
    assert.deepEqual(env.actions, ['old-sdk-script-removed', 'sdk-root-removed', 'new-script-appended']);
    assert.equal(env.outer.intact, true);
    assert.equal(env.document.getElementById('app'), env.outer);
    assert.equal(env.document.getElementById('KrSDK'), null);
    env.appended[0].onload();
    assert.equal(await pending, true);
  });

  test(`[${version}] non-forced reuse and first script loading preserve existing DOM`, async () => {
    const existing = setup(loader, name);
    assert.equal(await existing.reload(false), true);
    assert.deepEqual(existing.actions, ['existing-sdk-initialized']);
    const initial = setup(loader, name, { loaded: false });
    const pending = initial.reload(false);
    assert.deepEqual(initial.actions, ['new-script-appended']);
    assert.notEqual(initial.document.getElementById('KrSDK'), null);
    initial.appended[0].onload();
    assert.equal(await pending, false);
  });

  test(`[${version}] the in-flight guard avoids a second cleanup or script insertion`, async () => {
    const env = setup(loader, name);
    const first = env.reload(true);
    const second = env.reload(true);
    assert.equal(env.appended.length, 1);
    env.appended[0].onload();
    assert.deepEqual(await Promise.all([first, second]), [true, true]);
  });

  test(`[${version}] the patch is a single insertion at the appendChild seam`, () => {
    const { insertion, insertionIndex, forceVar: force, scriptVar } = locateDomesticSdkLoader(original);
    const cleanup = `,${force}&&([...document.scripts].filter(s=>s.src===${scriptVar}.src).forEach(s=>s.remove()),document.getElementById("KrSDK")?.remove())`;
    assert.equal(patched.replace(cleanup, ''), original);
    assert.equal(insertionIndex, original.indexOf(insertion));
    assert.equal((patched.match(/getElementById\("KrSDK"\)\?\.remove\(\)/g) ?? []).length, 1);
  });

  test(`[${version}] unknown upstream loader shapes fail instead of applying a broad DOM patch`, () => {
    assert.throws(() => patchDomesticLoginReload(''), /reload function changed/);
    assert.throws(() => patchDomesticLoginReload(original.replace('.dataset.kuroSdkRetry="true"', '.dataset.changed="true"')), /reload function changed/);
    assert.throws(() => patchDomesticLoginReload(original.replace(',document.head.appendChild(t),r=window.setTimeout(', ',document.head.append(t),r=window.setTimeout(')), /script insertion changed/);
  });

  test(`[${version}] the bundled Preact renders after forced-root cleanup`, async () => {
    const context = vm.createContext({ document: {}, setTimeout, clearTimeout });
    const runtime = () => vm.runInContext(`(()=>{${renderer};return {render:P,h:h$1};})()`, context);
    const first = runtime();
    const second = runtime();
    const oldRoot = { childNodes: [], firstChild: null };
    let firstRenders = 0;
    let staleRenders = 0;
    let cleanRenders = 0;
    first.render(first.h(() => { firstRenders += 1; return null; }, {}), oldRoot);
    second.render(second.h(() => { staleRenders += 1; return null; }, {}), oldRoot);
    assert.equal(firstRenders, 1);
    assert.equal(staleRenders, 0);

    const env = setup(loader, name, { root: oldRoot });
    const pending = env.reload(true);
    assert.equal(env.document.getElementById('KrSDK'), null);
    // The public SDK createEl() now creates a fresh root because the stale one is absent.
    const freshRoot = { childNodes: [], firstChild: null };
    const reloaded = runtime();
    reloaded.render(reloaded.h(() => { cleanRenders += 1; return null; }, {}), freshRoot);
    assert.equal(cleanRenders, 1);
    env.appended[0].onload();
    assert.equal(await pending, true);
  });
}

test('the loader shape shipped in the selected region frontend is patchable', () => {
  const bundled = findSource(source => source.includes('.dataset.kuroSdkRetry="true"')).source;
  const { loader, name } = harness(bundled);
  assert.ok(loader.includes('document.getElementById("KrSDK")?.remove()'));
  assert.ok(name);
});
