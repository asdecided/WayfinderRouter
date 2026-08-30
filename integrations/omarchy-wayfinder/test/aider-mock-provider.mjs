import { createServer } from "node:http";
import { writeFileSync } from "node:fs";

const port = Number.parseInt(process.argv[2] ?? "", 10);
const evidencePath = process.argv[3];
const maxRequestBytes = 2 * 1024 * 1024;

if (!Number.isInteger(port) || port < 1024 || port > 65535 || !evidencePath) {
  process.stderr.write("usage: node aider-mock-provider.mjs PORT EVIDENCE_PATH\n");
  process.exit(2);
}

let requests = 0;
let editRequestSeen = false;
let streamRequested = false;

function persistEvidence() {
  writeFileSync(evidencePath, `${JSON.stringify({
    schema_version: 1,
    agent: "aider",
    client_version: "0.86.1",
    requests,
    edit_request_seen: editRequestSeen,
    stream_requested: streamRequested,
    final_marker: "WAYFINDER_AIDER_EDIT_OK",
  }, null, 2)}\n`, { mode: 0o600 });
}

function json(response, status, value) {
  const body = JSON.stringify(value);
  response.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(body),
  });
  response.end(body);
}

function sse(response, chunks) {
  response.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
  });
  for (const chunk of chunks) {
    response.write(`data: ${JSON.stringify(chunk)}\n\n`);
  }
  response.end("data: [DONE]\n\n");
}

const server = createServer((request, response) => {
  if (request.method === "GET" && request.url === "/healthz") {
    return json(response, 200, { status: "ok" });
  }
  if (request.method !== "POST" || request.url !== "/v1/chat/completions") {
    return json(response, 404, { error: { message: "not found" } });
  }

  let size = 0;
  const chunks = [];
  request.on("data", (chunk) => {
    size += chunk.length;
    if (size > maxRequestBytes) {
      request.destroy();
      return;
    }
    chunks.push(chunk);
  });
  request.on("end", () => {
    let body;
    try {
      body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    } catch {
      return json(response, 400, { error: { message: "invalid JSON" } });
    }
    if (body.model !== "smoke-model" || body.stream !== true) {
      return json(response, 400, { error: { message: "unexpected model or stream mode" } });
    }

    requests += 1;
    streamRequested = true;
    editRequestSeen = JSON.stringify(body.messages ?? []).includes("WAYFINDER_AIDER_EDIT_OK");
    persistEvidence();

    const content = [
      "smoke.txt",
      "<<<<<<< SEARCH",
      "before",
      "=======",
      "WAYFINDER_AIDER_EDIT_OK",
      ">>>>>>> REPLACE",
    ].join("\n");
    return sse(response, [
      {
        id: "chatcmpl-wayfinder-aider-smoke",
        object: "chat.completion.chunk",
        model: body.model,
        choices: [{
          index: 0,
          delta: { content },
          finish_reason: "stop",
        }],
        usage: { prompt_tokens: 32, completion_tokens: 12 },
      },
    ]);
  });
});

server.listen(port, "127.0.0.1", () => {
  process.stdout.write(`aider smoke provider listening on 127.0.0.1:${port}\n`);
});
