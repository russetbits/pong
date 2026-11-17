#!/usr/bin/env node
// pong 批量客户端（host 侧）：一次 TCP 连接顺序发多条命令（同 id 配对），比逐条连接省开销
// 用法：node agent-batch-tcp.mjs < tasks.jsonl   （每行一个任务 JSON）
import net from "node:net";
import { createInterface } from "node:readline";

const tasks = [];
const rl = createInterface({ input: process.stdin });
for await (const line of rl) {
  const t = line.trim();
  if (t) tasks.push(JSON.parse(t));
}
if (!tasks.length) { console.error("stdin 无任务（每行一个 JSON）"); process.exit(2); }

const sock = net.connect({ host: "127.0.0.1", port: 7101 });  // 不设空闲 timeout（长任务 serial 会 5s 被掐）
let buf = "";
let qi = 0;               // 已发送游标
let done = 0;
const byId = new Map(tasks.map((t, i) => [t.id ?? "t" + i, i]));
sock.on("connect", () => {
  // 逐条发（服务端单线程顺序执行）；每收到一条再发下一条 = 简单流控
  sock.write(JSON.stringify(tasks[0]) + "\n"); qi = 1;
});
sock.on("data", (d) => {
  buf += d.toString("utf8");
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i); buf = buf.slice(i + 1);
    let r; try { r = JSON.parse(line); } catch { continue; }
    if (!byId.has(r.id)) continue;
    console.log(line); done++;
    if (qi < tasks.length) { sock.write(JSON.stringify(tasks[qi]) + "\n"); qi++; }
  }
  if (done === tasks.length) sock.end();
});
sock.on("error", (e) => { console.error("[tcp] " + e.message); process.exit(1); });
setTimeout(() => { if (done < tasks.length) { console.error("[tcp] 批量超时，完成 " + done + "/" + tasks.length); process.exit(1); } }, 120000);
