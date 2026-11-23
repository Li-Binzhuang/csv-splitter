#!/usr/bin/env node
// Official MCP SDK compatibility check: same flow as scripts/mcp_client.py.
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

function arg(name, fallback) {
  const at = process.argv.indexOf(`--${name}`);
  return at >= 0 ? process.argv[at + 1] : fallback;
}
const binary = arg("binary", process.env.CSVX_BIN || "target/release/csvx");
const root = arg("root", process.env.CSVX_ROOT || ".");
const file = arg("file", process.env.CSVX_FILE || "orders.csv");

const transport = new StdioClientTransport({
  command: binary,
  args: ["mcp", "--root", root],
  stderr: "pipe",
});
const client = new Client({ name: "csvx-sdk-demo", version: "1.0.0" }, { capabilities: {} });
await client.connect(transport);

const json = (text) => {
  try { return JSON.parse(text); } catch { return text; }
};
const call = async (name, params) => {
  const result = await client.callTool({ name, arguments: params });
  const text = result.content?.[0]?.text ?? "";
  if (result.isError) throw new Error(text);
  return json(text);
};

const tools = await client.listTools();
console.log(`sdk_tools=${tools.tools.length}`);
console.log(`schema_rows=${(await call("csvx_schema", { path: file })).rows}`);
console.log(`filter_matched=${(await call("csvx_filter", { path: file, where: "city=CDMX", limit: 10 })).matched}`);
const view = await call("csvx_view", { path: file, start: 0, count: 2 });
console.log(`view_rows=${view.rows.length}`);
await call("csvx_edit_set", { path: file, row: 1, col: "city", value: "SDK" });
await call("csvx_edit_undo", { path: file });
await call("csvx_convert", { path: file, format: "tsv", out: "sdk.tsv" });
console.log("convert_ok=true");
const job = await call("csvx_job_submit", {
  tool: "csvx_quality",
  arguments: { path: file, key: "id", group: "city" },
});
let progressEvents = 0;
const waited = await client.callTool(
  { name: "csvx_job_status", arguments: { id: job.job, wait: 2 } },
  undefined,
  { onprogress: () => { progressEvents += 1; } },
);
const waitedText = waited.content?.[0]?.text ?? "";
const status = json(waitedText);
console.log(`progress_events=${progressEvents}`);
console.log(`structured_ok=${waited.structuredContent?.ok === true}`);
for (let i = 0; i < 100 && status.state === "running"; i += 1) {
  await new Promise((resolve) => setTimeout(resolve, 100));
  const polled = await call("csvx_job_status", { id: job.job });
  if (polled.state !== "running") {
    console.log(`job=${polled.state}`);
    await client.close();
    process.exit(polled.state === "done" ? 0 : 1);
  }
}
if (status.state !== "running") {
  console.log(`job=${status.state}`);
  await client.close();
  process.exit(status.state === "done" ? 0 : 1);
}
throw new Error("job did not finish");
