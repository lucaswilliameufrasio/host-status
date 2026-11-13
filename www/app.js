"use strict";

/* Papéis das referências em status/DESIGN.md:
   A (Beszel) cabeçalho e gráficos, B (Stats) módulos de "Agora", C (Vercel) tabelas. */

const LIVE_MS = 5000;
const FULL_MS = 30000;
const STALE_SECONDS = 20;
const EVENT_GROUPS_SHOWN = 6;
const SVG = "http://www.w3.org/2000/svg";
const KIND_LABEL = {
  aceito: "Entrou",
  falha: "Senha ou chave recusada",
  usuario_invalido: "Usuário inválido",
  fechou_antes_de_autenticar: "Conexão sem login",
};
const KIND_DOT = { aceito: "ok", falha: "bad", usuario_invalido: "bad", fechou_antes_de_autenticar: "" };
const STATE_LABEL = { ok: "OK", atencao: "Atenção", problema: "Problema", desconhecido: "Sem dado" };
const STATE_DOT = { ok: "ok", atencao: "warn", problema: "bad", desconhecido: "" };
const ACARI_LABEL = {
  "Other large folders": "Outras pastas grandes",
  "Project builds": "Builds de projetos",
  "Git worktrees": "Worktrees do Git",
  "Developer caches": "Caches de desenvolvimento",
  "Toolchains and SDKs": "Toolchains e SDKs",
  "VMs and emulators": "VMs e emuladores",
  "Large files": "Arquivos grandes",
  "Temporary files": "Arquivos temporários",
  "Media": "Mídia",
  "Games": "Jogos",
  "Downloads": "Downloads",
  "App data": "Dados de apps",
};
const UNIT_BYTES = { B: 1, KB: 1e3, MB: 1e6, GB: 1e9, TB: 1e12 };

let lastDocument = null;
let lastLive = null;
let lastLiveAt = null;
let lastError = null;
let moreOpen = false;

/* ---------- utilitários ---------- */

function el(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined && text !== null) node.textContent = text;
  if (className) node.className = className;
  return node;
}

function svg(tag, attributes) {
  const node = document.createElementNS(SVG, tag);
  for (const [name, value] of Object.entries(attributes || {})) node.setAttribute(name, String(value));
  return node;
}

function replace(id, ...children) {
  const target = document.getElementById(id);
  target.replaceChildren(...children);
  return target;
}

function missing(value) {
  return value === null || value === undefined || Number.isNaN(value);
}

function number(value, digits) {
  return value.toLocaleString("pt-BR", { maximumFractionDigits: digits });
}

function percent(value) {
  return missing(value) ? "sem dado" : number(value, 1) + "%";
}

function plain(value) {
  return missing(value) ? "sem dado" : number(value, 2);
}

function scaledParts(value, units, base) {
  let index = 0;
  let amount = value;
  while (amount >= base && index < units.length - 1) {
    amount = amount / base;
    index += 1;
  }
  return [number(amount, amount >= 100 ? 0 : 1), units[index]];
}

function bytes(value) {
  if (missing(value)) return "sem dado";
  return scaledParts(value, ["B", "KB", "MB", "GB", "TB"], 1024).join(" ");
}

function bitrateParts(value) {
  if (missing(value)) return ["sem dado", ""];
  return scaledParts(value, ["bit/s", "kbit/s", "Mbit/s", "Gbit/s"], 1000);
}

function bitrate(value) {
  return bitrateParts(value).join(" ").trim();
}

function sameDay(a, b) {
  return a.toDateString() === b.toDateString();
}

function clock(ts) {
  if (missing(ts)) return "sem dado";
  const date = new Date(ts * 1000);
  const time = date.toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
  if (sameDay(date, new Date())) return time;
  return date.toLocaleDateString("pt-BR", { day: "2-digit", month: "2-digit" }) + " " + time;
}

function hour(ts) {
  return new Date(ts * 1000).toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
}

function duration(seconds) {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return days + " d " + hours + " h";
  if (hours > 0) return hours + " h " + minutes + " min";
  if (minutes > 0) return minutes + " min";
  return seconds + " s";
}

function dot(kind) {
  return el("span", null, "dot" + (kind ? " " + kind : ""));
}

function withDot(kind, text) {
  const wrapper = el("span", null, "state");
  wrapper.append(dot(kind), document.createTextNode(text));
  return wrapper;
}

function command(text) {
  return el("code", text, "cmd");
}

/* ---------- C: tabelas ---------- */

function proportion(value, max, text) {
  const wrapper = el("span", null, "proportion");
  const fill = el("i");
  fill.style.width = (max > 0 ? Math.max(0.04, Math.min(1, value / max)) * 100 : 0) + "%";
  if (!(value > 0)) fill.style.width = "0";
  wrapper.append(fill, el("b", text));
  return wrapper;
}

