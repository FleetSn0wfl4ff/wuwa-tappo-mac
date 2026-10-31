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
      // The official login SDK builds several third-party URLs as
      // protocol-relative references ("//gcaptcha4.geetest.com/load?..."). On an
      // https page those resolve to https:, but this page's origin is
      // tauri://localhost, so they resolve to tauri://<host>/... and are answered
      // by the app bundle instead of the real server: the JSONP callback never
      // runs, the HTML response throws "Unexpected token '<'" and the captcha can
      // never load. Pin every such URL back to https: before it is requested.
      try {
        const normalise = value => {
          if (typeof value !== 'string') return value;
          if (value.startsWith('//')) return `https:${value}`;
          const resolved = /^tauri:\/\/([^/?#]+)(.*)$/.exec(value);
          if (resolved && resolved[1] !== 'localhost') return `https://${resolved[1]}${resolved[2]}`;
          const asset = /^\/?(captcha_v4\/.*|js\/gcaptcha4.*|v4\/.*)$/.exec(value);
          return asset ? `https://static.geetest.com/${asset[1]}` : value;
        };

        const scriptDescriptor = Object.getOwnPropertyDescriptor(HTMLScriptElement.prototype, 'src');
        if (scriptDescriptor?.set) {
          Object.defineProperty(HTMLScriptElement.prototype, 'src', {
            configurable: true,
            enumerable: scriptDescriptor.enumerable,
            get: scriptDescriptor.get,
            set(value) {
              scriptDescriptor.set.call(this, normalise(value));
            },
          });
        }
        for (const constructor of ['HTMLImageElement', 'HTMLIFrameElement']) {
          const proto = window[constructor]?.prototype;
          const descriptor = proto ? Object.getOwnPropertyDescriptor(proto, 'src') : null;
          if (!descriptor?.set) continue;
          Object.defineProperty(proto, 'src', {
            configurable: true,
            enumerable: descriptor.enumerable,
            get: descriptor.get,
            set(value) {
              descriptor.set.call(this, normalise(value));
            },
          });
        }
        const setAttribute = Element.prototype.setAttribute;
        Element.prototype.setAttribute = function patchedSetAttribute(name, value) {
          return setAttribute.call(this, name, /^(?:src|href)$/i.test(String(name)) ? normalise(value) : value);
        };
        // Some SDK resources are already resolved to tauri://<host>/... by the
        // time they reach the DOM (and geetest also hands out document-relative
        // puzzle paths), so normalise again right before insertion.
        const normaliseElement = node => {
          if (!node || node.nodeType !== 1) return;
          for (const property of ['src', 'href']) {
            let value;
            try {
              value = node[property];
            } catch {
              continue;
            }
            if (typeof value !== 'string') continue;
            const fixed = normalise(value);
            if (fixed === value) continue;
            try {
              node[property] = fixed;
            } catch {
              /* ignore */
            }
          }
        };
        const appendChild = Node.prototype.appendChild;
        Node.prototype.appendChild = function patchedAppend(node) {
          try {
            normaliseElement(node);
          } catch {
            /* ignore */
          }
          return appendChild.call(this, node);
        };
        const insertBefore = Node.prototype.insertBefore;
        Node.prototype.insertBefore = function patchedInsert(node, reference) {
          try {
            normaliseElement(node);
          } catch {
            /* ignore */
          }
          return insertBefore.call(this, node, reference);
        };

        const open = XMLHttpRequest.prototype.open;
        XMLHttpRequest.prototype.open = function patchedOpen(method, url, ...rest) {
          return open.call(this, method, normalise(url), ...rest);
        };
        if (typeof window.fetch === 'function') {
          const originalFetch = window.fetch;
          window.fetch = function patchedFetch(input, init) {
            let target = input;
            if (typeof input === 'string') target = normalise(input);
            else if (input && typeof input.url === 'string') {
              const fixed = normalise(input.url);
              if (fixed !== input.url) target = new Request(fixed, input);
            }
            return originalFetch.call(this, target, init);
          };
        }
      } catch (error) {
        writeDiagnostic(`[login-ux] url normalisation ${String(error)}`);
      }

      // Failure-only diagnostics: the login SDK reports some rejections with a
      // plain object, which the global handler can only render as "[object
      // Object]". Keep the real payload in the app log so a future regression is
      // diagnosable without rebuilding; nothing is logged on the happy path.
      window.addEventListener('unhandledrejection', event => {
        const reason = event.reason;
        let detail;
        try {
          detail = JSON.stringify(reason, Object.getOwnPropertyNames(Object(reason)));
        } catch {
          detail = String(reason);
        }
        writeDiagnostic(`[login] rejection ${String(detail).slice(0, 600)}`);
      });

      // A transparent strip keeps the borderless dialog draggable without
      // stealing text selection from the phone/code inputs.
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

      // Hide scrollbar visuals instead of mutating `overflow`. The dialog is
      // sized by the SDK, so changing `overflow` on its containers also changes
      // the box metrics the SDK itself measures; a scrollbar-only rule leaves
      // every layout dimension untouched.
      try {
        const style = document.createElement('style');
        style.setAttribute('data-wuwatappo', 'scrollbar');
        style.textContent = [
          '::-webkit-scrollbar{display:none!important;width:0!important;height:0!important}',
          '::-webkit-scrollbar-thumb,::-webkit-scrollbar-track{display:none!important}',
          'html,body{overflow:hidden!important;scrollbar-width:none}',
        ].join('\n');
        (document.head || document.documentElement).appendChild(style);
      } catch (error) {
        writeDiagnostic(`[login-ux] scrollbar ${String(error)}`);
      }
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
