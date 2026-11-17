# Linux 侧部署形态（定案：两案并存，按角色选用）

背景：pong 是 **client** 侧命令桥（被管机上跑，Windows/Linux 均支持）。client **主动** `pong user@host` ssh 连接 **host（agent 部署处）**并建立反向隧道；host 上的 agent 经 sshd 侧动态分配端口访问 client。Linux 机器可能扮演两种角色，对应两种部署形态。

## 形态 A：Linux 作为 host（agent 部署处，跳板角色由 sshd 承担）

```
client(Windows/Linux, 跑 pong) ── pong user@host ──▶ Linux sshd(host)
                                                     └─ sshd 动态反向端口 ◀── host 上的 agent 直连
```

- **不需要在 Linux 额外部署任何东西**：sshd 是现成服务；pong 客户端在 client 侧发起连接
- 端口动态：sshd 侧 `tcpip_forward(127.0.0.1, 0)` 自选端口，connect 输出 `port=`，多 client 并存互不干扰
- host 侧 agent 拿到 `port=/pair=` 即可访问对应 client（回归脚本 tools/pong_regress.py 即此用法）
- 升级=只换 client 侧 pong 二进制；host 零维护

## 形态 B：Linux 作为 client（被管机本身跑 pong daemon）

```
Linux(被管机, 跑 pong daemon) ── pong user@host ──▶ host sshd ──▶ host agent 经隧道下发 exec/文件/串口请求
```

- daemon 部署在 Linux 被管机上：`pong user@host` 常驻（连接模型与 Windows client 完全同构）
- 常驻两种方式：
  - **user 级 systemd**（推荐，无需 sudo）：`cp tools/pong.service ~/.config/systemd/user/ && systemctl --user enable --now pong`（tools/pong.service 已备）
  - 前台/计划任务：`pong --daemon-run` 或登录时跑 `pong user@host`
- 平台差异：Linux 无 COM 串口（serial kind 返回明确报错）；路径分隔符/工作目录随平台

## 对比

| | A：Linux 做 host | B：Linux 做 client |
|---|---|---|
| 部署面 | 零（sshd 现成） | pong daemon + user systemd（可选） |
| agent 位置 | 本机 | 在 host |
| 维护 | 零 | 双端版本对齐 |
| 适用 | 管控中心 | Linux 机器本身是被管对象 |

两形态可并存：同一 Linux 机器既可作为 host 接纳其它 client 的连接，也可以自己跑 pong 作为 client 连别的 host。
