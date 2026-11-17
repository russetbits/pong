#!/usr/bin/env python3
"""pong v2 HMAC 端到端冒烟（本机 daemon；Linux/Windows 通用）

前置：BIN 指向 pong-cli（Linux debug/release 或 Windows pong-cli.exe）
跑法：python3 v2_smoke.py
流程：手写 token.dat → 起 daemon（PONG_DIR 隔离）→ t24/t25/t26 三态 → 清理
"""
import subprocess, json, socket, sys, time, os, shutil, tempfile
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pong_sig import sign_full

BIN = os.environ.get("V2_BIN") or "/tmp/ad-target/release/pong-cli"
PORT = 7101

def req(line, timeout=6):
    s = socket.create_connection(("127.0.0.1", PORT), timeout=timeout)
    s.sendall((line + "\n").encode())
    s.settimeout(timeout)
    buf = b""
    try:
        while True:
            c = s.recv(4096)
            if not c: break
            buf += c
            if b"\n" in buf: break
    except socket.timeout:
        pass
    s.close()
    return json.loads(buf.decode().strip()) if buf.strip() else None

def main():
    tmp = tempfile.mkdtemp(prefix="ad-v2smoke-")
    env = {**os.environ, "PONG_DIR": tmp}
    tok_path = os.path.join(os.path.dirname(os.path.abspath(BIN)), "token.dat")
    had_tok = os.path.exists(tok_path)
    tok = os.urandom(32).hex()
    with open(tok_path, "w") as f:
        f.write(tok)
    logf = open(os.path.join(tmp, "daemon.log"), "w")
    p = subprocess.Popen([BIN, "--daemon-run"], env=env, stdout=logf, stderr=logf)
    time.sleep(2.5)
    try:
        ok = True
        r = req(sign_full({"id": "t24", "kind": "version"}, tok))
        t24 = (r or {}).get("ok") is True
        bad = json.loads(sign_full({"id": "t25", "kind": "version"}, tok)); bad["sig"] = "ff" * 32
        r = req(json.dumps(bad, sort_keys=True, separators=(",", ":")))
        t25 = (r or {}).get("ok") is False
        good = sign_full({"id": "t26", "kind": "version"}, tok)
        r1 = req(good); r2 = req(good)
        t26 = (r1 or {}).get("ok") is True and (r2 or {}).get("ok") is False
        print(f"t24 sig-accept  {'PASS' if t24 else 'FAIL'}")
        print(f"t25 bad-sig     {'PASS' if t25 else 'FAIL'}")
        print(f"t26 replay-rej  {'PASS' if t26 else 'FAIL'}")
        ok = t24 and t25 and t26
        print(f"\n{'3/3' if ok else 'FAILURES'} v2 smoke")
        return 0 if ok else 1
    finally:
        p.terminate()
        logf.close()
        if not had_tok:
            os.remove(tok_path)

if __name__ == "__main__":
    sys.exit(main())
