#!/usr/bin/env python3
"""T022 E2E 用本地 mock 模型：OpenAI Chat Completions SSE。
第 1 次请求 → 返回 context.read 工具调用（用它自己从 user 消息里解析出的 reference id）。
第 2 次请求（含 tool 回执）→ 返回正文，并把收到的工具回执原文回显（证明真实回执进入了下一轮）。"""
import json, re, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 39107
LOG = "/private/tmp/fluxdb-t022-shots/mock-model.log"

def log(msg):
    with open(LOG, "a") as f:
        f.write(msg + "\n")

class H(BaseHTTPRequestHandler):
    def log_message(self, *a):  # silence
        pass
    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(n) or b"{}")
        msgs = body.get("messages", [])
        log("=== request path=%s msgs=%d" % (self.path, len(msgs)))
        for m in msgs:
            log("  [%s] %s" % (m.get("role"), json.dumps(m.get("content"), ensure_ascii=False)[:400]))
        tool_msgs = [m for m in msgs if m.get("role") == "tool"]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        if not tool_msgs:
            # find a reference id from any user message (public context embeds the id)
            ref = None
            for m in msgs:
                c = m.get("content")
                if isinstance(c, str):
                    mm = re.search(r'"(selection|result_set|knowledge|saved_query|file):[^"]+"', c)
                    if mm:
                        ref = mm.group(0).strip('"'); break
            if ref is None:
                ref = "selection:missing"
            args = json.dumps({"reference_id": ref, "max_bytes": 4096})
            for frame in [
                {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"context_read","arguments":args}}]},"finish_reason":None}]},
                {"choices":[{"delta":{},"finish_reason":"tool_calls"}]},
            ]:
                self.wfile.write(("data: " + json.dumps(frame) + "\n\n").encode()); self.wfile.flush()
            log("  -> emitted tool_call context_read ref=%s" % ref)
        else:
            receipt = tool_msgs[-1].get("content") or ""
            text = "已通过 context.read 读取引用，回执片段：" + str(receipt)[:300]
            for frame in [
                {"choices":[{"delta":{"content":text},"finish_reason":None}]},
                {"choices":[{"delta":{},"finish_reason":"stop"}]},
            ]:
                self.wfile.write(("data: " + json.dumps(frame, ensure_ascii=False) + "\n\n").encode()); self.wfile.flush()
            log("  -> emitted final text, echo=%s" % text[:120])
        self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()
    def do_GET(self):
        self.send_response(200); self.send_header("Content-Type","application/json"); self.end_headers()
        self.wfile.write(json.dumps({"data":[{"id":"offline","object":"model"}]}).encode())

HTTPServer(("127.0.0.1", PORT), H).serve_forever()
