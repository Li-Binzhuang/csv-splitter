#!/usr/bin/env node
// Official MCP SDK compatibility check: same flow as scripts/mcp_client.py.
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { existsSync } from "node:fs";
import { join } from "node:path";

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
const called = new Set();
const call = async (name, params) => {
  called.add(name);
  const result = await client.callTool({ name, arguments: params });
  const text = result.content?.[0]?.text ?? "";
  if (result.isError) throw new Error(text);
  return json(text);
};

const capabilities = client.getServerCapabilities();
console.log(`capabilities_tools=${capabilities?.tools !== undefined}`);
console.log(`capabilities_resources=${capabilities?.resources !== undefined}`);
const instructions = client.getInstructions() ?? "";
console.log(`instructions_chars=${instructions.length}`);
const resources = await client.listResources();
console.log(`resources=${resources.resources.length}`);
const resource = await client.readResource({ uri: "csvx://agent" });
const manifestText = resource.contents?.[0]?.text ?? "";
const manifest = json(manifestText);
console.log(`resource_protocol=${manifest.protocol ?? ""}`);
console.log(`resource_tools=${(manifest.tools ?? []).length}`);
const operationsResource = await client.readResource({ uri: "csvx://operations" });
const operations = json(operationsResource.contents?.[0]?.text ?? "{}");
console.log(`resource_operations=${(operations.operations ?? []).length}`);
console.log(`resource_error_codes=${(operations.errorCodes ?? []).length}`);
let unknownResource = false;
try {
  await client.readResource({ uri: "csvx://nope" });
} catch {
  unknownResource = true;
}
console.log(`unknown_resource_error=${unknownResource}`);
const tools = await client.listTools();
console.log(`sdk_tools=${tools.tools.length}`);
console.log(`sdk_annotated=${tools.tools.filter((tool) => tool.annotations).length}`);
const expectedTools = [
  "csvx_cache_clear", "csvx_cache_stats", "csvx_concat", "csvx_convert",
  "csvx_edit_apply", "csvx_edit_delete", "csvx_edit_preview", "csvx_edit_redo",
  "csvx_edit_set", "csvx_edit_undo", "csvx_filter", "csvx_gate", "csvx_job_cancel",
  "csvx_job_list", "csvx_job_status", "csvx_job_submit", "csvx_pipeline_run",
  "csvx_pipeline_status", "csvx_quality", "csvx_report", "csvx_schema", "csvx_search",
  "csvx_view",
];
const actualTools = tools.tools.map((tool) => tool.name).sort();
console.log(`sdk_names=${actualTools.length === expectedTools.length && actualTools.every((name, index) => name === expectedTools[index]) ? expectedTools.length : -1}`);

