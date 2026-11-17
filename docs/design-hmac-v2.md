# HMAC v2 请求认证设计（外审 2 轮定型，待排期实施）

## 目标

防 token 重放与被动窃听重放；**最大增益=token 不再明文上线**（v1 每请求带 token，v2 token 只作 HMAC key 不出网）。

## 协议

请求顶层新增字段（与 v1 的 `token` 字段互斥）：

```
ts:        unix 秒（发送时刻）
nonce:     16B 随机 hex（32 字符）
body_hash: SHA256(请求体原始字节) hex；空 body 用空串 hash
sig:       hex(HMAC-SHA256(key=token, msg="v2|" + kind + "|" + body_hash + "|" + ts + "|" + nonce))
```

（kind 即请求路径——本协议单端点无 path，用 kind 字段区分类别；method 恒为 POST 行协议）

## 验证流程（服务端）

1. token.dat 存在 → 必须 v2（无 sig 拒绝，防止"签名可选"形同虚设）
2. `|now - ts| <= 120s`（同机/局域网；±120s 足够）
3. nonce 未见过（内存 `HashMap<nonce, expiry>`，过期清理；**不持久化**——重启窗口重放风险极低，文档声明）
4. 常量时间验证 sig（复用 auth.rs const_eq）
5. 插入 nonce（expiry=now+240s）

## 密钥与轮换

- key=token 本体（单 token + 版本前缀 `v2_<hex>` 平滑轮换；不做双 token）
- 生成 token 即 v2 前缀；v1 旧 token 自然淘汰
- v2 实施时一并做 DPAPI 加密 token.dat（CryptProtectData；文件头 magic 区分格式，旧格式自动迁移）

## 依赖

RustCrypto `hmac` + `sha2` crate（不手写 HMAC）。客户端（agent 侧）同步实现签名。

## 明确不做

- 不替代 SSH 传输加密（隧道本身已加密）
- 不持久化 nonce（重启窗口风险声明即可）
- 不做时钟回拨 max_ts_seen（±120s 窗口内风险可容忍，复杂度不值）

## 实施清单

- [ ] core/auth.rs：sig 验证函数（常量时间）
- [ ] core/protocol.rs：tcp 循环认证门分流 v1/v2（token 前缀判别）
- [ ] Cargo.toml：+hmac +sha2
- [ ] agent 侧（远程脚本/SDK）：签名生成
- [ ] 回归：+t24 签名请求放行 +t25 错签拒绝 +t26 重放拒绝
- [ ] DPAPI 迁移（可选子项）
