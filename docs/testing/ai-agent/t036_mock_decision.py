#!/usr/bin/env python3
"""T036 System One 决策协议本地 mock（受控，不联网）。

只服务 `POST /systemone`：把请求中的 Choice 问题第一个候选作为答案返回，
并回带 usage。用于黑盒验证「请求体形状 / 认证头 / Choice 解析 / usage 记账」，
不参与真实模型验收（R02 仍需用户真实决策模型）。

用法：python3 t036_mock_decision.py [端口]，默认 39110。
日志：/private/tmp/fluxdb-t036/decision-mock.log
"""
import json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 39110
LOG = os.environ.get("FLUXDB_T036_MOCK_LOG", "/private/tmp/fluxdb-t036/decision-mock.log")


def log(msg):
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    with open(LOG, "a") as f:
        f.write(msg + "\n")


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):  # silence
        pass

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(n) or b"{}"
        try:
            body = json.loads(raw)
        except Exception:
            body = {}
        auth = self.headers.get("Authorization", "")
        # 只记录是否带认证头，绝不打印凭据明文。
        log("=== %s auth=%s" % (self.path, "bearer" if auth.startswith("Bearer ") else "none"))
        log("  body=%s" % json.dumps(body, ensure_ascii=False)[:400])

        questions = body.get("questions", {}) or {}
        qid, question = next(iter(questions.items()), ("decision", {"criteria": {"candidate-a": "x"}}))
        criteria = question.get("criteria") or {}
        keys = list(criteria.keys()) or ["candidate-a"]
        chosen = keys[0]
        answer = {
            "type": "choice",
            "choice": chosen,
            "probabilities": {k: (0.9 if k == chosen else round(0.1 / max(len(keys) - 1, 1), 4)) for k in keys},
            "confidence": 0.9,
        }
        payload = {"answers": {qid: answer}, "usage": {"input_tokens": 33, "output_tokens": 5}}
        data = json.dumps(payload, ensure_ascii=False).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
        log("  -> choice=%s" % chosen)

    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"ok":true}')


if __name__ == "__main__":
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    print("System One 决策 mock 监听 http://127.0.0.1:%d/systemone" % PORT)
    print("日志：%s" % LOG)
    HTTPServer(("127.0.0.1", PORT), H).serve_forever()
