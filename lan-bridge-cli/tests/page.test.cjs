const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const script = fs.readFileSync(path.join(__dirname, '../static/index.html'), 'utf8')
  .match(/<script>([\s\S]*?)<\/script>/)[1];
const png = Uint8Array.from([137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0]);
const file = (name, size = 12) => ({id: name, name, size});
const settle = async () => {
  for (let i = 0; i < 5; i++) await new Promise(resolve => setImmediate(resolve));
};

function page(entries, {imageFetch, decodeFails = false, observer = true} = {}) {
  class Element {
    constructor(tag) {
      this.tag = tag;
      this.children = [];
      this.attributes = {};
      this.disabled = false;
      this.open = false;
    }
    append(...children) { this.children.push(...children); }
    replaceChildren(...children) { this.children = children; }
    setAttribute(name, value) { this.attributes[name] = value; }
    removeAttribute(name) { delete this[name]; delete this.attributes[name]; }
    addEventListener() {}
    close() { this.open = false; }
    showModal() { this.open = true; }
    decode() { return decodeFails ? Promise.reject(Error('invalid image')) : Promise.resolve(); }
  }
  const elements = new Map(), calls = [], urls = [], revoked = [], observers = [];
  class Observer {
    constructor(callback) { this.callback = callback; this.targets = new Set(); observers.push(this); }
    observe(element) { this.targets.add(element); }
    unobserve(element) { this.targets.delete(element); }
    disconnect() { this.targets.clear(); }
    intersect() {
      this.callback([...this.targets].map(target => ({target, isIntersecting: true})));
    }
  }
  class Socket { static CLOSING = 2; constructor() { this.readyState = 1; } }
  const context = vm.createContext({
    document: {
      getElementById(id) {
        if (!elements.has(id)) elements.set(id, new Element(id));
        return elements.get(id);
      },
      createElement(tag) { return new Element(tag); },
    },
    window: {addEventListener() {}},
    location: {protocol: 'http:', host: 'localhost'},
    WebSocket: Socket,
    IntersectionObserver: observer ? Observer : undefined,
    AbortController, Blob, Uint8Array, FormData, setTimeout,
    URL: {
      createObjectURL(blob) {
        const url = 'blob:preview-' + urls.length;
        urls.push({url, blob});
        return url;
      },
      revokeObjectURL(url) { revoked.push(url); },
    },
    fetch: async (url, options) => {
      calls.push({url, options});
      if (url === '/api/files') return new Response(JSON.stringify(entries));
      if (imageFetch) return imageFetch(url, options);
      return new Response(png, {headers: {'Content-Type': 'application/octet-stream'}});
    },
  });
  vm.runInContext(script, context);
  return {
    context, elements, calls, urls, revoked, observers,
    load: () => vm.runInContext('files()', context),
    intersect: () => observers[0].intersect(),
    clear: () => vm.runInContext('clearPreviews()', context),
    buttons: () => [...vm.runInContext('previews.keys()', context)],
  };
}

test('images load lazily with access headers; SVG and ordinary files have no preview', async () => {
  const p = page([file('截图.PNG'), file('notes.txt'), file('unsafe.svg')]);
  await p.load();
  assert.equal(p.buttons().length, 1);
  assert.equal(p.calls.length, 1, 'listing does not eagerly download images');
  p.intersect();
  await settle();
  assert.equal(p.calls.length, 2);
  const request = p.calls[1];
  assert.equal(request.url, '/api/files/' + encodeURIComponent('截图.PNG'));
  assert.equal(request.options.headers['X-Access-Key'], '__KEY__');
  assert.ok(request.options.signal instanceof AbortSignal);
  assert.equal(p.urls[0].blob.type, 'image/png');
  assert.equal(p.buttons()[0].children[0].tag, 'img');
  assert.equal(p.buttons()[0].disabled, false);
  p.buttons()[0].onclick();
  assert.equal(p.elements.get('imageViewer').open, true);
  assert.equal(p.elements.get('fullImage').src, p.urls[0].url);
  assert.equal(p.calls.length, 2, 'full-size view reuses the image, no second download');
  p.elements.get('closeImage').onclick();
  assert.equal(p.elements.get('imageViewer').open, false);
  assert.equal(p.elements.get('fullImage').src, undefined);
});

