import { createServer } from "node:http";
import { writeFileSync } from "node:fs";

const port = Number.parseInt(process.argv[2] ?? "", 10);
const evidencePath = process.argv[3];
const contract = process.argv[4];
const contracts = {
  "codex-0.149.0": {
    agent: "codex",
    version: "0.149.0",
    preferredTool: "exec_command",
    argumentName: "cmd",
    toolMarker: "WAYFINDER_TOOL_ROUNDTRIP",
    finalMarker: "WAYFINDER_CODEX_SMOKE_OK",
  },
  "claude-code-2.1.241": {
    agent: "claude-code",
    version: "2.1.241",
    preferredTool: "Bash",
    argumentName: "command",
    toolMarker: "WAYFINDER_CLAUDE_TOOL_ROUNDTRIP",
    finalMarker: "WAYFINDER_CLAUDE_SMOKE_OK",
  },
};
const selected = contracts[contract];
const maxRequestBytes = 2 * 1024 * 1024;
const toolCallId = "call_wayfinder_smoke";

if (!Number.isInteger(port) || port < 1024 || port > 65535 || !evidencePath || !selected) {
  process.stderr.write(
    "usage: node coding-agent-mock-provider.mjs PORT EVIDENCE_PATH codex-0.149.0|claude-code-2.1.241\n",
  );
  process.exit(2);
}

let requestCount = 0;
let translatedToolName = "";

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

function functionTools(body) {
  return (Array.isArray(body.tools) ? body.tools : [])
    .filter((tool) => tool?.type === "function" && tool.function?.name)
    .map((tool) => tool.function);
}

function requestedTool(body) {
  const tools = functionTools(body);
  return tools.find((tool) => tool.name === selected.preferredTool)
    ?? tools.find((tool) => tool.name.toLowerCase() === selected.preferredTool.toLowerCase())
    ?? tools.find((tool) => tool.parameters?.properties?.[selected.argumentName]);
}

function toolResult(body) {
  return (Array.isArray(body.messages) ? body.messages : [])
    .find((message) => message?.role === "tool" && message.tool_call_id === toolCallId);
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

    requestCount += 1;
    const result = toolResult(body);
    if (!result) {
      const tool = requestedTool(body);
      if (!tool) {
        return json(response, 400, {
          error: { message: `${selected.preferredTool} was not translated for ${selected.agent}` },
        });
      }
      translatedToolName = tool.name;
      return sse(response, [
        {
          id: "chatcmpl-wayfinder-smoke-1",
          object: "chat.completion.chunk",
          model: "smoke-model",
          choices: [{
            index: 0,
            delta: {
              tool_calls: [{
                index: 0,
                id: toolCallId,
                type: "function",
                function: {
                  name: tool.name,
                  arguments: JSON.stringify({
                    [selected.argumentName]: `printf ${selected.toolMarker}`,
                  }),
                },
              }],
            },
            finish_reason: "tool_calls",
          }],
          usage: { prompt_tokens: 32, completion_tokens: 8 },
        },
      ]);
    }

    const content = typeof result.content === "string" ? result.content : JSON.stringify(result.content);
    if (!content.includes(selected.toolMarker)) {
      const preview = content.replaceAll(/\s+/g, " ").slice(0, 512);
      return json(response, 400, {
        error: {
          message: `tool result marker did not return through Wayfinder; received ${JSON.stringify(preview)}`,
        },
      });
    }
    writeFileSync(evidencePath, `${JSON.stringify({
      schema_version: 1,
      agent: selected.agent,
      client_version: selected.version,
      requests: requestCount,
      tool_name: translatedToolName,
      tool_call_id: toolCallId,
      tool_output_seen: true,
      final_marker: selected.finalMarker,
    }, null, 2)}\n`, { mode: 0o600 });
    return sse(response, [
      {
        id: "chatcmpl-wayfinder-smoke-2",
        object: "chat.completion.chunk",
        model: "smoke-model",
        choices: [{
          index: 0,
          delta: { content: selected.finalMarker },
          finish_reason: "stop",
        }],
        usage: { prompt_tokens: 48, completion_tokens: 6 },
      },
    ]);
  });
});

server.listen(port, "127.0.0.1", () => {
  process.stdout.write(`${selected.agent} smoke provider listening on 127.0.0.1:${port}\n`);
});
