(() => {
  // The official login SDK builds an analytics object whose persistence layer may
  // never be created on this origin. Every tracking call then throws
  // (`this.persistence.getOptTracking`), and the exception aborts the login flow
  // after an otherwise successful response, leaving the loading mask spinning.
  // Provide a no-op persistence only where an object has none of its own, so
  // tracking degrades to a no-op instead of breaking sign-in.
  try {
    const origin = typeof location !== 'undefined' ? String(location.protocol) : '';
    if (origin === 'tauri:' && !Object.getOwnPropertyDescriptor(Object.prototype, 'persistence')) {
      let reported = false;
      const stub = {
        getOptTracking: () => false,
        getEnableTracking: () => false,
        getDeviceId: () => '',
        setDeviceId: () => {},
        getDistinctId: () => '',
        setDistinctId: () => {},
        getUserProperties: () => ({}),
        getSuperProperties: () => ({}),
        getAccountId: () => '',
        setAccountId: () => {},
        setOptTracking: () => {},
        setEnableTracking: () => {},
        setSuperProperties: () => {},
        setUserProperties: () => {},
        clear: () => {},
        save: () => {},
        _load: () => {},
        _save: () => {},
      };
      Object.defineProperty(Object.prototype, 'persistence', {
        configurable: true,
        get() {
          if (!reported) {
            reported = true;
            writeDiagnostic('[sdk] analytics persistence was missing; using a no-op stub');
          }
          return stub;
        },
        set(value) {
          Object.defineProperty(this, 'persistence', {
            value,
            writable: true,
            configurable: true,
            enumerable: true,
          });
        },
      });
    }
  } catch (error) {
    console.warn('[macOS] Could not install persistence fallback', String(error));
  }

  const errorNames = new Set(['Error', 'TypeError', 'ReferenceError', 'SyntaxError', 'RangeError', 'EvalError', 'URIError', 'AggregateError']);

  function cleanMessage(value) {
    if (typeof value !== 'string') return 'Error details unavailable';
    if (/\b(?:token|access[_-]?token|refresh[_-]?token|id[_-]?token|infoToken|authorization|cookie|bearer|password|passwd|client[_-]?secret|sdk[_-]?openid)\b/i.test(value)) {
      return 'Sensitive error message omitted';
    }
    return value
      .replace(/(?:https?|wss?|tauri):\/\/[^\s<>"'()]+/gi, '[url omitted]')
      .replace(/[A-Za-z0-9_-]{32,}(?:\.[A-Za-z0-9_-]+)*/g, '[value omitted]')
      .replace(/[\x00-\x1f\x7f]/g, ' ')
      .slice(0, 400);
  }

  function sourceFrames(value) {
    if (typeof value !== 'string') return [];
    const locations = value.split('\n')
      .filter(line => /^\s*at\s|@|^(?:https?|wss?|tauri):\/\//.test(line))
      .map(line => line.replace(/[?#][^\s)]*/g, ''))
      .join('\n');
    return [...locations.matchAll(/(?:^|[\s/@(])([A-Za-z0-9_$.-]{1,120}\.m?js):(\d+)(?::(\d+))?/g)]
      .slice(0, 8)
      .map((match) => `${match[1]}:${match[2]}${match[3] ? `:${match[3]}` : ''}`);
  }

  function describeError(reason) {
    if (!(reason instanceof Error)) return null;
    return {
      name: errorNames.has(reason.name) ? reason.name : 'Error',
      message: cleanMessage(reason.message),
      frames: sourceFrames(reason.stack),
    };
  }

  function writeDiagnostic(message) {
    const invoke = window.__TAURI_INTERNALS__?.invoke ?? window.__TAURI__?.core?.invoke;
    if (typeof invoke !== 'function') {
      console.error('[macOS] Diagnostic bridge unavailable');
      return;
    }
    try {
      Promise.resolve(invoke('write_app_log', { level: 'error', message }))
        .catch(() => console.error('[macOS] Could not write diagnostic log'));
    } catch {
      console.error('[macOS] Could not write diagnostic log');
    }
  }

  window.addEventListener('error', event => {
    const error = describeError(event.error);
    const frames = error?.frames.length ? error.frames : sourceFrames(`at ${event.filename}:${event.lineno}:${event.colno}`);
    writeDiagnostic(`[webview] ${error ? `${error.name}: ${error.message}` : cleanMessage(event.message)}${frames.length ? ` at ${frames.join(' <- ')}` : ''}`);
  });
  window.addEventListener('unhandledrejection', event => {
    const error = describeError(event.reason);
    writeDiagnostic(error
      ? `[webview:unhandledrejection] ${error.name}: ${error.message}${error.frames.length ? ` at ${error.frames.join(' <- ')}` : ''}`
      : '[webview:unhandledrejection] Non-Error rejection');
  });

  // Login dialog: (1) keep the window at its fixed size and suppress the
  // scrollbars the SDK's viewport-sized wrappers would otherwise show; (2) expose
  // a small transparent drag strip at the top so the dialog stays movable without
  // stealing text selection from the phone/code inputs.
  try {
    const windowLabel = window.__TAURI_INTERNALS__?.metadata?.currentWindow?.label;
    if (windowLabel === 'login') {
      const strip = document.createElement('div');
      strip.setAttribute('data-tauri-drag-region', '');
      strip.style.cssText = 'position:fixed;left:0;top:0;right:56px;height:44px;z-index:2147483646;background:transparent;cursor:default';
      strip.addEventListener('mousedown', event => {
        if (event.button !== 0) return;
        // startDragging works even if the drag-region attribute is ignored.
        window.__TAURI__?.window?.getCurrentWindow?.()?.startDragging?.().catch(() => {});
      });
      const attachStrip = () => (document.body || document.documentElement).appendChild(strip);
      if (document.body) attachStrip();
      else document.addEventListener('DOMContentLoaded', attachStrip, { once: true });

      // The login view registers "onInitSccessful" immediately before calling
      // KrSDK.core.init(). When the SDK has already emitted that event the page
      // never sees it, concludes "current SDK is still not ready" and leaves the
      // dialog half-initialised (the get-code request then throws). Record every
      // SDK event from the moment the object exists and replay it for late
      // listeners.
      try {
        const hookSdkEvents = () => {
          const sdk = window.KrSDK;
          if (!sdk || typeof sdk !== 'object' || !sdk.event || typeof sdk.event !== 'object') return false;
          if (sdk.event.__wwtHooked) return true;
          const fired = new Set();
          const registrationNames = new Set(['on', 'once', 'addListener', 'removeListener', 'off']);
          for (const key of Object.keys(sdk.event)) {
            const original = sdk.event[key];
            if (typeof original !== 'function') continue;
            sdk.event[key] = function patched(...args) {
              const type = args[0];
              if (registrationNames.has(key)) {
                const callback = args[1];
                if ((key === 'on' || key === 'once' || key === 'addListener')
                  && typeof callback === 'function'
                  && typeof type === 'string'
                  && fired.has(type)) {
                  writeDiagnostic(`[login-sdk] replaying missed event ${type}`);
                  Promise.resolve().then(() => {
                    try {
                      callback({ type, replayed: true });
                    } catch (error) {
                      writeDiagnostic(`[login-sdk] replay callback failed ${String(error)}`);
                    }
                  });
                }
                return original.apply(this, args);
              }
              if (typeof type === 'string') fired.add(type);
              return original.apply(this, args);
            };
          }
          sdk.event.__wwtHooked = true;
          return true;
        };
        let hookAttempts = 0;
        const hookTimer = window.setInterval(() => {
          hookAttempts += 1;
          if (hookSdkEvents() || hookAttempts >= 200) window.clearInterval(hookTimer);
        }, 20);
      } catch (error) {
        writeDiagnostic(`[login-sdk] event recorder ${String(error)}`);
      }

      // The login page can receive the "SDK init" event while the SDK itself is
      // still not ready (seen as "current SDK is still not ready"), which leaves
      // the form painted but every SDK call broken. Watch for readiness and force
      // one script reload when it stalls.
      try {
        let attempts = 0;
        let reloadCount = 0;
        const ready = () => typeof window.KrSDK === 'object' && window.KrSDK !== null && typeof window.KrSDK.core === 'object';
        const reloadSdk = attempt => {
          const scripts = Array.from(document.scripts).filter(node => /kr-sdk\.js/.test(String(node.src || '')));
          const source = scripts[0]?.src;
          scripts.forEach(node => node.remove());
          try {
            delete window.KrSDK;
          } catch {
            window.KrSDK = undefined;
          }
          const host = document.getElementById('KrSDK');
          host?.remove();
          if (!source) {
            writeDiagnostic('[login-sdk] reload impossible: original src unknown');
            return;
          }
          const script = document.createElement('script');
          script.src = source;
          script.crossOrigin = 'anonymous';
          script.dataset.kuroSdkRetry = 'true';
          script.addEventListener('load', () => writeDiagnostic('[login-sdk] reloaded script loaded'));
          script.addEventListener('error', () => writeDiagnostic('[login-sdk] reloaded script FAILED'));
          document.head.appendChild(script);
          writeDiagnostic(`[login-sdk] forced reload #${attempt} of ${source.split('/').pop()}`);
        };
        const timer = window.setInterval(() => {
          attempts += 1;
          const ok = ready();
          if (ok) {
            window.clearInterval(timer);
            return;
          }
          if (attempts === 6) writeDiagnostic('[login-sdk] sdk not ready after 3s');
          // Retry the script until the SDK is actually usable instead of leaving
          // the dialog in its half-initialised state.
          if (attempts >= 8 && (attempts - 8) % 8 === 0 && reloadCount < 4) {
            reloadCount += 1;
            reloadSdk(reloadCount);
          }
          if (attempts >= 80) {
            window.clearInterval(timer);
            writeDiagnostic(`[login-sdk] gave up after 40s (reloads=${reloadCount})`);
          }
        }, 500);
      } catch (error) {
        writeDiagnostic(`[login-sdk] watchdog ${String(error)}`);
      }

      // The SDK is served from a CORS-enabled CDN, but the page inserts it as a
      // plain script, which turns every thrown error into an opaque
      // "Script error.". Marking it CORS keeps the real message available.
      try {
        const markSdkScript = node => {
          if (!node || node.tagName !== 'SCRIPT') return;
          const source = String(node.src || '');
          if (!/kr-sdk\.js/.test(source)) return;
          if (node.crossOrigin !== 'anonymous') {
            node.crossOrigin = 'anonymous';
          }
        };
        const watchSdkScript = node => {
          if (!node || node.tagName !== 'SCRIPT') return;
          if (!/kr-sdk\.js/.test(String(node.src || ''))) return;
          node.addEventListener('error', () => writeDiagnostic('[login-patch] sdk script failed to load'));
        };
        const appendChild = Node.prototype.appendChild;
        Node.prototype.appendChild = function patchedAppend(node) {
          try {
            markSdkScript(node);
            watchSdkScript(node);
          } catch {
            /* ignore */
          }
          return appendChild.call(this, node);
        };
        const insertBefore = Node.prototype.insertBefore;
        Node.prototype.insertBefore = function patchedInsert(node, reference) {
          try {
            markSdkScript(node);
          } catch {
            /* ignore */
          }
          return insertBefore.call(this, node, reference);
        };
      } catch (error) {
        writeDiagnostic(`[login-patch] crossorigin ${String(error)}`);
      }

      // The login dialog keeps a fixed window size. The SDK form is absolutely
      // positioned inside viewport-sized wrapper layers; those layers are what
      // start scrolling (and draw a scrollbar) once the form renders. Only those
      // full-viewport wrappers are pinned, so real lists inside the dialog (role
      // picker, agreement text) keep their own scrolling.
      const isViewportWrapper = element => {
        if (element === document.documentElement || element === document.body) return true;
        const style = window.getComputedStyle(element);
        if (style.position !== 'fixed' && style.position !== 'absolute') return false;
        const rect = element.getBoundingClientRect();
        return rect.width >= window.innerWidth * 0.9 && rect.height >= window.innerHeight * 0.9;
      };

      const pin = () => {
        const root = document.body;
        if (!root) return;
        const nodes = [document.documentElement, root, ...root.querySelectorAll('*')];
        for (const node of nodes.slice(0, 1500)) {
          if (!isViewportWrapper(node)) continue;
          const style = window.getComputedStyle(node);
          const scrollable = /(auto|scroll)/.test(style.overflowX) || /(auto|scroll)/.test(style.overflowY);
          if (!scrollable) continue;
          node.style.setProperty('overflow', 'hidden', 'important');
        }
      };

      // A single pass after the SDK has rendered is enough for the scrollbar and
      // keeps the dialog free of an observer that would re-scan the DOM on every
      // SDK mutation while the login request is in flight.
      for (const delay of [1500, 3000]) window.setTimeout(pin, delay);
    }
  } catch (error) {
    writeDiagnostic(`[login-ux] ${String(error)}`);
  }

  const state = { nativeInput: false };
  const productName = window.__WUWATAPPO_PROFILE__?.productName ?? 'Wuwa Tappo CN Mac';
  const isMainWindow = window.__TAURI_INTERNALS__?.metadata?.currentWindow?.label === 'main';
  if (isMainWindow) {
    for (const type of ['pointerdown', 'pointerup', 'mousedown', 'mouseup', 'click', 'dblclick', 'contextmenu']) {
      document.addEventListener(type, event => {
        if (state.nativeInput && event.isTrusted) {
          event.preventDefault();
          event.stopImmediatePropagation();
        }
      }, true);
    }
  }
  const initialize = async () => {
    const api = window.__TAURI__;
    if (!api) return;
    let attemptedRecovery = false;
    let checking = false;
    let previousStatus;
    const update = status => {
      state.nativeInput = status?.running === true;
      if (!isMainWindow) return;
      if (status?.granted === false || status?.running) attemptedRecovery = false;
      let warning = document.getElementById('wuwatappo-input-status');
      if (status?.running) warning?.remove();
      else {
        if (!warning) {
          warning = document.createElement('div');
          warning.id = 'wuwatappo-input-status';
          warning.setAttribute('role', 'status');
          // The overlay window is click-through outside the pet, so this notice
          // is informational only: it fades out and never claims to be clickable.
          warning.style.cssText = 'position:fixed;z-index:2147483647;left:16px;top:52px;max-width:460px;padding:12px 16px;border-radius:10px;background:#fff2df;color:#59321a;font:14px/1.5 system-ui;box-shadow:0 2px 16px #0002;pointer-events:none;opacity:0;transition:opacity .25s';
          (document.body || document.documentElement).append(warning);
        }
        warning.textContent = status?.error === 'input_status_unavailable'
          ? `无法读取 ${productName} 的键鼠监听状态。请从菜单栏图标的「启用键鼠监听」重试。`
          : !status?.granted
            ? `键鼠计数已暂停：请在「系统设置 → 隐私与安全性 → 输入监控」中允许 ${productName}，然后通过菜单栏图标退出并重新打开本应用。`
            : '已获得输入监控权限，但监听尚未启动。请从菜单栏图标的「启用键鼠监听」重试。';
        warning.style.opacity = '1';
        window.clearTimeout(warning.__hideTimer);
        warning.__hideTimer = window.setTimeout(() => {
          if (warning) warning.style.opacity = '0';
        }, 25000);
      }
      const signature = JSON.stringify([status?.granted ?? null, status?.running ?? null, status?.error ?? null]);
      if (signature !== previousStatus) {
        previousStatus = signature;
        api.core.invoke('write_app_log', { level: 'info', message: `[INPUT] granted=${status?.granted ?? 'unknown'} running=${!!status?.running} error=${status?.error ?? 'none'}` })
          .catch(() => console.error('[macOS] Could not write input status'));
      }
    };
    const check = async () => {
      if (checking) return;
      checking = true;
      try {
        const status = await api.core.invoke('get_input_status');
        update(status);
        if (isMainWindow && status.granted && !status.running && !attemptedRecovery) {
          attemptedRecovery = true;
          update(await api.core.invoke('resume_input_monitor'));
        }
      } catch {
        update({ granted: null, running: false, error: 'input_status_unavailable' });
      } finally { checking = false; }
    };
    try {
      await api.event.listen('mac-input-status', event => update(event.payload));
      await check();
      if (isMainWindow) {
        const timer = window.setInterval(check, 2000);
        window.addEventListener('pagehide', () => window.clearInterval(timer), { once: true });
      }
    } catch (error) {
      console.error('[macOS] Input status unavailable', String(error));
    }
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', initialize, { once: true });
  else initialize();
})();