function table(headers, rows, emptyText) {
  if (rows.length === 0) return el("p", emptyText, "empty");
  const wrapper = el("div", null, "scroll");
  const node = el("table");
  const head = el("tr");
  for (const header of headers) {
    const cell = el("th", header.label, header.numeric ? "num" : "");
    cell.scope = "col";
    if (header.width) cell.style.width = header.width;
    head.append(cell);
  }
  const thead = el("thead");
  thead.append(head);
  const tbody = el("tbody");
  for (const row of rows) {
    const line = el("tr");
    row.forEach((value, index) => {
      const header = headers[index];
      const cell = el("td", null, header.className || (header.numeric ? "num" : ""));
      cell.dataset.label = header.label;
      if (value instanceof Node) cell.append(value);
      else if (!missing(value)) cell.textContent = value;
      line.append(cell);
    });
    tbody.append(line);
  }
  node.append(thead, tbody);
  wrapper.append(node);
  return wrapper;
}

/* ---------- B: módulos de "Agora" ---------- */

function key(series) {
  return el("span", null, "key" + (series ? " " + series : ""));
}

function rows(pairs) {
  const list = el("dl", null, "rows");
  for (const pair of pairs) {
    const line = el("div");
    const term = el("dt");
    if (pair.key !== undefined) term.append(key(pair.key));
    term.append(document.createTextNode(pair.label));
    const value = el("dd");
    if (pair.value instanceof Node) value.append(pair.value);
    else value.textContent = pair.value;
    line.append(term, value);
    list.append(line);
  }
  return list;
}

function sectionLabel(text) {
  return el("p", text, "section");
}

function ring(parts, label, ariaLabel) {
  const size = 84;
  const stroke = 8;
  const radius = (size - stroke) / 2;
  const circumference = 2 * Math.PI * radius;
  const node = svg("svg", { width: size, height: size, viewBox: "0 0 " + size + " " + size, class: "ring", role: "img", "aria-label": ariaLabel });
  node.append(svg("circle", { cx: size / 2, cy: size / 2, r: radius, class: "ring-track", "stroke-width": stroke }));
  let offset = 0;
  for (const part of parts) {
    const share = Math.max(0, Math.min(100, part.pct || 0)) / 100;
    if (share <= 0) continue;
    const length = share * circumference;
    node.append(svg("circle", {
      cx: size / 2, cy: size / 2, r: radius, class: "ring-part", "stroke-width": stroke,
      stroke: "var(" + part.color + ")",
      "stroke-dasharray": length + " " + (circumference - length),
      "stroke-dashoffset": -offset,
      transform: "rotate(-90 " + size / 2 + " " + size / 2 + ")",
    }));
    offset += length;
  }
  const text = svg("text", { x: size / 2, y: size / 2 + 6, "text-anchor": "middle", class: "ring-value" });
  text.textContent = label;
  node.append(text);
  return node;
}

function moduleCard(title, ...children) {
  const card = el("article", null, "card module");
  card.append(el("h2", title, "module-title"), ...children);
  return card;
}

const SERIES = ["s1", "s2", "s3"];
const SERIES_VAR = ["--series", "--series-2", "--series-3"];

function cpuModule(live) {
  const parts = live.cpu_parts || [];
  const ringParts = parts.length > 0
    ? parts.map((part, index) => ({ pct: part.pct, color: SERIES_VAR[index] }))
    : [{ pct: live.cpu_pct, color: "--series" }];
  const details = parts.map((part, index) => ({ key: SERIES[index], label: part.label, value: percent(part.pct) }));
  details.push({ key: "", label: "Ocioso", value: percent(live.cpu_idle) });
  if (!missing(live.temperature_c)) {
    details.push({ label: "Temperatura", value: number(live.temperature_c, 0) + " °C" });
  }
  return moduleCard(
    "CPU",
    ring(ringParts, missing(live.cpu_pct) ? "–" : Math.round(live.cpu_pct) + "%", "CPU em uso: " + percent(live.cpu_pct)),
    sectionLabel("Detalhes"),
    rows(details),
    sectionLabel("Carga média"),
    rows([
      { label: "1 minuto", value: plain(live.load1) },
      { label: "5 minutos", value: plain(live.load5) },
      { label: "15 minutos", value: plain(live.load15) },
    ]),
  );
}

