"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");

const script = fs.readFileSync("www/app.js", "utf8");
const start = script.indexOf("function renderHost(data) {");
const end = script.indexOf("\n/* ---------- A: gráficos", start);
assert.ok(start >= 0 && end > start, "renderHost não encontrado");

const nodes = {
  title: { textContent: "" },
  meta: {
    firstElementChild: { textContent: "status" },
    replaceChildren(...children) {
      this.children = children;
    },
  },
};
const context = {
  document: {
    title: "",
    getElementById(id) {
      return nodes[id];
    },
  },
  el(tag, text) {
    return { tag, textContent: text };
  },
  duration() {
    return "1 h";
  },
};
vm.createContext(context);
vm.runInContext(script.slice(start, end), context);

function render(build) {
  context.renderHost({ host: { name: "Teste" }, generated_at: 1, build });
  return nodes.meta.children.map((item) => item.textContent);
}

assert.deepEqual(render({ version: "0.1.0", commit: "abcdef1234567890", dirty: false }), [
  "status", "host-status 0.1.0 (abcdef123456)",
]);
assert.match(render({ version: "0.1.0", commit: "abcdef123456", dirty: true })[1], /alterações locais/);
assert.match(render({ version: "0.1.0" })[1], /commit desconhecido.*estado do código desconhecido/);
assert.deepEqual(render(undefined), ["status"]);
