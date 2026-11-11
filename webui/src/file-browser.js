// XRouter WebUI：深色文件夹浏览（root 列目录，参考 bindhosts webui/utils/file_selector.js）
// mode=dir：选择文件夹；mode=ovpn：只显示文件夹与 .ovpn / .conf 文件，点文件即选中
import { ICONS } from "./icons.js";
import { MEDIA_ROOT, toMediaPath, crumbs, parseLs } from "./logic.js";

const $ = (id) => document.getElementById(id);
let cur = MEDIA_ROOT;
let mode = "dir";
let xr = null;
let resolveFn = null;
let picked = null;

function iconEl(name) {
  const i = document.createElement("md-icon");
  i.textContent = ICONS[name] || "";
  return i;
}

// 目录切换不做过渡：读到结果后一次性替换列表
async function list(path) {
  const box = $("fb-list");
  const r = await xr("ls", path, mode === "ovpn" ? "ovpn" : "dir");
  if (r.errno !== 0 && path !== MEDIA_ROOT) return list(MEDIA_ROOT);
  cur = path;
  const items = parseLs(r.stdout);
  const nodes = items.map((it) => {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "fb-item" + (it.dir ? "" : " file");
    row.innerHTML = "<md-ripple></md-ripple>";
    row.append(iconEl(it.dir ? "folder" : "description"));
    const s = document.createElement("span"); s.textContent = it.name; row.append(s);
    if (it.dir) row.append(Object.assign(iconEl("chevron_right"), { className: "trail" }));
    row.onclick = () => {
      const p = cur + "/" + it.name;
      if (it.dir) list(p);
      else { picked = p; $("dlg-files").close("ok"); }
    };
    return row;
  });
  if (!nodes.length) {
    const e = document.createElement("div"); e.className = "fb-empty"; e.textContent = "空";
    nodes.push(e);
  }
  box.replaceChildren(...nodes);
  box.scrollTop = 0;
  renderCrumbs();
}

function renderCrumbs() {
  const c = $("fb-crumbs");
  const cs = crumbs(cur);
  c.replaceChildren(...cs.flatMap((x, i) => {
    const b = document.createElement("button");
    b.type = "button"; b.className = "crumb" + (i === cs.length - 1 ? " last" : "");
    if (i === 0) b.append(iconEl("home"));
    b.append(x.label);
    b.onclick = () => { if (x.path !== cur) list(x.path); };
    return i ? [Object.assign(iconEl("chevron_right"), { className: "sep" }), b] : [b];
  }));
  c.scrollLeft = c.scrollWidth;
  $("fb-up").disabled = cur === MEDIA_ROOT;
}

let bound = false;
function bindOnce() {
  if (bound) return; bound = true;
  $("fb-up").addEventListener("click", () => {
    if (cur === MEDIA_ROOT) return;
    list(cur.slice(0, cur.lastIndexOf("/")) || MEDIA_ROOT);
  });
  $("dlg-files").addEventListener("closed", () => {
    const d = $("dlg-files");
    let out = null;
    if (d.returnValue === "ok") out = mode === "dir" ? cur : picked;
    if (resolveFn) { resolveFn(out); resolveFn = null; }
  });
}

export async function openBrowser(opts) {
  bindOnce();
  xr = opts.xr; mode = opts.mode || "dir"; picked = null;
  $("fb-pick").hidden = mode !== "dir";
  $("dlg-files").returnValue = "";
  const start = opts.start ? toMediaPath(opts.start) : (mode === "ovpn" ? MEDIA_ROOT + "/Download" : MEDIA_ROOT);
  await list(start);
  return new Promise((resolve) => { resolveFn = resolve; $("dlg-files").show(); });
}