function memoryModule(live) {
  const memory = live.memory;
  const swap = live.swap;
  if (!memory) return moduleCard("Memória", el("p", "Sem dado.", "empty"));
  const total = memory.total_bytes;
  const pct = (value) => ((value || 0) / total) * 100;
  const parts = memory.parts || [];
  const stack = el("div", null, "stack");
  parts.forEach((part, index) => {
    const piece = el("span", null, SERIES[index]);
    piece.style.width = pct(part.bytes) + "%";
    stack.append(piece);
  });
  const ringParts = parts
    .map((part, index) => ({ pct: pct(part.bytes), color: SERIES_VAR[index], ring: part.ring }))
    .filter((part) => part.ring);
  const details = parts.map((part, index) => ({ key: SERIES[index], label: part.label, value: bytes(part.bytes) }));
  details.push({ key: "", label: "Livre", value: bytes(memory.free_bytes) });
  details.push({ label: "Swap", value: swap ? bytes(swap.used_mb * 1048576) + " de " + bytes(swap.total_mb * 1048576) : "sem swap" });
  return moduleCard(
    "Memória",
    ring(ringParts, Math.round(memory.used_pct) + "%", "Memória em uso: " + percent(memory.used_pct)),
    sectionLabel("Detalhes"),
    rows([{ label: "Em uso", value: bytes(memory.used_bytes) + " de " + bytes(total) }]),
    stack,
    rows(details),
  );
}

function diskModule(live) {
  const disk = live.disk;
  if (!disk) return moduleCard("Disco", el("p", "Sem dado.", "empty"));
  const low = disk.used_pct >= 90;
  const acari = lastDocument && lastDocument.acari;
  const state = el("span", low ? "Pouco espaço" : "OK", "pill" + (low ? " bad" : ""));
  const details = [
    { label: "Estado", value: state },
    { key: low ? "bad" : "s1", label: "Usado", value: bytes(disk.total_bytes - disk.free_bytes) },
    { key: "", label: "Livre", value: bytes(disk.free_bytes) },
    { label: "Total", value: bytes(disk.total_bytes) },
  ];
  if (acari && !missing(acari.regenerable_bytes)) {
    details.push({ label: "Recuperável", value: bytes(acari.regenerable_bytes) });
  }
  return moduleCard(
    "Disco",
    ring([{ pct: disk.used_pct, color: low ? "--bad" : "--series" }], Math.round(disk.used_pct) + "%",
      "Disco usado: " + percent(disk.used_pct)),
    sectionLabel(disk.label || "Disco"),
    rows(details),
  );
}

const INTERNET_LABEL = { ok: "OK", sem_dns: "Sem DNS", sem_internet: "Sem internet", sem_rede: "Sem rede" };

function networkModule(live) {
  const pair = el("div", null, "pair");
  for (const [value, series, label] of [[live.rx_bps, "s1", "Entrada"], [live.tx_bps, "s2", "Saída"]]) {
    const cell = el("div");
    const [amount, unit] = bitrateParts(value);
    const big = el("strong", amount);
    if (unit) big.append(el("small", unit));
    const caption = el("span");
    caption.append(key(series), document.createTextNode(label));
    cell.append(big, caption);
    pair.append(cell);
  }
  const tailscale = lastDocument && lastDocument.tailscale ? lastDocument.tailscale.state : "sem dado";
  const network = lastDocument && lastDocument.network;
  const internet = network
    ? el("span", INTERNET_LABEL[network.status] || network.status, "pill" + (network.status === "ok" ? "" : " bad"))
    : "sem dado";
  const yesNo = (value) => (value ? "responde" : "sem resposta");
  return moduleCard(
    "Rede",
    pair,
    sectionLabel("Interface"),
    rows([
      { label: "Interface", value: live.interface || "sem dado" },
      { key: "s1", label: "Recebido desde o boot", value: bytes(live.rx_total_bytes) },
      { key: "s2", label: "Enviado desde o boot", value: bytes(live.tx_total_bytes) },
      { label: "Tailscale", value: tailscale },
    ]),
    sectionLabel("Conexão"),
    rows([
      { label: "Internet", value: internet },
      { label: "Gateway", value: network && network.gateway ? network.gateway + ", " + yesNo(network.gateway_ok) : "sem dado" },
      { label: "DNS", value: network ? yesNo(network.dns_ok) : "sem dado" },
    ]),
  );
}

function renderNow(live) {
  lastLive = live;
  replace("now", cpuModule(live), memoryModule(live), diskModule(live), networkModule(live));
}

/* ---------- A: cabeçalho ---------- */

function renderHost(data) {
  const host = data.host;
  if (host.name) {
    document.title = host.name;
    document.getElementById("title").textContent = host.name;
  }
  const items = [];
  if (host.role) items.push(host.role);
  if (host.os) items.push(host.os);
  if (host.chip) items.push(host.chip + (host.cores ? ", " + host.cores + " núcleos" : ""));
  if (host.boot_ts) items.push("ligado há " + duration(data.generated_at - host.boot_ts));
  const meta = document.getElementById("meta");
  meta.replaceChildren(meta.firstElementChild, ...items.map((text) => el("li", text)));
}

