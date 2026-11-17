#!/usr/bin/env node
// pong TCP 客户端（host 侧）:：写一行 JSON 到 127.0.0.1:7101，收一行 JSON 结果（embedded ssh 反向隧道通到 Windows）
// 用法：node agent-send-tcp.mjs '<任务json>' [超时秒]
import net from "node:net";

const [, , taskArg, timeoutSec = "30"] = process.argv;
if (!taskArg) { console.error("用法: node agent-send-tcp.mjs '<json>' [超时秒]"); process.exit(2); }
const task = JSON.parse(taskArg);
if (!task.id) { task.id = "t" + Date.now().toString(36); }

const t0 = Date.now();
const sock = net.connect({ host: "127.0.0.1", port: 7101 });
// 注意：不能设 socket 空闲 timeout（serial 类任务 10s+ 无回包会被掐死连接→回包丢失，实测 5s 空闲即断）
// 连接建立超时由下方总体超时兜底
let buf = "";
let done = false;
sock.on("connect", () => { sock.write(JSON.stringify(task) + "\n"); });
sock.on("data", (d) => {
  buf += d.toString("utf8");
  const i = buf.indexOf("\n");
  if (i < 0) return;
  const line = buf.slice(0, i);
  try {
    const r = JSON.parse(line);
    if (r.id !== task.id) return;   // 串线响应忽略
    console.log(line);
  } catch { return; }
  done = true; sock.end();
});
sock.on("error", (e) => { console.error("[tcp] " + e.message + "（client 侧 pong 未跑或隧道未建立）"); process.exit(1); });
// sock.on("timeout") 已移除：空闲超时会掐死长任务（serial）的连接
setTimeout(() => { if (!done) { console.error("[tcp] 超时 " + timeoutSec + "s"); process.exit(1); } }, Number(timeoutSec) * 1000);
