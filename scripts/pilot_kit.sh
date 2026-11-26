#!/usr/bin/env bash
# Create a local, synthetic design-partner kickoff kit. No network, no customer data.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${CSVX_PILOT_BINARY:-target/release/csvx}"
[[ -x "$BIN" ]] || BIN=target/debug/csvx
[[ -x "$BIN" ]] || { echo "pilot_kit=fail no csvx binary" >&2; exit 2; }
OUT="${CSVX_PILOT_OUT:-pilot-kit}"
[[ -e "$OUT" ]] && { echo "error: $OUT already exists; choose another --out" >&2; exit 2; }

"$BIN" demo -o "$OUT/demo" >/dev/null
SAMPLE="${CSVX_PILOT_FILE:-$OUT/demo/orders.csv}"
[[ -s "$SAMPLE" ]] || { echo "pilot_kit=fail sample missing: $SAMPLE" >&2; exit 1; }
python3 scripts/pilot_report.py \
  --binary "$BIN" \
  --file "$SAMPLE" \
  --where city=CDMX \
  --key id \
  --group city \
  --out "$OUT/pilot-report.md" >/dev/null
python3 scripts/pilot_intake.py --sample "$SAMPLE" --out "$OUT/intake" --max-gb "${CSVX_PILOT_MAX_GB:-20}" >/dev/null
VERSION="$("$BIN" --version)"
python3 - "$OUT" "$VERSION" "$SAMPLE" <<'PY'
import datetime
import json
import pathlib
import sys
out = pathlib.Path(sys.argv[1])
kickoff = {
    "generatedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "binaryVersion": sys.argv[2],
    "sample": str(pathlib.Path(sys.argv[3]).resolve()),
    "report": str((out / "pilot-report.md").resolve()),
    "intake": str((out / "intake/intake.json").resolve()),
    "checklist": [
        {"id": "scope", "status": "partner", "item": "one concrete question and acceptance metric"},
        {"id": "sample", "status": "partner", "item": "de-identified 1-20 GB slice or 1 GB first"},
        {"id": "boundary", "status": "ready", "item": "local-only; no telemetry; root-scoped access"},
        {"id": "baseline", "status": "ready", "item": "synthetic demo report generated"},
        {"id": "decision", "status": "human", "item": "go/no-go after 1 week; pricing is separate"},
    ],
    "limits": ["no SaaS", "no multi-user auth", "single local user"],
}
(out / "kickoff.json").write_text(json.dumps(kickoff, indent=2) + "\n")
md = [
    "# csvx design-partner kickoff",
    "",
    f"- generated: {kickoff['generatedAt']}",
    f"- binary: `{kickoff['binaryVersion']}`",
    f"- sample: `{kickoff['sample']}`",
    f"- report: `{kickoff['report']}`",
    f"- intake: `{kickoff['intake']}`",
    "",
    "## Checklist",
    "",
]
for item in kickoff["checklist"]:
    md.append(f"- [{item['status']}] {item['item']}")
md += [
    "",
    "## Boundary",
    "",
    "Local-only, no telemetry, root-scoped file access. Pricing and contracts are not",
    "decided by this kit; a human must approve go/no-go after the trial.",
    "",
]
(out / "kickoff.md").write_text("\n".join(md))
print(f"pilot_kit=ok out={out}")
print(f"kickoff_json={out / 'kickoff.json'}")
print(f"kickoff_md={out / 'kickoff.md'}")
PY