/* ---------- A: gráficos ---------- */

function niceTop(peak) {
  if (!(peak > 0)) return 1;
  const raw = peak / 4;
  const exponent = Math.pow(10, Math.floor(Math.log10(raw)));
  for (const step of [1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10]) {
    if (step * exponent >= raw) return step * exponent * 4;
  }
  return 40 * exponent;
}

function timeTicks(first, last, maxTicks) {
  const span = Math.max(last - first, 1);
  const choices = [5, 10, 15, 20, 30, 60, 120, 180, 240, 360].map((minutes) => minutes * 60);
  let interval = choices[choices.length - 1];
  for (const choice of choices) {
    if (span / choice <= maxTicks) {
      interval = choice;
      break;
    }
  }
  const offset = -new Date(first * 1000).getTimezoneOffset() * 60;
  const ticks = [];
  let tick = Math.ceil((first + offset) / interval) * interval - offset;
  while (tick <= last) {
    ticks.push(tick);
    tick += interval;
  }
  return ticks;
}

function memoryTotal() {
  const source = lastLive || (lastDocument && lastDocument.now);
  return source && source.memory ? source.memory.total_bytes : null;
}

const CHARTS = [
  {
    title: "CPU", sub: "Uso médio de todos os núcleos", top: () => 100, format: percent,
    series: [{ key: "cpu_pct", label: "CPU", kind: "area" }],
  },
  {
    title: "Carga", sub: "Processos prontos para rodar, média de 1 minuto", format: plain,
    series: [{ key: "load1", label: "Carga", kind: "area" }],
  },
  {
    title: "Rede", sub: "Tráfego da interface padrão", format: bitrate,
    series: [
      { key: "rx_bps", label: "Entrada", kind: "area", keyClass: "s1" },
      { key: "tx_bps", label: "Saída", kind: "line", keyClass: "s2" },
    ],
  },
];

function runs(points, keys, step) {
  // Trechos contínuos onde todas as chaves têm valor; coleta que faltou vira buraco.
  const result = [];
  let current = [];
  let previous = null;
  for (const point of points) {
    const complete = keys.every((name) => !missing(point[name]));
    const gap = previous !== null && point.ts - previous > step * 2.5;
    if (!complete || gap) {
      if (current.length > 0) result.push(current);
      current = [];
    }
    if (complete) current.push(point);
    previous = point.ts;
  }
  if (current.length > 0) result.push(current);
  return result;
}

function chartCard(points, config) {
  const card = el("article", null, "card chart");
  const head = el("div", null, "chart-head");
  const titles = el("div");
  titles.append(el("h2", config.title), el("p", config.sub, "sub"));
  head.append(titles);
  if (config.series.length > 1) {
    const legend = el("div", null, "legend");
    for (const series of config.series) {
      const item = el("span");
      item.append(key(series.keyClass), document.createTextNode(series.label));
      legend.append(item);
    }
    head.append(legend);
  }
  card.append(head);
  const keys = config.series.map((series) => series.key);
  const usable = points.filter((point) => keys.some((name) => !missing(point[name])));
  if (usable.length < 2) {
    card.append(el("p", "Ainda sem pontos suficientes neste período.", "empty"));
    return card;
  }
  const host = el("div", null, "plot");
  card.append(host);
  if (config.partialNote && usable[0].ts > points[0].ts) {
    card.append(el("p", config.partialNote(clock(usable[0].ts)), "note"));
  }
  let drawnWidth = 0;
  const draw = () => {
    const width = Math.floor(host.clientWidth);
    if (width > 0 && width !== drawnWidth) {
      drawnWidth = width;
      // O eixo do tempo cobre o período inteiro; onde a série não existe fica vazio.
      drawChart(host, points, config, width);
    }
  };
  card.draw = draw;
  if (window.ResizeObserver) {
    card.observer = new ResizeObserver(draw);
    card.observer.observe(host);
  }
  return card;
}

