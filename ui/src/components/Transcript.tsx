import { useMemo } from "react";

/**
 * Turn a Claude Code JSONL tail into a readable conversation snippet.
 *
 * Each `line` is one JSON object from the agent's project transcript file.
 * We render speaker + a short content preview. Lines that don't parse
 * fall back to a muted raw view so the user still sees *something*.
 */
export function Transcript({ lines }: { lines: string[] }) {
  const rows = useMemo(() => lines.map(parseLine), [lines]);
  if (rows.length === 0) {
    return <span className="muted">No transcript captured yet.</span>;
  }
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      {rows.map((r, i) => (
        <div key={i} style={{ display: "grid", gridTemplateColumns: "78px 1fr", gap: 12, alignItems: "baseline" }}>
          <div className="muted" style={{ fontSize: 11, textTransform: "uppercase", letterSpacing: "0.06em" }}>
            {r.speaker}
          </div>
          <div style={{ color: r.tone === "muted" ? "var(--text-muted)" : "var(--text)" }}>
            {r.body}
          </div>
        </div>
      ))}
    </div>
  );
}

type Row = { speaker: string; body: string; tone?: "muted" };

function parseLine(line: string): Row {
  let v: any;
  try { v = JSON.parse(line); } catch { return { speaker: "raw", body: line, tone: "muted" }; }
  const kind = String(v.type ?? "");
  switch (kind) {
    case "user": {
      return { speaker: "you", body: previewUser(v) };
    }
    case "assistant": {
      return { speaker: "claude", body: previewAssistant(v) };
    }
    case "tool_use": {
      const name = String(v.name ?? "tool");
      return { speaker: `tool · ${name}`, body: shortJson(v.input) };
    }
    case "tool_result": {
      return { speaker: "result", body: previewToolResult(v), tone: "muted" };
    }
    case "result": {
      const sub = String(v.subtype ?? "");
      const tone = sub === "error_max_turns" || sub === "rate_limit" ? undefined : "muted" as const;
      return { speaker: sub || "end", body: previewResult(v), tone };
    }
    case "needs_permission":
    case "tool_permission_request": {
      const t = v.tool ?? v.name ?? "tool";
      return { speaker: "needs perm.", body: `Agent is asking to run ${t}. Answer in the agent app.` };
    }
    default:
      return { speaker: kind || "event", body: shortJson(v), tone: "muted" };
  }
}

function previewUser(v: any): string {
  const content = v.message?.content ?? v.content;
  return contentToText(content) || "(empty user message)";
}

function previewAssistant(v: any): string {
  const content = v.message?.content ?? v.content;
  return contentToText(content) || "(empty assistant message)";
}

function previewToolResult(v: any): string {
  const c = v.content ?? v.output;
  const text = contentToText(c) || shortJson(c);
  return truncate(text, 280);
}

function previewResult(v: any): string {
  const sub = String(v.subtype ?? "");
  if (sub === "rate_limit") return "Hit a rate limit. Will retry when allowed.";
  if (sub === "error_max_turns") return "Reached the max-turns limit.";
  if (v.message) return contentToText(v.message) || "(end of turn)";
  return "(end of turn)";
}

function contentToText(content: any): string {
  if (content == null) return "";
  if (typeof content === "string") return truncate(content, 320);
  if (Array.isArray(content)) {
    const parts: string[] = [];
    for (const block of content) {
      if (typeof block === "string") parts.push(block);
      else if (block?.type === "text" && typeof block.text === "string") parts.push(block.text);
      else if (block?.type === "tool_use") parts.push(`[uses ${block.name}]`);
      else if (block?.type === "tool_result") parts.push(`[result: ${shortJson(block.content)}]`);
    }
    return truncate(parts.join(" ").trim(), 320);
  }
  if (typeof content === "object") {
    if (typeof content.text === "string") return truncate(content.text, 320);
  }
  return "";
}

function shortJson(v: unknown): string {
  if (v == null) return "";
  try {
    const s = JSON.stringify(v);
    return truncate(s, 220);
  } catch {
    return String(v);
  }
}

function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  return s.slice(0, max) + "…";
}
