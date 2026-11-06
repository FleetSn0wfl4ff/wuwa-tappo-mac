const INVOKE_WRAPPER = /async function ([$\w]+)\(e,t=\{\},n\)\{return window\.__TAURI_INTERNALS__\.invoke\(e,t,n\)\}/g;

export function patchSessionBarriers(source) {
  let changes = 0;
  const patched = source.replace(
    INVOKE_WRAPPER,
    (_, name) => {
      changes += 1;
      return `async function ${name}(e,t={},n){if(e==="open_login_window"||e==="open_panel_window"||e==="plugin:event|emit"&&(t.event==="cat-auth-login-success"||t.event==="cat-auth-logout"))await window.__WUWATAPPO_SESSION__?.flush();return window.__TAURI_INTERNALS__.invoke(e,t,n)}`;
    },
  );
  // The number of wrappers differs per region and per release: the CN 1.0.3
  // entry module keeps one, the GLOBAL one keeps two. Patch every wrapper the
  // entry module actually has and fail if any unpatched copy is left behind,
  // rather than pinning a count that silently drops the barrier.
  if (changes < 1) throw new Error(`Expected at least one original Tauri invoke wrapper in the entry module, found ${changes}. Reconcile the app version before building.`);
  if (/async function [$\w]+\(e,t=\{\},n\)\{return window\.__TAURI_INTERNALS__\.invoke\(e,t,n\)\}/.test(patched)) {
    throw new Error('An entry-module Tauri invoke wrapper was left without the session barrier.');
  }
  return patched;
}

export function patchLoginReady(source) {
  const original='function N(n){return o(u,t=>{n(t.payload??{})})}';
  const replacement='async function N(n){const unlisten=await o(u,t=>{n(t.payload??{})});await window.__TAURI_INTERNALS__.invoke("login_window_ready");return unlisten}';
  if(source.split(original).length!==2) throw new Error('Original login listener bridge changed; reconcile modal readiness before building.');
  return source.replace(original,replacement);
}

export function patchDomesticSdkUrls(source) {
  if (!source.includes('const APP_REGION="CN"')) {
    throw new Error('Expected the official CN frontend. Extract the CN installer before building.');
  }
  let changes = 0;
  const patched = source.replace(
    /(isdk(?:Src|Css)=")(\/\/pro-cdn-sdk\.kurogame\.com\/pro\/web-sdk\/1\.x\/(?:kr-sdk\.js|style\.css)")/g,
    (_, prefix, url) => {
      changes += 1;
      return `${prefix}https:${url}`;
    },
  );
  if (changes !== 2) throw new Error(`Expected 2 CN SDK URLs, found ${changes}. Reconcile the app version before building.`);
  return patched;
}

