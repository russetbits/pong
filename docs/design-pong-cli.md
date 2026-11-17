# pong CLI 设计（2026-11-17 用户定案）

## 命名

项目/二进制/包名 **pong**（ping 的反义：不是 agent 去 ping 机器，而是机器把执行能力打回来）。

## 角色模型（不变）

client（Windows/Linux 被管机）**主动** ssh 连接 host（跳板），同时是 **cmd 接收端**：

```
pong user@buildhost          # client 主动 ssh（russh，复用现有连接层）
  └─ ssh -R：host 上开非特权端口 P，反向转发回 client 本机命令桥
stdout: port=39481 pair=XK7Q2A   # 用户抄给 agent
host agent → 连 host:39481（回程到 client 桥）→ 首条消息带 pair 握手 → 绑定连接
```

## CLI 子命令（单 bin pong.exe + pong-gui.exe 托盘分离）

| 命令 | 语义 |
|---|---|
| `pong user@host` | 连接（默认模式，adb 式 daemon：打印 port=/pair= 后立即返回，daemon 常驻）。`-p <port>`=ssh 端口；无参=读 ~/.ssh/config（同名主机条目），报错风格与 ssh 相似；默认尝试 ~/.ssh/id_* 密钥 |
| `pong help` | 面向用户帮助 |
| `pong status` | daemon pid + state JSON {port,local,ctl} |
| `pong port` | 列出活跃 forwards（无参 fallback=`port list`） |
| `pong port map <local>` | 加 forward（host sshd 侧自选远端口） |
| `pong port map <remote>:<local>` | 加 forward（固定远端口，被占报错） |
| `pong port unmap <remote>` | 摘除 forward |
| `pong repick` | 重生成**端口+配对码**（都换）；旧码旧端口立即失效 |
| `pong exit` | 停 daemon 并清理状态文件 |
| `pong version` | 版本/构建信息 |

## 配对码（动态握手）

- 连接建立后：pong 随机选**非特权端口**（>1024 且未被占）请求 sshd -R 转发 + 生成 6-8 字符 pair-code
- stdout 打印 `port=<P> pair=<CODE>`（唯一机器可读输出，用户复制给 agent）
- **会话握手**（不是每请求）：agent 首条消息带 pair → 验证通过绑定该连接；错码断连+限速（AUTH_FAILS 同类计数）
- ssh 通道加密+host 侧回环限定 ⇒ 无需每请求认证；**v2 HMAC 保留给托盘 GUI 模式**（不经 ssh 直连场景）
- `pong repick`：端口+码都换，旧对立即作废

## 配置瘦身

- serial 等功能参数**全部不进配置面**：agent 在 open/read/write 请求里传参（现有行协议 serial kind 已带参数——config.ini 删 serial_*，GUI serial 设置页删）
- config.ini 只留：连接相关（ssh key 路径例外项/known_hosts 行为）；CLI 纯 ssh 语义**不读 config.ini**
- 托盘 pong-gui 保留 config.ini 全功能

## 实施顺序

1. 改名 pong（P0 crate/bin 名 + P1 config 兼容查找 + P2 文档，方案见锚点改名预研）
2. CLI 入口层重写：clap 子命令 + ssh config 解析（~/.ssh/config Host 条目：HostName/User/Port/IdentityFile）+ ssh 式报错
3. 动态端口+配对码：R5 forwards 改造（sshd 分配或 pong 选非特权端口）+ 握手门 + repick
4. 回归脚本改走动态端口（探针先抓 stdout 的 port/pair）
5. pong-gui 托盘（GUI 侧仅改名+serial 页删除）

## 复用面

russh 连接层、R5 反向隧道、行协议 21 kind、执行引擎（CREATE_SUSPENDED/Job Object）、v2 HMAC（托盘模式）——全部不动；本次主要是 CLI 入口层重写 + 握手门新增。
