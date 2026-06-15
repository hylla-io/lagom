#!/usr/bin/env node
// Minimal, instant-start stdio MCP server for the lagom PoC: two tools (echo,
// secret) so lagom can drop one and pin an arg on the other. Connects in <1s so
// it beats claude -p's async (nonblocking) MCP attach race.
const rl = require("readline").createInterface({ input: process.stdin });
const send = (o) => process.stdout.write(JSON.stringify(o) + "\n");
rl.on("line", (line) => {
  let m;
  try { m = JSON.parse(line); } catch { return; }
  if (m.method === "initialize") {
    send({ jsonrpc: "2.0", id: m.id, result: { protocolVersion: "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "fast", version: "0" } } });
  } else if (m.method === "notifications/initialized") {
    // no response
  } else if (m.method === "tools/list") {
    send({ jsonrpc: "2.0", id: m.id, result: { tools: [
      { name: "echo", description: "Echo a message", inputSchema: { type: "object", properties: { message: { type: "string" }, token: { type: "string" } }, required: ["message", "token"] } },
      { name: "secret", description: "Reveal the secret", inputSchema: { type: "object", properties: { key: { type: "string" } }, required: ["key"] } },
    ] } });
  } else if (m.method === "tools/call") {
    const a = (m.params && m.params.arguments) || {};
    send({ jsonrpc: "2.0", id: m.id, result: { content: [{ type: "text", text: m.params.name + ":" + JSON.stringify(a) }] } });
  } else if (m.id !== undefined) {
    send({ jsonrpc: "2.0", id: m.id, error: { code: -32601, message: "method not found" } });
  }
});
