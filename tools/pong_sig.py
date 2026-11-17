#!/usr/bin/env python3
"""pong v2 请求签名参考实现（docs/design-hmac-v2.md）

用法：
    import pong_sig
    req = {"id": "1", "kind": "version"}
    pong_sig.sign(req, token)      # 原地补 ts/nonce/body_hash/sig 字段
    sock.sendall((json.dumps(req) + "\n").encode())

token 为 hex 字符串（token.dat 内容）；服务端以 token.dat 为 HMAC key 验证。
v2 生效后 token 本体不再随请求发送（req 里的 "token" 字段勿带）。
"""
import hashlib, hmac as _hmac, json, os, time

def sign(req: dict, token: str) -> dict:
    body = json.dumps({k: v for k, v in req.items() if k not in ("ts", "nonce", "body_hash", "sig")}, sort_keys=True, separators=(",", ":"))
    ts = int(time.time())
    nonce = os.urandom(16).hex()
    body_hash = hashlib.sha256(body.encode()).hexdigest()
    msg = f"v2|{req.get('kind', '')}|{body_hash}|{ts}|{nonce}"
    sig = _hmac.new(token.encode(), msg.encode(), hashlib.sha256).hexdigest()
    # 服务端验证以"线上原始请求体"为 body——行协议下即整行 JSON；
    # 因此签名的 body 必须与最终发送字节一致：先构造完整字段集再序列化一次
    req.update({"ts": ts, "nonce": nonce, "body_hash": body_hash, "sig": sig})
    return req

def sign_full(req: dict, token: str) -> str:
    """签名并返回最终线上行（保证 body_hash 与发送字节一致）"""
    fields = {k: v for k, v in req.items() if k not in ("ts", "nonce", "body_hash", "sig")}
    ts = int(time.time())
    nonce = os.urandom(16).hex()
    body_hash = hashlib.sha256(json.dumps(fields, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    msg = f"v2|{req.get('kind', '')}|{body_hash}|{ts}|{nonce}"
    sig = _hmac.new(token.encode(), msg.encode(), hashlib.sha256).hexdigest()
    return json.dumps({**fields, "ts": ts, "nonce": nonce, "body_hash": body_hash, "sig": sig}, sort_keys=True, separators=(",", ":"))

if __name__ == "__main__":
    # 自测：签名→（模拟服务端）验签
    tok = "a" * 64
    line = sign_full({"id": "t", "kind": "version"}, tok)
    obj = json.loads(line)
    fields = {k: v for k, v in obj.items() if k not in ("ts", "nonce", "body_hash", "sig")}
    bh = hashlib.sha256(json.dumps(fields, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    msg = f"v2|{obj['kind']}|{bh}|{obj['ts']}|{obj['nonce']}"
    want = _hmac.new(tok.encode(), msg.encode(), hashlib.sha256).hexdigest()
    assert want == obj["sig"], "self-test FAIL"
    print("self-test OK:", line[:80], "...")