function drawChart(host, points, config, width) {
  const stacked = config.series.some((series) => series.kind === "band");
  let peak = 0;
  for (const point of points) {
    let sum = 0;
    for (const series of config.series) {
      const value = point[series.key] || 0;
      sum = stacked ? sum + value : Math.max(sum, value);
    }
    peak = Math.max(peak, sum);
  }
  const fixedTop = config.top ? config.top() : null;
  const top = fixedTop || niceTop(peak);
  const height = width < 420 ? 150 : 180;
  let widest = 0;
  for (let index = 0; index <= 4; index += 1) {
    widest = Math.max(widest, config.format((top / 4) * index).length);
  }
  const left = widest * 7 + 14;
  const bottom = 22;
  const first = points[0].ts;
  const last = points[points.length - 1].ts;
  const span = Math.max(last - first, 1);
  const step = points.length > 1 ? points[1].ts - points[0].ts : 60;
  const x = (ts) => left + ((ts - first) / span) * (width - left - 2);
  const y = (value) => (height - bottom) - (Math.min(Math.max(value, 0), top) / top) * (height - bottom - 8);

  const chart = svg("svg", { viewBox: "0 0 " + width + " " + height, width: width, height: height, role: "img" });
  const summary = config.series.map((series) => {
    const own = points.map((point) => point[series.key]).filter((value) => !missing(value));
    if (own.length === 0) return series.label + " sem dado";
    return series.label + " agora " + config.format(own[own.length - 1]) + ", pico " + config.format(Math.max(...own));
  }).join("; ");
  chart.setAttribute("aria-label", config.title + ": " + summary);

  for (let index = 0; index <= 4; index += 1) {
    const value = (top / 4) * index;
    const lineY = y(value);
    chart.append(svg("line", { x1: left, x2: width, y1: lineY, y2: lineY, class: "grid-line" }));
    const label = svg("text", { x: left - 10, y: lineY + 4, "text-anchor": "end", class: "axis" });
    label.textContent = config.format(value);
    chart.append(label);
  }
  for (const tick of timeTicks(first, last, width < 420 ? 3 : 6)) {
    const tickX = x(tick);
    // Rótulo perto da borda alinha para dentro em vez de ser cortado.
    let anchor = "middle";
    if (tickX > width - 22) anchor = "end";
    if (tickX < left + 22) anchor = "start";
    const label = svg("text", { x: tickX, y: height - 4, "text-anchor": anchor, class: "axis" });
    label.textContent = hour(tick);
    chart.append(label);
  }

  const base = height - bottom;
  if (stacked) {
    for (const run of runs(points, config.series.map((series) => series.key), step)) {
      let lower = run.map(() => 0);
      for (const series of config.series) {
        const upper = run.map((point, index) => lower[index] + point[series.key]);
        const upperEdge = run.map((point, index) => x(point.ts).toFixed(1) + "," + y(upper[index]).toFixed(1));
        const lowerEdge = run.map((point, index) => x(point.ts).toFixed(1) + "," + y(lower[index]).toFixed(1)).reverse();
        chart.append(svg("polygon", { points: upperEdge.concat(lowerEdge).join(" "), class: "band-" + series.band }));
        lower = upper;
      }
    }
  } else {
    for (const series of config.series) {
      for (const run of runs(points, [series.key], step)) {
        const line = run.map((point) => x(point.ts).toFixed(1) + "," + y(point[series.key]).toFixed(1)).join(" ");
        if (series.kind === "area") {
          const area = x(run[0].ts).toFixed(1) + "," + base + " " + line + " " + x(run[run.length - 1].ts).toFixed(1) + "," + base;
          chart.append(svg("polygon", { points: area, class: "area-1" }));
          chart.append(svg("polyline", { points: line, class: "line-1" }));
        } else {
          chart.append(svg("polyline", { points: line, class: "line-2" }));
        }
      }
    }
  }
  host.replaceChildren(chart);
}

function memoryChart(labels) {
  const names = labels && labels.length > 0 ? labels : ["Usada"];
  return {
    title: "Memória", sub: names.join(", ") + ", empilhadas", top: memoryTotal,
    partialNote: (since) => "A composição da memória é gravada desde " + since + "; antes disso o período fica vazio.",
    format: (value) => (value === 0 ? "0" : bytes(value)),
    series: names.map((label, index) => ({
      key: "mem_p" + (index + 1) + "_bytes", label, kind: "band", band: index + 1, keyClass: SERIES[index],
    })),
  };
}

function renderCharts(data) {
  const range = document.getElementById("range").value;
  const points = data.series[range] || [];
  const configs = CHARTS.slice();
  configs.splice(1, 0, memoryChart(data.memory_labels));
  for (const old of document.getElementById("charts").children) {
    if (old.observer) old.observer.disconnect();
  }
  const cards = configs.map((config) => chartCard(points, config));
  replace("charts", ...cards);
  for (const card of cards) if (card.draw) card.draw();
}

/* ---------- C: tabelas das seções ---------- */

function checkSummary(checks, allClear) {
  const problems = checks.filter((item) => item.state === "problema").length;
  const warnings = checks.filter((item) => item.state === "atencao").length;
  const parts = [];
  if (problems > 0) parts.push(problems === 1 ? "1 problema" : problems + " problemas");
  if (warnings > 0) parts.push(warnings === 1 ? "1 ponto de atenção" : warnings + " pontos de atenção");
  return parts.length > 0 ? parts.join(" e ") + "." : allClear;
}

