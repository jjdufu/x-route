// XRouter WebUI：zashboard 零 IP 内部通道加载器（经由宿主 xr clash-call / clash-stream <-> /dev/hvc1）
import { xrCmd, decodeRpcStdout } from "./logic.js";

function b64ToBytes(b64) {
  const bin = atob(b64 || "");
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function resolveUiPath(baseFile, ref) {
  const clean = String(ref || "").split(/[?#]/)[0].trim();
  if (!clean) return "";
  try {
    return new URL(clean, "https://z.local/" + String(baseFile || "").replace(/^\/+/, "")).pathname.replace(/^\/+/, "");
  } catch {
    return clean.replace(/^\/+/, "");
  }
}

export function isSpecialRef(ref) {
  const s = String(ref || "").trim();
  return !s || /^(?:data:|blob:|https?:|mailto:|about:|javascript:|#|%23)/i.test(s);
}

export const CSS_URL_RE = /url\(\s*(?:"([^"]*)"|'([^']*)'|([^'")\s]+))\s*\)/gi;

export function extractCssAssetRefs(cssText) {
  const out = [];
  for (const m of String(cssText || "").matchAll(CSS_URL_RE)) {
    const inner = m[1] !== undefined ? m[1] : m[2] !== undefined ? m[2] : m[3];
    if (!inner || isSpecialRef(inner)) continue;
    out.push({ raw: m[0], inner });
  }
  return out;
}

export const UI_MOD_PREFIX = "__xrui__/";