console.log(`schema_rows=${(await call("csvx_schema", { path: file })).rows}`);
console.log(`filter_matched=${(await call("csvx_filter", { path: file, where: "city=CDMX", limit: 10 })).matched}`);
const view = await call("csvx_view", { path: file, start: 0, count: 2 });
console.log(`view_rows=${view.rows.length}`);
await call("csvx_edit_set", { path: file, row: 1, col: "city", value: "SDK" });
await call("csvx_edit_undo", { path: file });
await call("csvx_edit_redo", { path: file });
await call("csvx_edit_preview", { path: file, ops: "set:1:city:SDK2" });
await call("csvx_edit_apply", { path: file, ops: "set:1:city:SDK2" });
await call("csvx_edit_delete", { path: file, row: 2, count: 1 });
await call("csvx_convert", { path: file, format: "tsv", out: "sdk.tsv" });
console.log("convert_ok=true");
if (existsSync(join(root, "plan.csv"))) {
  let pipelineProgress = 0;
  called.add("csvx_pipeline_run");
  const pipeline = await client.callTool(
    { name: "csvx_pipeline_run", arguments: { path: "plan.csv", contentCheck: true } },
    undefined,
    { onprogress: () => { pipelineProgress += 1; } },
  );
  if (pipeline.isError) throw new Error(pipeline.content?.[0]?.text ?? "pipeline failed");
  console.log(`pipeline_progress_events=${pipelineProgress}`);
  const status = await call("csvx_pipeline_status", {
    path: "plan.csv",
    verify: true,
    contentCheck: true,
  });
  console.log(`status_fresh=${(status.steps ?? []).filter((step) => step.fresh).length}`);
}
if (existsSync(join(root, "orders.csv.gz"))) {
  const gzip = await call("csvx_search", {
    path: "orders.csv.gz",
    query: "CDMX",
    materialize: true,
  });
  console.log(`gzip_matched=${gzip.matched ?? 0}`);
  await call("csvx_search", { path: "orders.csv.gz", query: "CDMX", materialize: true });
  const cache = await call("csvx_cache_stats", {});
  console.log(`cache_hits=${cache.hits}`);
  console.log(`cache_entries=${cache.entries}`);
  const cleared = await call("csvx_cache_clear", {});
  console.log(`cache_removed=${cleared.removed}`);
  console.log(`cache_stale=${cleared.staleRemoved}`);
  const after = await call("csvx_cache_stats", {});
  console.log(`cache_after=${after.entries}`);
}
await call("csvx_concat", { paths: "orders.csv,filtered.csv", out: "joined.csv" });
const gate = await call("csvx_gate", { path: file, minRows: 1, unique: "id" });
console.log(`gate_rows=${gate.rows}`);
const quality = await call("csvx_quality", { path: file, key: "id", group: "city" });
console.log(`quality_keys=${(quality.candidateKeys ?? []).length}`);
const report = await call("csvx_report", { path: file, specText: "title,SDK report\nsummary,", format: "md" });
console.log(`report_has_title=${String(report).includes("# SDK report")}`);
const rejected = await client.callTool({
  name: "csvx_job_submit",
  arguments: { tool: "csvx_cache_stats", arguments: {} },
});
console.log(`job_reject_cache=${rejected.isError === true}`);
console.log(`job_reject_code=${rejected.structuredContent?.code ?? ""}`);
const job = await call("csvx_job_submit", {
  tool: "csvx_quality",
  arguments: { path: file, key: "id", group: "city" },
});
let progressEvents = 0;
called.add("csvx_job_status");
const waited = await client.callTool(
  { name: "csvx_job_status", arguments: { id: job.job, wait: 2 } },
  undefined,
  { onprogress: () => { progressEvents += 1; } },
);
const waitedText = waited.content?.[0]?.text ?? "";
let status = json(waitedText);
console.log(`progress_events=${progressEvents}`);
console.log(`structured_ok=${waited.structuredContent?.ok === true}`);
for (let i = 0; i < 100 && status.state === "running"; i += 1) {
  await new Promise((resolve) => setTimeout(resolve, 100));
  status = await call("csvx_job_status", { id: job.job });
}
console.log(`job=${status.state}`);
const listed = await call("csvx_job_list", {});
console.log(`job_listed=${listed.jobs.some((entry) => entry.job === job.job)}`);
const cancelled = await call("csvx_job_cancel", { id: job.job });
console.log(`cancel_state=${cancelled.state}`);
const unknown = await client.callTool({ name: "csvx_nope", arguments: {} });
console.log(`unknown_tool_error=${unknown.isError === true}`);
console.log(`unknown_code=${unknown.structuredContent?.code ?? ""}`);
const noRequired = new Set(["csvx_cache_clear", "csvx_cache_stats", "csvx_job_list"]);
let requiredRejections = 0;
for (const name of expectedTools) {
  if (noRequired.has(name)) continue;
  const result = await client.callTool({ name, arguments: {} });
  if (result.isError) requiredRejections += 1;
}
console.log(`required_rejections=${requiredRejections}`);
const badConvert = await client.callTool({
  name: "csvx_convert",
  arguments: { path: file, format: "bogus", out: "bad.csv" },
});
const badReport = await client.callTool({
  name: "csvx_report",
  arguments: { path: file, format: "bogus", specText: "title,SDK\nsummary,rows" },
});
console.log(`enum_rejections=${(badConvert.isError === true ? 1 : 0) + (badReport.isError === true ? 1 : 0)}`);
const badCount = await client.callTool({
  name: "csvx_view",
  arguments: { path: file, count: 0 },
});
const badRow = await client.callTool({
  name: "csvx_edit_set",
  arguments: { path: file, row: 0, col: "city", value: "X" },
});
const badCountHigh = await client.callTool({
  name: "csvx_view",
  arguments: { path: file, count: 1001 },
});
const badFilterLimit = await client.callTool({
  name: "csvx_filter",
  arguments: { path: file, where: "city=CDMX", limit: 1001 },
});
const badSearchLimit = await client.callTool({
  name: "csvx_search",
  arguments: { path: file, query: "CDMX", limit: 1001 },
});
const badJobs = await client.callTool({
  name: "csvx_pipeline_run",
  arguments: { path: "plan.csv", jobs: 65 },
});
const badWait = await client.callTool({
  name: "csvx_job_status",
  arguments: { id: job.job, wait: 31 },
});
const badGate = await client.callTool({
  name: "csvx_gate",
  arguments: { path: file, minRows: -1 },
});
const rangeResults = [badCount, badRow, badCountHigh, badFilterLimit, badSearchLimit, badJobs, badWait, badGate];
console.log(`range_rejections=${rangeResults.filter((result) => result.isError === true).length}`);
const missingTools = expectedTools.filter((name) => !called.has(name));
console.log(`sdk_calls=${expectedTools.length - missingTools.length}`);
console.log(`sdk_missing=${missingTools.join(",")}`);
await client.close();
process.exit(status.state === "done" ? 0 : 1);
