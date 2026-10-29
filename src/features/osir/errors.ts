import { extractErrorMessage } from "@/utils/errorUtils";
export function connectionError(
  error: unknown,
  secrets: string[] = [],
): string {
  let message = extractErrorMessage(error) || "操作未完成，请刷新状态后重试。";
  for (const secret of secrets.filter(Boolean))
    message = message.split(secret).join("[已隐藏]");
  return message
    .replace(/(Bearer|Basic)\s+[^\s,;"'<>]+/gi, "$1 [已隐藏]")
    .replace(/sk-[A-Za-z0-9_-]+/g, "[已隐藏]")
    .replace(
      /((?:access[_-]?token|refresh[_-]?token|id[_-]?token|api[_-]?key|authorization|code[_-]?verifier|client[_-]?secret|password)["']?\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s&,;}]+)/gi,
      "$1[已隐藏]",
    )
    .replace(/([?&](?:code|state)=)[^\s&#]+/gi, "$1[已隐藏]");
}
