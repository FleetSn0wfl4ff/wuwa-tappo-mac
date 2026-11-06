import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const source = await readFile(new URL('../scripts/mac-platform.js', import.meta.url), 'utf8');

// Extract the exact `normalise` function that ships in the injected script.
// This keeps the test coupled to the real rewrite, not a copy.
function extractNormalise(text) {
  const marker = 'const normalise = value => {';
  const start = text.indexOf(marker);
  assert.notEqual(start, -1, 'normalise() must exist in mac-platform.js');
  let index = start + marker.length;
  let depth = 1;
  while (index < text.length && depth > 0) {
    if (text[index] === '{') depth += 1;
    else if (text[index] === '}') depth -= 1;
    index += 1;
  }
  assert.equal(depth, 0, 'normalise() braces must balance');
  return new Function('value', text.slice(start + marker.length, index - 1));
}

const normalise = extractNormalise(source);

test('protocol-relative captcha URLs are pinned to https', () => {
  assert.equal(
    normalise('//gcaptcha4.geetest.com/load?callback=cb&risk_type=slide'),
    'https://gcaptcha4.geetest.com/load?callback=cb&risk_type=slide',
  );
});

test('tauri:// third-party URLs are rewritten before they hit the bundle', () => {
  assert.equal(
    normalise('tauri://gcaptcha4.geetest.com/load?callback=cb&risk_type=slide'),
    'https://gcaptcha4.geetest.com/load?callback=cb&risk_type=slide',
  );
  assert.equal(
    normalise('tauri://static.geetest.com/captcha_v4/policy/x.js'),
    'https://static.geetest.com/captcha_v4/policy/x.js',
  );
});

test('application-local and already-https URLs are left alone', () => {
  assert.equal(normalise('tauri://localhost/assets/index.js'), 'tauri://localhost/assets/index.js');
  assert.equal(normalise('https://gcaptcha4.geetest.com/load?x=1'), 'https://gcaptcha4.geetest.com/load?x=1');
  assert.equal(normalise('data:text/plain,x'), 'data:text/plain,x');
  assert.equal(normalise(undefined), undefined);
});

test('geetest document-relative asset paths go to static.geetest.com', () => {
  assert.equal(normalise('captcha_v4/policy/x.js'), 'https://static.geetest.com/captcha_v4/policy/x.js');
  assert.equal(normalise('js/gcaptcha4.js'), 'https://static.geetest.com/js/gcaptcha4.js');
  assert.equal(normalise('v4/abc'), 'https://static.geetest.com/v4/abc');
});

test('the injected script patches every path a captcha can arrive through', () => {
  assert.match(source, /Object\.defineProperty\(HTMLScriptElement\.prototype, 'src'/);
  assert.match(source, /for \(const constructor of \['HTMLImageElement', 'HTMLIFrameElement'\]\)/);
  assert.match(source, /Element\.prototype\.setAttribute/);
  assert.match(source, /Node\.prototype\.appendChild/);
  assert.match(source, /Node\.prototype\.insertBefore/);
  assert.match(source, /XMLHttpRequest\.prototype\.open/);
  assert.match(source, /window\.fetch/);
  assert.match(source, /new Request\(fixed, input\)/);
  assert.match(source, /windowLabel === 'login'/);
  assert.doesNotMatch(source, /crossOrigin/);
  assert.doesNotMatch(source, /<base\b/i);
});
