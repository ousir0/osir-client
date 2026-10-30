import type { ConnectionToolId } from "./providerConfig";

const normalized = (id: string) =>
  id.toLowerCase().split("/").pop()!.replace(/[ _]/g, "-");

/** Pick only an ID actually returned by the provider. Explicit user choices
 * survive a refresh; preferences apply when a connection has no valid model. */
export function defaultConnectionModel(
  tool: ConnectionToolId,
  models: readonly string[],
  current = "",
): string {
  const conversation = models.filter(
    (id) =>
      id.trim() !== "" &&
      !/image|embedding|rerank|tts|whisper|dall-e/i.test(normalized(id)),
  );
  if (conversation.includes(current)) return current;
  const available = conversation;
  const preferred = available.find((id) => {
    const name = normalized(id);
    return tool === "claude" || tool === "claude-desktop"
      ? /^(?:claude-)?opus-5[.-]5(?:$|-)/.test(name)
      : tool === "codex" || tool === "opencode"
        ? /^gpt-6[.-]1-sol(?:$|-)/.test(name)
        : false;
  });
  if (preferred) return preferred;
  const family = available.filter((id) => {
    const name = normalized(id);
    if (tool === "claude" || tool === "claude-desktop")
      return /claude|opus|sonnet|haiku/.test(name);
    if (tool === "codex") return /^(?:gpt-|o\d)/.test(name);
    if (tool === "gemini") return name.startsWith("gemini-");
    return true;
  });
  // Catalogs currently expose IDs, not release timestamps. Compare numeric
  // versions within the tool's family; prefer stable IDs over preview aliases
  // at the same version. Never synthesize an unavailable latest-model alias.
  const ranked = [...(family.length ? family : available)].sort((a, b) => {
    const parts = (id: string) =>
      [...normalized(id).matchAll(/\d+/g)].map((part) => Number(part[0]));
    const left = parts(a),
      right = parts(b);
    for (let index = 0; index < Math.max(left.length, right.length); index++) {
      const delta = (right[index] ?? 0) - (left[index] ?? 0);
      if (delta) return delta;
    }
    const preview = (id: string) => /preview|experimental|exp(?:-|$)/i.test(id);
    return Number(preview(a)) - Number(preview(b));
  });
  return ranked[0] ?? "";
}