export function rewriteJsModuleImports(jsPath, jsText) {
  const jsDeps = new Set();
  const cssRefs = new Map();
  let out = String(jsText || "");

  out = out.replace(/(\b(?:from|import)\s*)(["'])(\.\.?\/[^"'\s\\]+\.m?js)\2/g, (_, kw, q, rel) => {
    const dep = resolveUiPath(jsPath, rel);
    if (!dep) return _;
    jsDeps.add(dep);
    return `${kw}${q}${UI_MOD_PREFIX}${dep}${q}`;
  });

  out = out.replace(/(\bimport\s*\(\s*)(["'`])(\.\.?\/[^"'`\s\\]+\.m?js)\2(\s*\))/g, (_, pre, _q, rel, post) => {
    const dep = resolveUiPath(jsPath, rel);
    if (!dep) return _;
    jsDeps.add(dep);
    return `${pre}"${UI_MOD_PREFIX}${dep}"${post}`;
  });

  out = out.replace(/(["'`])(\.\.?\/[^"'`\s\\]+\.css)\1/g, (raw, _q, rel) => {
    const dep = resolveUiPath(jsPath, rel);
    if (dep) cssRefs.set(raw, dep);
    return raw;
  });

  out = out.replace(/(["'])(\.\.?\/[A-Za-z0-9_./-]+\.m?js)\1/g, (raw, q, rel) => {
    const dep = resolveUiPath(jsPath, rel);
    if (!dep) return raw;
    jsDeps.add(dep);
    return `${q}data:text/javascript,${q}`;
  });

  return { code: out, jsDeps: [...jsDeps], cssRefs };
}

// 注入到同源 blob iframe <head> 最前方的传输垫片：拦截 xrouter.internal（与相对 API 路径）的 fetch / XHR / WebSocket
function buildShimCode(token) {
  return `(() => {
  var TOKEN = ${JSON.stringify(token)};
  var NativeFetch = window.fetch ? window.fetch.bind(window) : null;
  var NativeXHR = window.XMLHttpRequest;
  var NativeWS = window.WebSocket;

  function getT() {
    var t = window.parent && window.parent.__xrClashTransport;
    return t && t.token === TOKEN ? t : null;
  }

  function parseTarget(raw) {
    var s = String(raw || "");
    if (!s || /^(?:blob:|data:|about:)/i.test(s)) return null;
    try {
      var u = s.startsWith("/") && !s.startsWith("//")
        ? new URL(s, "http://xrouter.internal")
        : new URL(s, window.location.href);
      if (u.protocol === "blob:" || u.protocol === "data:") return null;
      if (u.hostname === "xrouter.internal") return u;
    } catch (_) {}
    return null;
  }

  function hdrToObj(h) {
    var out = {};
    if (!h) return out;
    if (typeof h.forEach === "function") {
      h.forEach(function (v, k) { out[String(k)] = String(v); });
    } else if (Array.isArray(h)) {
      for (var i = 0; i < h.length; i++) if (h[i] && h[i].length >= 2) out[String(h[i][0])] = String(h[i][1]);
    } else if (typeof h === "object") {
      for (var k in h) if (Object.prototype.hasOwnProperty.call(h, k) && h[k] !== undefined && h[k] !== null) out[k] = String(h[k]);
    }
    return out;
  }

  function b64Dec(b64) {
    var bin = atob(b64 || "");
    var u8 = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) u8[i] = bin.charCodeAt(i);
    return u8;
  }

  function bytesEnc(u8) {
    var s = "";
    for (var i = 0; i < u8.length; i += 32768) s += String.fromCharCode.apply(null, u8.subarray(i, i + 32768));
    return btoa(s);
  }

  async function bodyToB64(body) {
    if (body === undefined || body === null) return "";
    if (typeof body === "string") return bytesEnc(new TextEncoder().encode(body));
    if (body instanceof ArrayBuffer) return bytesEnc(new Uint8Array(body));
    if (ArrayBuffer.isView(body)) return bytesEnc(new Uint8Array(body.buffer, body.byteOffset, body.byteLength));
    if (typeof Blob !== "undefined" && body instanceof Blob) return bytesEnc(new Uint8Array(await body.arrayBuffer()));
    if (typeof URLSearchParams !== "undefined" && body instanceof URLSearchParams) return bytesEnc(new TextEncoder().encode(body.toString()));
    return bytesEnc(new TextEncoder().encode(String(body)));
  }

  if (typeof navigator !== "undefined" && navigator.serviceWorker) {
    try {
      Object.defineProperty(navigator.serviceWorker, "register", {
        configurable: true,
        value: function () { return Promise.resolve({}); },
      });
    } catch (_) {}
  }

  window.fetch = async function (input, init) {
    var url = typeof input === "string" ? input : (input && input.url) || "";
    var u = parseTarget(url);
    if (!u) return NativeFetch ? NativeFetch(input, init) : Promise.reject(new TypeError("network disabled"));
    var T = getT();
    if (!T) throw new TypeError("xrouter clash channel closed");
    var method = (init && init.method) || (typeof Request !== "undefined" && input instanceof Request && input.method) || "GET";
    var hdrs = hdrToObj((init && init.headers) || (typeof Request !== "undefined" && input instanceof Request && input.headers));
    var rawBody = init && init.body !== undefined ? init.body
      : (typeof Request !== "undefined" && input instanceof Request && method !== "GET" && method !== "HEAD" ? await input.clone().arrayBuffer() : null);
    var res = await T.request({
      method: String(method).toUpperCase(),
      path: u.pathname + u.search,
      headers: hdrs,
      bodyB64: await bodyToB64(rawBody),
    });
    if (!res || (!res.ok && !res.status)) throw new TypeError((res && res.error) || "clash proxy error");
    var st = Number(res.status) || 502;
    var emptySt = st === 204 || st === 205 || st === 304;
    var bytes = emptySt ? null : b64Dec(res.body_b64 || "");
    return new Response(bytes, { status: st, headers: new Headers(res.headers || {}) });
  };

  function FakeXHR() {
    var self = this;
    var ev = new EventTarget();
    var nat = null;
    var method = "GET";
    var target = null;
    var urlStr = "";
    var reqHdrs = {};
    var respHdrs = {};
    var aborted = false;
    var timer = 0;

    self.readyState = 0;
    self.status = 0;
    self.statusText = "";
    self.response = null;
    self.responseText = "";
    self.responseURL = "";
    self.responseType = "";
    self.timeout = 0;
    self.withCredentials = false;
    self.upload = new EventTarget();
    self.onreadystatechange = null;
    self.onload = null;
    self.onloadend = null;
    self.onerror = null;
    self.onabort = null;
    self.ontimeout = null;
    self.onprogress = null;

    function emit(name) {
      var e = new Event(name);
      ev.dispatchEvent(e);
      var fn = self["on" + name];
      if (typeof fn === "function") fn.call(self, e);
    }

    self.addEventListener = function (t, l, o) {
      if (nat) return nat.addEventListener(t, l, o);
      ev.addEventListener(t, l, o);
    };
    self.removeEventListener = function (t, l, o) {
      if (nat) return nat.removeEventListener(t, l, o);
      ev.removeEventListener(t, l, o);
    };
    self.open = function (m, url, asyncFlag) {
      method = String(m || "GET").toUpperCase();
      urlStr = String(url || "");
      target = parseTarget(urlStr);
      if (!target) {
        nat = new NativeXHR();
        return nat.open(m, url, asyncFlag !== false);
      }
      nat = null;
      reqHdrs = {};
      respHdrs = {};
      aborted = false;
      self.readyState = 1;
      self.status = 0;
      self.statusText = "";
      self.response = null;
      self.responseText = "";
      self.responseURL = target.href;
      emit("readystatechange");
    };
    self.setRequestHeader = function (k, v) {
      if (nat) return nat.setRequestHeader(k, v);
      reqHdrs[String(k)] = String(v);
    };
    self.getAllResponseHeaders = function () {
      if (nat) return nat.getAllResponseHeaders();
      return Object.keys(respHdrs).map(function (k) { return k + ": " + respHdrs[k]; }).join("\\r\\n");
    };
    self.getResponseHeader = function (name) {
      if (nat) return nat.getResponseHeader(name);
      var lk = String(name || "").toLowerCase();
      for (var k in respHdrs) if (k.toLowerCase() === lk) return respHdrs[k];
      return null;
    };
    self.abort = function () {
      if (nat) return nat.abort();
      if (aborted || self.readyState === 4) return;
      aborted = true;
      clearTimeout(timer);
      self.readyState = 4;
      emit("abort");
      emit("loadend");
    };
    self.send = function (body) {
      if (nat) {
        nat.timeout = self.timeout;
        nat.withCredentials = self.withCredentials;
        if (self.responseType) nat.responseType = self.responseType;
        ["readystatechange", "load", "loadend", "error", "abort", "timeout", "progress"].forEach(function (n) {
          nat["on" + n] = function () {
            self.readyState = nat.readyState;
            self.status = nat.status;
            self.statusText = nat.statusText;
            self.response = nat.response;
            if (!self.responseType || self.responseType === "text") self.responseText = nat.responseText;
            self.responseURL = nat.responseURL;
            emit(n);
          };
        });
        return nat.send(body);
      }
      var T = getT();
      if (!T || !target) {
        queueMicrotask(function () {
          self.readyState = 4;
          self.status = 0;
          emit("readystatechange");
          emit("error");
          emit("loadend");
        });
        return;
      }
      if (self.timeout > 0) {
        timer = setTimeout(function () {
          if (aborted || self.readyState === 4) return;
          aborted = true;
          self.readyState = 4;
          self.status = 0;
          emit("timeout");
          emit("loadend");
        }, self.timeout);
      }
      Promise.resolve(bodyToB64(body)).then(function (bodyB64) {
        return T.request({
          method: method,
          path: target.pathname + target.search,
          headers: reqHdrs,
          bodyB64: bodyB64,
        });
      }).then(function (res) {
        if (aborted) return;
        clearTimeout(timer);
        if (!res || (!res.ok && !res.status)) {
          self.readyState = 4;
          self.status = 0;
          emit("readystatechange");
          emit("error");
          emit("loadend");
          return;
        }
        respHdrs = res.headers || {};
        self.status = Number(res.status) || 200;
        self.statusText = self.status >= 200 && self.status < 300 ? "OK" : "";
        var bytes = b64Dec(res.body_b64 || "");
        var rt = self.responseType || "";
        if (rt === "arraybuffer") {
          self.response = bytes.buffer;
        } else if (rt === "blob") {
          self.response = new Blob([bytes], { type: self.getResponseHeader("content-type") || "application/octet-stream" });
        } else {
          var txt = new TextDecoder("utf-8").decode(bytes);
          self.responseText = txt;
          if (rt === "json") {
            try { self.response = JSON.parse(txt); } catch (_) { self.response = null; }
          } else {
            self.response = txt;
          }
        }
        self.readyState = 4;
        emit("readystatechange");
        emit("load");
        emit("loadend");
      }, function () {
        if (aborted) return;
        clearTimeout(timer);
        self.readyState = 4;
        self.status = 0;
        emit("readystatechange");
        emit("error");
        emit("loadend");
      });
    };
  }
  FakeXHR.UNSENT = 0; FakeXHR.OPENED = 1; FakeXHR.HEADERS_RECEIVED = 2; FakeXHR.LOADING = 3; FakeXHR.DONE = 4;
  window.XMLHttpRequest = FakeXHR;

  function FakeWS(url, protocols) {
    var u = parseTarget(url);
    if (!u) return new NativeWS(url, protocols);
    var self = this;
    var ev = new EventTarget();
    var ctrl = null;
    self.url = u.href;
    self.protocol = "";
    self.extensions = "";
    self.bufferedAmount = 0;
    self.binaryType = "blob";
    self.readyState = 0;
    self.onopen = null;
    self.onmessage = null;
    self.onerror = null;
    self.onclose = null;
    self.addEventListener = function (t, l, o) { ev.addEventListener(t, l, o); };
    self.removeEventListener = function (t, l, o) { ev.removeEventListener(t, l, o); };

    function emit(name, e) {
      ev.dispatchEvent(e);
      var fn = self["on" + name];
      if (typeof fn === "function") fn.call(self, e);
    }

    var T = getT();
    if (!T) {
      queueMicrotask(function () {
        self.readyState = 3;
        emit("error", new Event("error"));
        emit("close", new CloseEvent("close", { code: 1006, reason: "channel closed", wasClean: false }));
      });
    } else {
      ctrl = T.openWs(u.pathname + u.search, {
        onOpen: function () {
          if (self.readyState !== 0) return;
          self.readyState = 1;
          emit("open", new Event("open"));
        },
        onMessage: function (binary, payload) {
          if (self.readyState !== 1) return;
          var data = payload;
          if (binary) {
            var u8 = b64Dec(payload || "");
            data = self.binaryType === "arraybuffer" ? u8.buffer : new Blob([u8]);
          }
          emit("onmessage" === "onmessage" ? "message" : "message", new MessageEvent("message", { data: data }));
        },
        onError: function () {
          emit("error", new Event("error"));
        },
        onClose: function (code, reason) {
          if (self.readyState === 3) return;
          self.readyState = 3;
          emit("close", new CloseEvent("close", {
            code: Number(code) || 1000,
            reason: String(reason || ""),
            wasClean: (Number(code) || 1000) === 1000,
          }));
        },
      });
    }

    self.send = function (data) {
      if (self.readyState !== 1 || !ctrl) return;
      if (typeof data === "string") {
        ctrl.send(false, data);
      } else if (data instanceof ArrayBuffer) {
        ctrl.send(true, bytesEnc(new Uint8Array(data)));
      } else if (ArrayBuffer.isView(data)) {
        ctrl.send(true, bytesEnc(new Uint8Array(data.buffer, data.byteOffset, data.byteLength)));
      } else if (typeof Blob !== "undefined" && data instanceof Blob) {
        data.arrayBuffer().then(function (ab) { ctrl.send(true, bytesEnc(new Uint8Array(ab))); });
      } else {
        ctrl.send(false, String(data));
      }
    };
    self.close = function () {
      if (self.readyState >= 2) return;
      self.readyState = 2;
      if (ctrl) ctrl.close();
      else {
        self.readyState = 3;
        emit("close", new CloseEvent("close", { code: 1000, reason: "", wasClean: true }));
      }
    };
  }
  FakeWS.CONNECTING = 0; FakeWS.OPEN = 1; FakeWS.CLOSING = 2; FakeWS.CLOSED = 3;
  FakeWS.prototype.CONNECTING = 0; FakeWS.prototype.OPEN = 1; FakeWS.prototype.CLOSING = 2; FakeWS.prototype.CLOSED = 3;
  window.WebSocket = FakeWS;
})();`;
}

export function createClashEmbed({ xr, spawn, frame, stateBox, stateMsg, reloadBtn, isKsu }) {
  const token = Math.random().toString(36).slice(2) + Date.now().toString(36);
  const activeWs = new Map();
  const blobUrls = [];
  let wsSeq = 0;
  let loadSeq = 0;
  let mountedPort = 0;
  let loading = false;
  let lastEnabled = false;

  async function clashCall(payload) {
    const r = await xr("clash-call", JSON.stringify(payload));
    return decodeRpcStdout(r.stdout) || { ok: false, error: r.stderr || "内部通道无响应" };
  }

  function closeAllWs() {
    for (const [id, entry] of activeWs) {
      activeWs.delete(id);
      entry.closed = true;
      xr("clash-ws-close", id).catch(() => {});
    }
  }

  function revokeBlobs() {
    while (blobUrls.length) {
      const u = blobUrls.pop();
      try { URL.revokeObjectURL(u); } catch { /* ignore */ }
    }
  }

  function teardown() {
    loadSeq++;
    loading = false;
    mountedPort = 0;
    closeAllWs();
    if (window.__xrClashTransport && window.__xrClashTransport.token === token) {
      delete window.__xrClashTransport;
    }
    if (!frame.hidden) frame.hidden = true;
    if (frame.getAttribute("src") && frame.getAttribute("src") !== "about:blank") {
      frame.src = "about:blank";
    }
    revokeBlobs();
    stateBox.hidden = false;
    stateMsg.textContent = "等待开启或就绪…";
    reloadBtn.hidden = true;
  }

  function installTransport() {
    window.__xrClashTransport = {
      token,
      request(req) {
        return clashCall({
          op: "req",
          method: req.method || "GET",
          path: req.path || "/",
          headers: req.headers || {},
          body_b64: req.bodyB64 || "",
        });
      },
      openWs(path, handlers) {
        const wsId = `w${++wsSeq}`;
        const entry = { closed: false, queue: Promise.resolve() };
        activeWs.set(wsId, entry);

        let opened = false;
        let chunkBuf = null;
        let child = null;

        const handleObj = (msg) => {
          if (!msg || entry.closed) return;
          if (typeof msg._chunks === "number" && msg._chunks > 0) {
            chunkBuf = { need: msg._chunks, parts: [] };
            return;
          }
          if (msg.op === "ws_open_ok" || (msg.id === "open" && msg.ok === true)) {
            if (!opened) {
              opened = true;
              handlers.onOpen && handlers.onOpen();
            }
          } else if (msg.id === "open" && msg.ok === false) {
            activeWs.delete(wsId);
            entry.closed = true;
            handlers.onError && handlers.onError(msg.error);
            handlers.onClose && handlers.onClose(1006, msg.error || "");
          } else if (msg.op === "ws_msg") {
            handlers.onMessage && handlers.onMessage(!!msg.binary, msg.binary ? msg.data_b64 : msg.data);
          } else if (msg.op === "ws_close") {
            activeWs.delete(wsId);
            entry.closed = true;
            handlers.onClose && handlers.onClose(msg.code || 1000, msg.reason || "");
          }
        };

        const feedLine = (raw) => {
          const line = String(raw || "").trimEnd();
          if (!line) return;
          if (chunkBuf) {
            chunkBuf.parts.push(line.startsWith("@") ? line.slice(1) : line);
            if (chunkBuf.parts.length >= chunkBuf.need) {
              const joined = chunkBuf.parts.join("");
              chunkBuf = null;
              try { handleObj(JSON.parse(joined)); } catch { /* ignore */ }
            }
            return;
          }
          let parsed = null;
          try { parsed = JSON.parse(line); } catch { return; }
          if (parsed && typeof parsed._chunks === "number" && parsed._chunks > 0) {
            chunkBuf = { need: parsed._chunks, parts: [] };
          } else {
            handleObj(parsed);
          }
        };

        try {
          child = spawn(xrCmd("clash-stream", wsId, path));
          child.stdout.on("data", (d) => {
            for (const ln of String(d).split(/\r?\n/)) feedLine(ln);
          });
          child.on("exit", () => {
            if (!entry.closed) {
              activeWs.delete(wsId);
              entry.closed = true;
              handlers.onClose && handlers.onClose(1006, "");
            }
          });
          child.on("error", () => {
            if (!entry.closed) {
              activeWs.delete(wsId);
              entry.closed = true;
              handlers.onError && handlers.onError("spawn error");
              handlers.onClose && handlers.onClose(1006, "spawn error");
            }
          });
        } catch {
          activeWs.delete(wsId);
          entry.closed = true;
          queueMicrotask(() => handlers.onClose && handlers.onClose(1006, "spawn error"));
        }

        return {
          send(binary, data) {
            if (entry.closed) return;
            entry.queue = entry.queue
              .then(() => (entry.closed ? null : xr("clash-ws-send", wsId, binary ? "bin" : "text", String(data || ""))))
              .catch(() => {});
          },
          close() {
            if (entry.closed) return;
            entry.closed = true;
            activeWs.delete(wsId);
            xr("clash-ws-close", wsId).catch(() => {});
            handlers.onClose && handlers.onClose(1000, "");
          },
        };
      },
    };
  }

  async function mount(port) {
    const seq = ++loadSeq;
    loading = true;
    closeAllWs();
    revokeBlobs();
    frame.hidden = true;
    stateBox.hidden = false;
    reloadBtn.hidden = true;

    if (!isKsu()) {
      loading = false;
      stateMsg.textContent = "请在管理器 WebUI 中查看内置分流面板（纯内部 hvc1 通道）";
      return;
    }

    stateMsg.textContent = "正在通过内部通道加载界面…";
    installTransport();

    const assetCache = new Map();
    const fetchUiAsset = async (relPath, forceMime) => {
      const key = `${relPath}|${forceMime || ""}`;
      if (assetCache.has(key)) return assetCache.get(key);
      const res = await clashCall({ op: "ui", path: relPath });
      if (!res || !res.ok || !res.body_b64) {
        throw new Error((res && res.error) || `读取失败: ${relPath}`);
      }
      const bytes = b64ToBytes(res.body_b64);
      const blob = new Blob([bytes], { type: forceMime || res.mime || "application/octet-stream" });
      const url = URL.createObjectURL(blob);
      blobUrls.push(url);
      assetCache.set(key, url);
      return url;
    };

    const cssCache = new Map();
    const rewriteCssToBlob = async (cssPath) => {
      if (cssCache.has(cssPath)) return cssCache.get(cssPath);
      const cssRes = await clashCall({ op: "ui", path: cssPath });
      if (seq !== loadSeq) return "";
      if (!cssRes || !cssRes.ok || !cssRes.body_b64) {
        throw new Error(`读取样式失败: ${cssPath}`);
      }
      let cssText = new TextDecoder("utf-8").decode(b64ToBytes(cssRes.body_b64));
      for (const { raw, inner } of extractCssAssetRefs(cssText)) {
        const assetPath = resolveUiPath(cssPath, inner);
        if (!assetPath) continue;
        try {
          const blobUrl = await fetchUiAsset(assetPath);
          if (seq !== loadSeq) return "";
          cssText = cssText.split(raw).join(`url("${blobUrl}")`);
        } catch {
          // 非关键字体或装饰资源缺失时不中断面板加载
        }
      }
      const cssBlob = new Blob([cssText], { type: "text/css; charset=utf-8" });
      const cssUrl = URL.createObjectURL(cssBlob);
      blobUrls.push(cssUrl);
      cssCache.set(cssPath, cssUrl);
      return cssUrl;
    };

    const importMap = {};
    const jsVisited = new Set();
    const loadJsModuleGraph = async (entryPath, required) => {
      if (!entryPath || jsVisited.has(entryPath)) return;
      jsVisited.add(entryPath);
      const jsRes = await clashCall({ op: "ui", path: entryPath });
      if (seq !== loadSeq) return;
      if (!jsRes || !jsRes.ok || !jsRes.body_b64) {
        if (required) throw new Error((jsRes && jsRes.error) || `读取脚本失败: ${entryPath}`);
        return;
      }
      const rawJs = new TextDecoder("utf-8").decode(b64ToBytes(jsRes.body_b64));
      let { code, jsDeps, cssRefs } = rewriteJsModuleImports(entryPath, rawJs);
      for (const [rawLit, depCss] of cssRefs) {
        try {
          const cssUrl = await rewriteCssToBlob(depCss);
          if (seq !== loadSeq) return;
          if (cssUrl) code = code.split(rawLit).join(JSON.stringify(cssUrl));
        } catch { /* ignore optional chunk css */ }
      }
      const jsBlob = new Blob([code], { type: "application/javascript; charset=utf-8" });
      const jsUrl = URL.createObjectURL(jsBlob);
      blobUrls.push(jsUrl);
      importMap[`${UI_MOD_PREFIX}${entryPath}`] = jsUrl;

      for (const dep of jsDeps) {
        await loadJsModuleGraph(dep, false);
        if (seq !== loadSeq) return;
      }
    };

    try {
      const htmlRes = await clashCall({ op: "ui", path: "index.html" });
      if (seq !== loadSeq) return;
      if (!htmlRes || !htmlRes.ok || !htmlRes.body_b64) {
        throw new Error((htmlRes && htmlRes.error) || "无法读取 zashboard/index.html");
      }
      const rawHtml = new TextDecoder("utf-8").decode(b64ToBytes(htmlRes.body_b64));
      const doc = new DOMParser().parseFromString(rawHtml, "text/html");

      doc.querySelectorAll('link[rel="manifest"], link[rel="apple-touch-icon"], link[rel="prefetch"], link[rel="modulepreload"], script[src*="registerSW"]').forEach((el) => el.remove());
      doc.querySelectorAll('link[rel="icon"]').forEach((el) => {
        if (el.id) {
          const holder = doc.createElement("template");
          holder.id = el.id;
          el.replaceWith(holder);
        } else {
          el.remove();
        }
      });

      for (const link of [...doc.querySelectorAll('link[rel="stylesheet"][href]')]) {
        const href = link.getAttribute("href");
        if (!href || isSpecialRef(href)) continue;
        const cssPath = resolveUiPath("index.html", href);
        const cssUrl = await rewriteCssToBlob(cssPath);
        if (seq !== loadSeq) return;
        link.setAttribute("href", cssUrl);
      }

      for (const el of [...doc.querySelectorAll("img[src]")]) {
        const ref = el.getAttribute("src");
        if (!ref || isSpecialRef(ref)) continue;
        const rel = resolveUiPath("index.html", ref);
        try {
          const blobUrl = await fetchUiAsset(rel);
          if (seq !== loadSeq) return;
          el.setAttribute("src", blobUrl);
        } catch { /* ignore optional image */ }
      }

      for (const sc of [...doc.querySelectorAll("script[src]")]) {
        const src = sc.getAttribute("src");
        if (!src || isSpecialRef(src)) continue;
        const jsPath = resolveUiPath("index.html", src);
        if ((sc.getAttribute("type") || "").toLowerCase() === "module") {
          await loadJsModuleGraph(jsPath, true);
          if (seq !== loadSeq) return;
          sc.removeAttribute("src");
          sc.removeAttribute("crossorigin");
          sc.textContent = `import ${JSON.stringify(`${UI_MOD_PREFIX}${jsPath}`)};`;
        } else {
          try {
            const blobUrl = await fetchUiAsset(jsPath, "application/javascript; charset=utf-8");
            if (seq !== loadSeq) return;
            sc.setAttribute("src", blobUrl);
          } catch (err) {
            if (!sc.hasAttribute("onerror")) throw err;
            const onErr = sc.getAttribute("onerror") || "";
            sc.removeAttribute("src");
            sc.removeAttribute("onerror");
            sc.textContent = onErr;
          }
        }
      }

      if (doc.head) {
        if (Object.keys(importMap).length > 0) {
          const im = doc.createElement("script");
          im.type = "importmap";
          im.textContent = JSON.stringify({ imports: importMap });
          doc.head.prepend(im);
        }
        const shim = doc.createElement("script");
        shim.textContent = buildShimCode(token);
        doc.head.prepend(shim);
      }

      const finalHtml = "<!DOCTYPE html>\n" + doc.documentElement.outerHTML;
      const docUrl = URL.createObjectURL(new Blob([finalHtml], { type: "text/html; charset=utf-8" }));
      blobUrls.push(docUrl);

      mountedPort = port;
      loading = false;
      frame.hidden = false;
      stateBox.hidden = true;
      frame.src = `${docUrl}#/setup?hostname=xrouter.internal&port=${encodeURIComponent(String(port))}&secret=xrouter-internal&http=1`;
    } catch (e) {
      if (seq !== loadSeq) return;
      loading = false;
      mountedPort = 0;
      frame.hidden = true;
      stateBox.hidden = false;
      stateMsg.textContent = `加载失败：${(e && e.message) || e}`;
      reloadBtn.hidden = false;
    }
  }

  return {
    sync(showTab, port, pageVisible) {
      if (!showTab) {
        if (lastEnabled || mountedPort || loading) {
          lastEnabled = false;
          teardown();
        }
        return;
      }
      lastEnabled = true;
      if (pageVisible && !loading && mountedPort !== port) {
        mount(port);
      }
    },
    reload(port) {
      if (!lastEnabled) return;
      mountedPort = 0;
      mount(port);
    },
    teardown() {
      lastEnabled = false;
      teardown();
    },
  };
}
