// e2e test: proxy end-to-end against a mock upstream.
// Usage (from repo root, binary already built):
//   node tests/e2e/mock-upstream.cjs        (terminal 1)
//   context-prune serve --upstream http://localhost:9999 --db tests/e2e/e2e.db
//   node tests/e2e/run.cjs                  (terminal 3)
const http = require("http");

const PROXY = process.env.PROXY_URL || "http://127.0.0.1:8787";
let failures = 0;

function post(path, body) {
  return new Promise((resolve, reject) => {
    const data = JSON.stringify(body);
    const req = http.request(
      `${PROXY}${path}`,
      { method: "POST", headers: { "content-type": "application/json", "content-length": Buffer.byteLength(data) } },
      (res) => {
        let out = "";
        res.on("data", (c) => (out += c));
        res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, body: out }));
      }
    );
    req.on("error", reject);
    req.end(data);
  });
}

function get(path) {
  return new Promise((resolve, reject) => {
    http.get(`${PROXY}${path}`, (res) => {
      let out = "";
      res.on("data", (c) => (out += c));
      res.on("end", () => resolve({ status: res.statusCode, body: out }));
    }).on("error", reject);
  });
}

function check(name, cond, detail) {
  if (cond) {
    console.log(`PASS  ${name}`);
  } else {
    failures++;
    console.log(`FAIL  ${name}${detail ? " — " + detail : ""}`);
  }
}

(async () => {
  // 1. Models passthrough (F1 acceptance)
  const models = await get("/v1/models");
  check("F1 models passthrough", models.status === 200 && models.body.includes("mock-model-1"), models.body.slice(0, 100));

  // 2. Chat completion with a fat tool result in the request (F2 request compression)
  const toolResult = "grep hit\n".repeat(500) + "real signal: needle found\n";
  const chat = await post("/v1/chat/completions", {
    model: "mock-model-1",
    messages: [
      { role: "user", content: "analyze" },
      { role: "tool", content: toolResult },
    ],
  });
  check("F1 chat completion proxied", chat.status === 200, `status ${chat.status}`);

  const receivedUpstream = parseInt(chat.headers["x-received-bytes"] || "0", 10);
  const originalSize = Buffer.byteLength(JSON.stringify({
    model: "mock-model-1",
    messages: [{ role: "user", content: "analyze" }, { role: "tool", content: toolResult }],
  }));
  check(
    "F2 request body compressed upstream",
    receivedUpstream > 0 && receivedUpstream < originalSize * 0.6,
    `upstream saw ${receivedUpstream}B vs original ${originalSize}B`
  );

  // 3. Response compression (mock returns 400x repeated lines)
  const parsed = JSON.parse(chat.body);
  const content = parsed.choices?.[0]?.message?.content || "";
  check("F2 response content compressed", content.includes("RESULT: 42") && content.includes("repeated x"), `len ${content.length}`);
  check("F5 response still valid JSON", typeof parsed === "object" && parsed.id === "chatcmpl-mock");

  // 4. Malformed JSON passes through unchanged (F5 safety)
  const bad = await new Promise((resolve, reject) => {
    const req = http.request(`${PROXY}/v1/echo`, { method: "POST", headers: { "content-type": "application/json" } }, (res) => {
      let out = "";
      res.on("data", (c) => (out += c));
      res.on("end", () => resolve({ status: res.statusCode }));
    });
    req.on("error", reject);
    req.end("this is not json {{{");
  });
  check("F5 malformed body does not 5xx", bad.status < 500, `status ${bad.status}`);

  // 5. Stats endpoint (F3 acceptance)
  const stats = await get("/stats");
  const s = JSON.parse(stats.body);
  check("F3 /stats endpoint", stats.status === 200 && s.requests >= 3, stats.body);
  check("F3 savings recorded", s.bytes_in > 0 && s.bytes_out >= 0, stats.body);

  console.log(failures === 0 ? "\nALL E2E CHECKS PASSED" : `\n${failures} CHECK(S) FAILED`);
  process.exit(failures === 0 ? 0 : 1);
})().catch((e) => {
  console.error("e2e error:", e.message);
  process.exit(2);
});
