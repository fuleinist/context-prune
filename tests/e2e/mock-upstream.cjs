// Mock OpenAI-shaped upstream for e2e testing context-prune.
// Responds to any POST with a chat-completion-shaped JSON body whose content
// is highly compressible; echoes received body size in a header.
const http = require("http");

const PORT = process.env.MOCK_PORT || 9999;

const server = http.createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    if (req.url === "/v1/models") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ data: [{ id: "mock-model-1" }] }));
      return;
    }
    const content = "tool output line\n".repeat(400) + "RESULT: 42\n";
    const payload = {
      id: "chatcmpl-mock",
      object: "chat.completion",
      choices: [
        {
          index: 0,
          message: { role: "assistant", content },
          finish_reason: "stop",
        },
      ],
    };
    const out = JSON.stringify(payload);
    res.writeHead(200, {
      "content-type": "application/json",
      "x-received-bytes": String(Buffer.byteLength(body)),
    });
    res.end(out);
  });
});

server.listen(PORT, () => console.log(`mock upstream on ${PORT}`));