function checkTable(checks, emptyText) {
  return table(
    [{ label: "Estado", width: "120px" }, { label: "Item" }, { label: "Detalhe" }],
    checks.map((item) => [withDot(STATE_DOT[item.state], STATE_LABEL[item.state] || item.state), item.label, item.detail]),
    emptyText,
  );
}

function renderReadings(data) {
  const readings = data.readings || [];
  document.getElementById("readings-card").hidden = readings.length === 0;
  if (readings.length === 0) return;
  // Problemas e atenção primeiro: é o que pede ação.
  const order = { problema: 0, atencao: 1, desconhecido: 2, ok: 3 };
  const sorted = readings.slice().sort((a, b) => (order[a.state] ?? 4) - (order[b.state] ?? 4));
  document.getElementById("readings-summary").textContent = checkSummary(readings, "Nada fora do normal nos registros.");
  replace("readings", checkTable(sorted, ""));
}

function renderPosture(data) {
  const checks = data.posture;
  const problems = checks.filter((item) => item.state === "problema").length;
  document.getElementById("posture-summary").textContent = checkSummary(checks, "Tudo em ordem.");
  const nodes = [checkTable(checks, "Sem verificações.")];
  if (problems > 0 && data.platform === "macos") {
    const hint = el("p", "Para corrigir, rode no Mac: ", "note");
    hint.append(command("sudo bash ~/.local/share/macos-setup/harden.sh"));
    nodes.push(hint);
  }
  replace("posture", ...nodes);
}

function shortKey(fingerprint) {
  if (!fingerprint) return "";
  const body = fingerprint.replace(/^SHA256:/, "");
  return body.slice(0, 6) + "…" + body.slice(-4);
}

function groupEvents(events) {
  // Eventos vêm do mais novo para o mais antigo; seguidos e iguais viram uma linha.
  const groups = [];
  for (const event of events) {
    const id = [event.kind, event.user, event.ip, event.key].join("|");
    const current = groups[groups.length - 1];
    if (current && current.id === id) {
      current.count += 1;
      current.oldest = event.ts;
    } else {
      groups.push({ id, event, count: 1, newest: event.ts, oldest: event.ts });
    }
  }
  return groups;
}

function eventRows(groups) {
  return groups.map((group) => {
    const event = group.event;
    const when = group.count > 1 && clock(group.oldest) !== clock(group.newest)
      ? clock(group.oldest) + " – " + clock(group.newest)
      : clock(group.newest);
    const label = withDot(KIND_DOT[event.kind], KIND_LABEL[event.kind] || event.kind);
    if (group.count > 1) label.append(el("span", "× " + group.count, "count"));
    let keyCell = "";
    if (event.key) {
      keyCell = el("span", shortKey(event.key), "chip");
      keyCell.title = event.key;
    }
    return [when, label, event.user || "não informado", event.ip, keyCell];
  });
}