// The forced-reload path is the only place that appends the KrSDK script tag.
// Its identifiers are minified (`we`/`t`/`r` in 1.0.3, `ge`/`t`/`r` in 1.0.4),
// so locate it by structure: the single `dataset.kuroSdkRetry` script tag, its
// enclosing async loader function, and the append/setTimeout pair inside it.
export function locateDomesticSdkLoader(source) {
  const retry = [...source.matchAll(/([$\w]+)\.dataset\.kuroSdkRetry="true"/g)];
  if (retry.length !== 1) {
    throw new Error('Original CN SDK reload function changed; reconcile the login reload patch before building.');
  }
  const marker = retry[0];
  const start = source.lastIndexOf('async function ', marker.index);
  const header = start < 0 ? null : /^async function ([$\w]+)\(([$\w]+)=!1\)\{/.exec(source.slice(start));
  if (!header) {
    throw new Error('Original CN SDK reload function changed; reconcile the login reload patch before building.');
  }
  const next = source.indexOf('async function ', start + 1);
  const end = next < 0 ? source.length : next;
  const body = source.slice(start, end);
  const insertionPattern = /,document\.head\.appendChild\(([$\w]+)\),([$\w]+)=window\.setTimeout\(/g;
  const insertions = [...body.matchAll(insertionPattern)];
  if (insertions.length !== 1 || insertions[0][1] !== marker[1]) {
    throw new Error('Original CN SDK script insertion changed; reconcile the login reload patch before building.');
  }
  const [insertion, scriptVar, timerVar] = insertions[0];
  return {
    start,
    end,
    marker: insertionPattern,
    insertion,
    insertionIndex: start + insertions[0].index,
    name: header[1],
    forceVar: header[2],
    scriptVar,
    timerVar,
    loader: body,
  };
}

export function patchDomesticLoginReload(source) {
  const { insertion, insertionIndex, forceVar, scriptVar } = locateDomesticSdkLoader(source);
  const cleanup = `,${forceVar}&&([...document.scripts].filter(s=>s.src===${scriptVar}.src).forEach(s=>s.remove()),document.getElementById("KrSDK")?.remove())`;
  return source.slice(0, insertionIndex) + cleanup + insertion + source.slice(insertionIndex + insertion.length);
}

// 1.0.3 routes the label through the component's own translation helper
// (`le.value=t("setting_version_check_failed")`) instead of the module-scope
// `n`, so capture that identifier rather than assuming its name. 1.0.4 also
// added `resolveUpdateMissingMessage()` in the entry module, which calls
// `i18n.global.t("setting_version_check_failed")` for the same key; selecting
// the view by that bare call would now match two files, so the file selector
// below reuses this exact component pattern instead.
export const MAC_UPDATE_FEEDBACK_PATTERN = /if\(([$\w]+)\.kind==="error"\)\{([^{}]*?)([$\w]+)\.value=([$\w]+)\("setting_version_check_failed"\);return\}/g;

export function matchesMacUpdateFeedback(source) {
  return [...source.matchAll(MAC_UPDATE_FEEDBACK_PATTERN)].length === 1;
}

export function patchMacUpdateFeedback(source) {
  const matches = [...source.matchAll(MAC_UPDATE_FEEDBACK_PATTERN)];
  if (matches.length !== 1) throw new Error('Original update feedback changed; reconcile the Mac update message before building.');
  return source.replace(MAC_UPDATE_FEEDBACK_PATTERN, (_, error, before, target, translate) => `if(${error}.kind==="error"){${before}${target}.value=${error}.message||${translate}("setting_version_check_failed");return}`);
}

export function patchMacUpdater(source) {
  const resolver = 'async function resolveUpdaterLatestJsonUrl(){const e=resolveUpdaterLatestHostJsonUrl();if(!e)return null;';
  const recovery = 'async function resolveUpdaterLatestJsonUrl(){let e=resolveUpdaterLatestHostJsonUrl();if(!e){const config=unwrapApiData(await activityInfo());const host=typeof config?.assets_host==="string"?config.assets_host.trim():"";if(!host||host===LOCAL_ROLE_ASSETS_HOST)throw new Error("官方配置缺少更新资源地址");setUpdaterAssetsHost(host,{triggerSilentCheck:!1});e=resolveUpdaterLatestHostJsonUrl()}';
  const failure = 'if(logDiagnostic("UPDATE","check_failed",{stage:"check",reason_code:diagnosticErrorCode(t),fallback:e?.interactive?"report_error":"treat_as_latest"},"warn"),e?.interactive)return{kind:"error",message:n};try{const s=await getVersion();return console.warn("[updater] check skipped (treat as latest)",t),{kind:"none",currentVersion:s}}catch{return{kind:"error",message:n}}';
  const report = 'return logDiagnostic("UPDATE","check_failed",{stage:"check",reason_code:diagnosticErrorCode(t),fallback:"report_error"},"warn"),{kind:"error",message:n}';
  for (const original of [resolver, failure]) {
    if (source.split(original).length !== 2) throw new Error('Original updater flow changed; reconcile the Mac update path before building.');
  }
  return source.replace(resolver, recovery).replace(failure, report);
}

export function patchMacAbout(source, region = 'CN') {
  const headings = [...source.matchAll(/<h2 (data-v-[a-f0-9]+)>系统配置<\/h2>/g)];
  if (headings.length !== 1) throw new Error('Original About system section changed; reconcile the Mac build information before building.');
  const original = headings[0][0], scope = headings[0][1];
  const remotes = [...source.matchAll(/t=([$\w]+)\(\(\)=>k\("about_content"\)\?String\(v\("about_content"\)\)\.trim\(\):""\)/g)];
  if (remotes.length !== 1) throw new Error('Original remote About content changed; reconcile the Mac build information before building.');
  const [remote, computed] = remotes[0];
  const info = region === 'GLOBAL'
    ? `<h2 ${scope}>macOS community build</h2><p ${scope}>Wuwa Tappo Global Mac · Unofficial · Apple Silicon · macOS 13+</p>`
    : `<h2 ${scope}>macOS 重建版</h2><p ${scope}>Wuwa Tappo CN Mac · 非官方重建 · Apple Silicon · macOS 13 及以上</p>`;
  const banner = `<section class="about-section" ${scope}>${info}</section>`;
  return source
    .replace(original, `${info}<h3 ${scope}>Windows 原版系统配置</h3>`)
    .replace(remote, `t=${computed}(()=>{const html=k("about_content")?String(v("about_content")).trim():"";return html?${JSON.stringify(banner)}+html.replace("系统配置","Windows 原版系统配置").replace("系統配備","Windows 原版系統配備"):""})`);
}

export function patchGlobalAuthListener(source) {
  const bridge = 'async function onOpenUrl(e){return await listen("deep-link://new-url",t=>{e(t.payload)})}';
  const wake = 'async function onOpenUrl(e){return await listen("mac-oauth-callback-ready",()=>{getCurrent().then(t=>{if(t?.length)return e(t)}).catch(()=>console.warn("[macOS] OAuth callback could not be read"))})}';
  const current = 'try{const t=await getCurrent();t?.length&&handleRawUrls(t)}catch(t){console.warn("[overseas-auth] getCurrent failed:",t)}';
  const listener = 'try{unlisten=await onOpenUrl(t=>{handleRawUrls(t)})}catch(t){throw console.warn("[overseas-auth] onOpenUrl failed:",t),bound=!1,t}';
  if (source.split(bridge).length !== 2 || source.split(current + listener).length !== 2) throw new Error('Original GLOBAL OAuth callback binding changed; reconcile cold and warm callback delivery before building.');
  return source.replace(bridge, wake).replace(current + listener, listener + current);
}

export function patchMacLayoutBarriers(source) {
  const tree = parse(source, { sourceType: 'module', ecmaVersion: 'latest' });
  const targets = [];
  function visit(node, callback) {
    if (!node || typeof node !== 'object') return;
    callback(node);
    for (const value of Object.values(node)) {
      if (Array.isArray(value)) value.forEach(child => visit(child, callback));
      else if (value && typeof value === 'object') visit(value, callback);
    }
  }
  visit(tree, node => {
    if (node.type !== 'FunctionDeclaration') return;
    const content = source.slice(node.start, node.end);
    if (content.includes('[taskbar-insets] desktop state refresh failed')
      || (content.includes('[main-window-bounds] sync failed') && content.includes('syncMainWindowContentBoundsNow'))) targets.push(node);
  });
  if (targets.length !== 2) throw new Error('Original desktop layout barriers changed; reconcile macOS layout before building.');
  const frames = [];
  for (const target of targets) {
    const matches = [];
    visit(target.body, node => {
      if (node.type === 'AwaitExpression' && node.argument.type === 'NewExpression'
        && node.argument.callee.type === 'Identifier' && node.argument.callee.name === 'Promise'
        && source.slice(node.argument.start, node.argument.end).includes('requestAnimationFrame')) matches.push(node);
    });
    if (matches.length !== 1) throw new Error('Original desktop layout frame changed; reconcile macOS layout before building.');
    frames.push(matches[0]);
  }
  for (const frame of frames.sort((a, b) => b.start - a.start)) source = source.slice(0, frame.start) + 'await __wuwatappoMacLayoutBarrier()' + source.slice(frame.end);
  const helper = `async function __wuwatappoMacLayoutBarrier(){
  const nativeWindow=window.__TAURI__.window.getCurrentWindow();
  const [size,scale]=await Promise.all([nativeWindow.innerSize(),nativeWindow.scaleFactor()]);
  const root=document.documentElement;
  if(!root)throw new Error("macOS DOM layout root is unavailable");
  getComputedStyle(root).getPropertyValue("--pet-scale");
  const rootRect=root.getBoundingClientRect();
  const dockRect=document.querySelector(".pet-dock")?.getBoundingClientRect();
  const width=window.innerWidth,height=window.innerHeight;
  if(!Number.isFinite(scale)||scale<=0||!Number.isFinite(width)||!Number.isFinite(height)||width<=0||height<=0||!Number.isFinite(size.width)||!Number.isFinite(size.height)||Math.abs(width-size.width/scale)>2||Math.abs(height-size.height/scale)>2)throw new Error("macOS layout viewport does not match native window");
  for(const rect of[rootRect,dockRect])if(rect&&![rect.left,rect.top,rect.width,rect.height].every(Number.isFinite))throw new Error("macOS DOM layout geometry is invalid");
}`;
  return helper + source;
}
import { parse } from 'acorn';
