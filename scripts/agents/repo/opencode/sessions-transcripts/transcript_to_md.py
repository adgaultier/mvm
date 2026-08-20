#!/usr/bin/env python3
"""Turn an opencode session transcript JSON into a clean human-readable .md.

Strips all metadata (ids, timestamps, token counts, sessions, step markers,
reasoning); keeps only the actual conversation: role headers, verbatim text,
and tool calls with their inputs/outputs.

Usage:
    python3 transcript_to_md.py SESSION.json [OUTPUT.md]
"""
import json
import sys
from pathlib import Path


def render_message(msg, out):
    role = msg["info"]["role"]
    out.append(f"## {role}\n")
    for part in msg.get("parts", []):
        ptype = part.get("type")
        if ptype == "text":
            text = part["text"].strip()
            try:
                pretty = json.dumps(json.loads(text), indent=2)
            except (json.JSONDecodeError, TypeError):
                out.append(text)
            else:
                out.append("```json")
                for line in pretty.split("\n"):
                    out.append(f"    {line}")
                out.append("```")
            out.append("")
        elif ptype == "tool":
            tool = part.get("tool", "?")
            state = part.get("state", {})
            out.append(f"**tool: `{tool}`**")
            out.append("")
            if state.get("input") is not None:
                out.append(f"input: `{json.dumps(state['input'], separators=(',', ':'))}`")
                out.append("")
            status = state.get("status")
            if status and status != "completed":
                out.append(f"status: {status}")
                out.append("")
            output = state.get("output")
            if output is not None:
                out.append("output:")
                out.append("")
                out.append("```json")
                try:
                    pretty = json.dumps(json.loads(output), indent=2)
                except (json.JSONDecodeError, TypeError):
                    pretty = output.rstrip("\n")
                for line in pretty.split("\n"):
                    out.append(f"    {line}")
                out.append("```")
                out.append("")
        # reasoning / step-start / step-finish: internal metadata, skipped


def convert(src: Path, dst: Path) -> None:
    data = json.loads(src.read_text())
    out = [f"# {data['info']['title']}\n"]
    for msg in data.get("messages", []):
        render_message(msg, out)
    dst.write_text("\n".join(out) + "\n")


def main() -> None:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        sys.exit(1)
    src = Path(sys.argv[1])
    dst = Path(sys.argv[2]) if len(sys.argv) > 2 else src.with_suffix(".md")
    convert(src, dst)
    print(f"wrote {dst}")


if __name__ == "__main__":
    main()