function renderSsh(data) {
  const ssh = data.ssh;
  const day = ssh.counts["24h"] || {};
  const refused = (day.falha || 0) + (day.usuario_invalido || 0) + (day.fechou_antes_de_autenticar || 0);
  let summary = "Últimas 24 horas: " + (day.aceito || 0) + " entradas e " + refused + " conexões sem login.";
  if (data.fail2ban) {
    summary += " fail2ban: " + data.fail2ban.currently_banned + " banido(s) agora, " + data.fail2ban.total_banned + " no total.";
  }
  if (ssh.error) summary += " Falha na leitura do log: " + ssh.error + ".";
  document.getElementById("ssh-summary").textContent = summary;

  const headers = [{ label: "Quando", className: "when" }, { label: "Evento" }, { label: "Usuário" },
    { label: "Origem" }, { label: "Chave" }];
  const groups = groupEvents(ssh.recent);
  const nodes = [table(headers, eventRows(groups.slice(0, EVENT_GROUPS_SHOWN)), "Nenhum evento registrado ainda.")];
  if (groups.length > EVENT_GROUPS_SHOWN) {
    const more = el("details", null, "more");
    more.open = moreOpen;
    more.addEventListener("toggle", () => { moreOpen = more.open; });
    more.append(el("summary", "Ver mais " + (groups.length - EVENT_GROUPS_SHOWN) + " linhas"),
      table(headers, eventRows(groups.slice(EVENT_GROUPS_SHOWN)), ""));
    nodes.push(more);
  }
  if (ssh.since_ts) nodes.push(el("p", "Registro desde " + clock(ssh.since_ts) + ".", "note"));
  replace("ssh-events", ...nodes);

  const who = ssh.who || [];
  const mostEntries = Math.max(1, ...who.map((item) => item.entries));
  replace("ssh-who", table(
    [{ label: "Usuário" }, { label: "Aparelho" }, { label: "Origens" }, { label: "Entradas", width: "110px" }],
    who.map((item) => {
      let device;
      if (item.method !== "publickey") {
        device = withDot("bad", item.method === "password" ? "senha" : item.method || "sem chave");
      } else if (item.device) {
        device = item.device;
      } else {
        device = el("span", shortKey(item.key), "chip");
        device.title = item.key;
      }
      const origins = el("span", null, "origins");
      for (const origin of item.origins) {
        const line = el("span", origin.ip);
        line.append(el("span", " " + origin.network + " · " + origin.entries, "count"));
        origins.append(line);
      }
      return [item.user, device, origins, proportion(item.entries, mostEntries, String(item.entries))];
    }),
    "Ninguém entrou nos últimos 7 dias.",
  ));

  const busiest = Math.max(1, ...ssh.origins_7d.map((origin) => Math.max(origin.accepted, origin.refused)));
  replace("ssh-origins", table(
    [{ label: "Endereço" }, { label: "Entradas", width: "130px" }, { label: "Sem login", width: "130px" },
     { label: "Último", numeric: true }],
    ssh.origins_7d.map((origin) => [
      origin.ip,
      proportion(origin.accepted, busiest, String(origin.accepted)),
      proportion(origin.refused, busiest, String(origin.refused)),
      clock(origin.last_ts),
    ]),
    "Nenhum evento registrado ainda.",
  ));
  replace("ssh-sessions", table(
    [{ label: "Usuário" }, { label: "Terminal" }, { label: "Origem" }],
    ssh.sessions.map((session) => [session.user, session.tty, session.origin || "local"]),
    "Nenhuma sessão aberta.",
  ));
}

function parseHighlight(text) {
  const match = text.match(/^(.*?)\s+([\d.,]+)\s*(B|KB|MB|GB|TB)$/);
  if (!match) return { label: text, bytes: null };
  return {
    label: ACARI_LABEL[match[1]] || match[1],
    bytes: parseFloat(match[2].replace(",", "")) * UNIT_BYTES[match[3]],
  };
}

function decimalBytes(value) {
  if (missing(value)) return "";
  return scaledParts(value, ["B", "KB", "MB", "GB", "TB"], 1000).join(" ");
}

function renderCleanup(data) {
  const acari = data.acari;
  document.getElementById("cleanup-card").hidden = !acari;
  // O card de portas vai para a coluna com menos linhas, para as duas terminarem perto.
  const ports = document.getElementById("ports-card");
  const leftRows = data.posture.length + (acari ? acari.highlights.length + 4 : 0);
  const rightRows = data.processes.length;
  const column = document.getElementById(leftRows <= rightRows ? "column-left" : "column-right");
  if (ports.parentElement !== column) column.append(ports);
  if (!acari) return;
  const items = acari.highlights.map(parseHighlight);
  const biggest = Math.max(1, ...items.map((item) => item.bytes || 0));
  const nodes = [table(
    [{ label: "Categoria" }, { label: "Tamanho", width: "140px" }],
    items.map((item) => [item.label, item.bytes === null ? "" : proportion(item.bytes, biggest, decimalBytes(item.bytes))]),
    "Nada relevante.",
  )];
  if (!missing(acari.regenerable_bytes)) {
    nodes.push(el("p", decimalBytes(acari.regenerable_bytes) + " são caches e builds que se refazem sozinhos.", "note"));
  }
  const hint = el("p", "Detalhe e limpeza: ", "note");
  hint.append(command("acari guide"));
  nodes.push(hint);
  replace("cleanup", ...nodes);
}

const RECENT_DEVICE_SECONDS = 24 * 3600;
const PORTS_SHOWN = 12;

// Bit "localmente administrado": celulares e notebooks usam MAC aleatório por rede.
function privateMac(mac) {
  const second = parseInt((mac || "").charAt(1), 16);
  return !Number.isNaN(second) && (second & 2) === 2;
}

