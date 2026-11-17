# pong

pong 是运行在 client（被控机）上的命令桥守护：它在 client 上执行 `pong user@host` 主动 ssh 连到 host 并建立反向隧道。运行在 host 上的 agent 随后能访问**任意**已连接的 client，执行其上的程序与命令（exec / 文件读写 / 配置 / 串口等）。

**定位**：agent 能直接 ssh 到被控机的场景不需要本程序（直接 ssh 即可）。pong 面向 **host 无法直接 ssh 到 client** 的网络环境——client 出站单向可达 host（企业内网单向隔离、NAT 后的办公机等），反向隧道让 agent 的命令"打回来"（ping 的反义）。

## 架构

```
host (agent 运行处)                    client (pong daemon)
      │ sshd 监听 22                         │
      │◀─── pong user@host ── ssh 连接 ──────│ (client 上执行)
      │──── sshd 侧反向端口 <port> ◀─────────│ 桥 TCP (127.0.0.1:local)
      │                                      │   ▲
      │── 连 host 的 <port>（经隧道到 client 桥）▶┼───┘
      │           行协议 JSON 请求/响应        │
```

- 每台 client 的 `pong user@host` 起 daemon（adb 式）：解析 ssh（-p / ssh config）、建立反向隧道、打印 `port=/pair=` 后立即返回
- 动态端口：桥 `bind 127.0.0.1:0` 自选 + host sshd 侧 `tcpip_forward(port 0)` 自选（无端口冲突；多 client 各占各的端口互不干扰）
- 动态配对码：daemon 每次连接生成随机 6 位 pair，agent 首条消息须带 `{"pair": "..."}`（错码 3 次断连）；`repick` 重生成、旧码即时失效
- 纯内存凭据：pair/token 不落盘；host key 采用 TOFU（首次连接记录，之后校验）

## 使用

```
pong [-p PORT] user@host   # client 连接 host（无参读 ~/.ssh/config，报错同 ssh(1)）
pong help                  # 帮助
pong status                # daemon pid + state JSON {port,local,ctl}
pong port                  # 列出 forwards（fallback=list）
pong port map <local>              # 加 forward（host sshd 自选远端口）
pong port map <remote>:<local>     # 加 forward（固定远端口，被占则报错）
pong port unmap <remote>           # 摘除 forward
pong repick                # 重生成 port+pair（旧 pair 失效）
pong exit                  # 停 daemon 并清理状态文件
pong version               # 版本/构建信息
```

host 侧 agent 连入某 client：

```python
import socket, json
s = socket.create_connection(("127.0.0.1", port))       # port= 该 client connect 输出
s.sendall(json.dumps({"id":"h1","kind":"ping","pair":pair}).encode()+b"\n")
print(s.makefile().readline())                           # {"id":"h1","ok":true,"pair_ack":true}
s.sendall(json.dumps({"id":"c1","kind":"exec","args":["echo hi"]}).encode()+b"\n")   # 命令在 client 上执行
print(s.makefile().readline())                           # {"ok":true,"output":"hi",...}
```

## 请求 kind 一览

| kind | 说明 |
|---|---|
| ping / version | 探活 / 版本 |
| exec / adb | 执行命令（args[0] 含空格=整条命令行按首词拆分；timeout_ms 缺省 60s，Job 全树杀） |
| ls / stat / chdir | 目录列取 / 属性 / 切 cwd |
| read / write / fread / fwrite | 文件读写（fread/fwrite 分块+crc 校验；块 ≤700KB 原始） |
| config get / set | 配置读取 / 热生效修改（forwards 为 protected 键，agent 禁改） |
| serial / serial_open / serial_write / serial_close | 串口（参数在请求中传入，不进配置面） |

单行请求上限 1.1MB（防内存 DoS）；大流量走 fread/fwrite 分块。

## 构建

client 实现支持 Windows / Linux。

```bash
# 交叉编译 Windows 产物（在 host 等 Linux 机器上）
cargo build --release --target x86_64-pc-windows-gnu --no-default-features --features cli --bins
# 本机原生构建
cargo build --no-default-features --features cli --bins
```

产物：`target/.../release/pong.exe`（Windows 交叉）/ `pong`（Linux 原生）。GUI 版（pong-gui，eframe 托盘）随 default features 构建。

## 状态与数据

- 状态文件：temp 目录（Windows `%TEMP%\pong`，Linux `/tmp/pong`）下 `bridge.state`（`{"state":"up","port":R,"local":L,"ctl":C}`）+ `bridge.pid`
- daemon 日志：temp 目录 `log/bridge.log`（+ `daemon.err`）
- pair/token 纯内存，永不落盘

## 回归

回归脚本在 **host** 上执行（连接目标 client）：

```
# host 上（已 pong user@host 连上某 client 后）
python tools/pong_regress.py conn.txt     # conn.txt = "pong user@host" 的输出，23 例
# host=Linux 且 client=本机（本机 daemon 冒烟）
PONG_REGR_DIR=/tmp/pong python3 tools/pong_regress.py <connect输出文件>
```

## 文档

- docs/design-pong-cli.md — CLI/协议设计
- docs/design-hmac-v2.md — v2 HMAC 请求签名