test('refresh revokes object URLs and discards old preview state', async () => {
  const p = page([file('photo.jpg')]);
  await p.load();
  p.intersect();
  await settle();
  const url = p.urls[0].url;
  const signal = p.calls[1].options.signal;
  await p.load();
  assert.equal(signal.aborted, true);
  assert.deepEqual(p.revoked, [url]);
  assert.equal(p.buttons().length, 1);
  assert.equal(p.buttons()[0].textContent, '图片预览待加载…');
});

test('at most two previews load concurrently and refresh aborts old requests', async () => {
  const p = page([file('a.png'), file('b.png'), file('c.png')], {
    imageFetch: (url, options) => new Promise((resolve, reject) => {
      options.signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
    }),
  });
  await p.load();
  p.intersect();
  assert.equal(p.calls.length, 3, 'one list request plus two image requests');
  const signals = p.calls.slice(1).map(call => call.options.signal);
  p.clear();
  await settle();
  assert.ok(signals.every(signal => signal.aborted));
  assert.equal(p.calls.length, 3, 'queued stale preview is never requested');
  assert.equal(p.buttons().length, 0);
});

test('oversized files are not fetched and renamed SVG content is rejected', async () => {
  const p = page([file('large.png', 21 * 1024 * 1024), file('disguised.png')], {
    imageFetch: () => new Response('<svg xmlns="http://www.w3.org/2000/svg"></svg>'),
  });
  await p.load();
  assert.equal(p.buttons().length, 1);
  p.intersect();
  await settle();
  assert.equal(p.urls.length, 0);
  assert.equal(p.buttons()[0].textContent, '无法预览，请下载查看');
  assert.equal(p.buttons()[0].disabled, true);
});

test('stream limit also handles files growing after the listing', async () => {
  let cancelled = false;
  const p = page([file('changed.png')], {
    imageFetch: () => new Response(new ReadableStream({
      start(controller) { controller.enqueue(new Uint8Array(20 * 1024 * 1024 + 1)); },
      cancel() { cancelled = true; },
    })),
  });
  await p.load();
  p.intersect();
  await settle();
  assert.equal(cancelled, true);
  assert.equal(p.urls.length, 0);
  assert.match(p.buttons()[0].title, /20 MiB/);
});

test('decode failure revokes the temporary URL, leaving downloads available', async () => {
  const p = page([file('broken.png')], {decodeFails: true});
  await p.load();
  p.intersect();
  await settle();
  assert.deepEqual(p.revoked, [p.urls[0].url]);
  assert.equal(p.buttons()[0].textContent, '无法预览，请下载查看');
  assert.equal(p.elements.get('list').children[0].children[1].textContent, '下载');
});

test('without IntersectionObserver preview is explicitly click-to-load', async () => {
  const p = page([file('a.png')], {observer: false});
  await p.load();
  assert.equal(p.calls.length, 1);
  assert.equal(p.buttons()[0].textContent, '点击加载图片预览');
  p.buttons()[0].onclick();
  await settle();
  assert.equal(p.calls.length, 2);
  assert.equal(p.buttons()[0].children[0].tag, 'img');
});

test('signature checks support only PNG, JPEG, GIF and WebP', () => {
  const p = page([]);
  for (const [signature, mime] of [
    [Array.from(png), 'image/png'],
    [[255, 216, 255], 'image/jpeg'],
    [Array.from(Buffer.from('GIF89a')), 'image/gif'],
    [Array.from(Buffer.from('RIFF1234WEBP')), 'image/webp'],
  ]) {
    assert.equal(
      vm.runInContext(`imageMime(new Uint8Array(${JSON.stringify(signature)}))`, p.context),
      mime,
    );
  }
  assert.throws(() => vm.runInContext('imageMime(new Uint8Array([]))', p.context));
});