function renderDevices(data) {
  const card = document.getElementById("devices-card");
  card.hidden = !data.devices;
  if (!data.devices) return;
  const headers = [{ label: "Aparelho" }, { label: "Estado" }, { label: "IP" }, { label: "MAC" }, { label: "Visto por último", numeric: true }];
  const toRow = (device) => [
    device.label || device.vendor || (privateMac(device.mac) ? "MAC privado (celular ou notebook)" : "desconhecido"),
    withDot(device.connected ? "ok" : "", device.connected ? "Conectado" : "Fora"),
    device.ip,
    el("span", device.mac, "chip"),
    clock(device.last_seen),
  ];
  const recent = data.devices.filter((device) => device.connected || data.generated_at - device.last_seen < RECENT_DEVICE_SECONDS);
  const older = data.devices.filter((device) => !recent.includes(device));
  const nodes = [table(headers, recent.map(toRow), "Nenhum aparelho visto nas últimas 24 horas.")];
  if (older.length > 0) {
    const more = el("details", null, "more");
    more.append(el("summary", "Ver mais " + older.length + " aparelhos vistos antes"), table(headers, older.map(toRow), ""));
    nodes.push(more);
  }
  replace("devices", ...nodes);
}

function renderLists(data) {
  document.getElementById("ports-sub").textContent = data.platform === "macos"
    ? "TCP fora do loopback. Processos de outros usuários e do sistema aparecem como serviço do sistema."
    : "TCP fora do loopback, com o processo dono de cada porta.";
  const busiest = Math.max(1, ...data.processes.map((item) => item.cpu_pct));
  replace("processes", table(
    [{ label: "Processo" }, { label: "CPU", width: "140px" }, { label: "Memória", numeric: true }],
    data.processes.map((item) => [item.name, proportion(item.cpu_pct, busiest, percent(item.cpu_pct)), percent(item.mem_pct)]),
    "Sem dados de processos.",
  ));
  const portHeaders = [{ label: "Porta", width: "90px" }, { label: "Endereço" }, { label: "Processo" }];
  const portRow = (item) => [String(item.port), item.address, item.process || "não identificado"];
  const ports = [table(portHeaders, data.listeners.slice(0, PORTS_SHOWN).map(portRow), "Nenhuma porta aberta fora do loopback.")];
  if (data.listeners.length > PORTS_SHOWN) {
    const more = el("details", null, "more");
    more.append(el("summary", "Ver mais " + (data.listeners.length - PORTS_SHOWN) + " portas"),
      table(portHeaders, data.listeners.slice(PORTS_SHOWN).map(portRow), ""));
    ports.push(more);
  }
  replace("listeners", ...ports);

}

/* ---------- ciclo ---------- */

function renderStatus() {
  const updated = document.getElementById("updated");
  const liveDot = document.getElementById("live-dot");
  if (lastLiveAt === null) {
    liveDot.className = "dot";
    updated.textContent = lastError
      ? "Não consegui ler o painel (" + lastError + "). Tentando de novo."
      : "Aguardando a primeira coleta";
    return;
  }
  const age = Math.max(0, Math.floor(Date.now() / 1000) - lastLiveAt);
  if (age > STALE_SECONDS) {
    liveDot.className = "dot bad";
    updated.textContent = "Parado há " + duration(age) + (lastError ? " (" + lastError + ")" : "");
  } else {
    liveDot.className = "dot ok";
    updated.textContent = "Ao vivo, há " + age + " s";
  }
}

function renderFull(data) {
  lastDocument = data;
  renderHost(data);
  if (lastLiveAt === null || data.generated_at > lastLiveAt) {
    lastLiveAt = data.generated_at;
    const live = Object.assign({}, data.now);
    if (lastLive) {
      for (const name of ["cpu_parts", "cpu_idle", "load5", "load15", "interface", "rx_total_bytes", "tx_total_bytes"]) {
        if (missing(live[name])) live[name] = lastLive[name];
      }
    }
    renderNow(live);
  } else if (lastLive) {
    renderNow(lastLive);
  }
  renderCharts(data);
  renderReadings(data);
  renderPosture(data);
  renderSsh(data);
  renderCleanup(data);
  renderDevices(data);
  renderLists(data);
}

async function load(path) {
  const response = await fetch(path, { cache: "no-store" });
  if (!response.ok) throw new Error("HTTP " + response.status);
  return response.json();
}

async function refreshLive() {
  try {
    const live = await load("/live.json");
    lastError = null;
    if (lastLiveAt === null || live.generated_at >= lastLiveAt) {
      lastLiveAt = live.generated_at;
      renderNow(live);
    }
  } catch (error) {
    lastError = error.message;
  }
  renderStatus();
}

async function refreshFull() {
  try {
    renderFull(await load("/current.json"));
    lastError = null;
  } catch (error) {
    lastError = error.message;
  }
  renderStatus();
}

document.getElementById("range").addEventListener("change", () => {
  if (lastDocument) renderCharts(lastDocument);
});
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) {
    refreshFull();
    refreshLive();
  }
});
refreshFull();
refreshLive();
setInterval(refreshLive, LIVE_MS);
setInterval(refreshFull, FULL_MS);
setInterval(renderStatus, 1000);
