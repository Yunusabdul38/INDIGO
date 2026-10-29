// Drives index.html in headless Chrome over the DevTools protocol (no npm
// packages: Node's own fetch and WebSocket). One action per run; the Chrome
// profile in PROFILE keeps IndexedDB between runs, as a browser would.
//
//   node check.mjs <base url> <profile dir> <words> <action> [arg] [arg2]
//   KILL_AFTER_MS=1500 node check.mjs ...   kills Chrome mid-action (a closed tab)
//   NETWORK=regtest node check.mjs ...       the local regtest stack (default: signet)
import { spawn } from "node:child_process";

const [base, profile, words, action, arg = "", arg2 = ""] = process.argv.slice(2);
const chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const port = 9300 + Math.floor(Math.random() * 500);
const proc = spawn(chrome, [
  "--headless=new", `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  "--no-first-run", "--no-default-browser-check", "about:blank",
], { stdio: "ignore" });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function devtools() {
  for (let i = 0; i < 100; i++) {
    try {
      const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
      const page = pages.find((p) => p.type === "page");
      if (page) return page.webSocketDebuggerUrl;
    } catch {}
    await sleep(100);
  }
  throw new Error("Chrome did not start");
}

const ws = new WebSocket(await devtools());
await new Promise((r) => ws.addEventListener("open", r, { once: true }));
let id = 0;
const pending = new Map();
ws.addEventListener("message", (m) => {
  const msg = JSON.parse(m.data);
  if (pending.has(msg.id)) { pending.get(msg.id)(msg.result); pending.delete(msg.id); }
});
const call = (method, params = {}) => new Promise((r) => {
  pending.set(++id, r);
  ws.send(JSON.stringify({ id, method, params }));
});

// The page's console and uncaught errors, for when an action hangs.
ws.addEventListener("message", (m) => {
  const msg = JSON.parse(m.data);
  if (msg.method === "Runtime.consoleAPICalled") {
    console.error("console:", msg.params.args.map((a) => a.value ?? a.description).join(" ").slice(0, 400));
  } else if (msg.method === "Runtime.exceptionThrown") {
    console.error("exception:", (msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text).slice(0, 6000));
  }
});
await call("Runtime.enable");

const q = new URLSearchParams({ network: process.env.NETWORK || "signet", words, action, arg, arg2 });
await call("Page.navigate", { url: `${base}/index.html?${q}` });

if (process.env.KILL_AFTER_MS) {
  await sleep(Number(process.env.KILL_AFTER_MS));
  proc.kill("SIGKILL");
  console.log("KILLED");
  process.exit(0);
}

const deadline = Date.now() + 5 * 60 * 1000;
let out = "running…";
let shown = false;
while (Date.now() < deadline) {
  const r = await call("Runtime.evaluate", { expression: "document.getElementById('out')?.textContent ?? ''" });
  out = r?.result?.value ?? "";
  if (out.startsWith("OK") || out.startsWith("FAIL")) break;
  // A receive-wait shows its invoice before it finishes: pass it on once.
  if (out.startsWith("INVOICE ") && !shown) {
    console.log(out);
    shown = true;
  }
  await sleep(500);
}
console.log(out);
await call("Browser.close");
proc.kill();
process.exit(out.startsWith("OK") ? 0 : 1);
