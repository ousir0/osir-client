import { useQuery, useQueryClient } from "@tanstack/react-query";

export type CodexInstallSource = "auto" | "official" | "osir_mirror";
export const CODEX_INSTALL_SOURCE_QUERY = [
  "osir-codex-install-source",
] as const;
export const CODEX_INSTALL_SOURCES: Record<CodexInstallSource, string> = {
  auto: "自动选择",
  osir_mirror: "OSIR 下载源",
  official: "官方更新源",
};
export const codexDesktopReleaseKey = (
  source: CodexInstallSource,
  targetPath?: string,
) => ["osir-codex-update-check", source, targetPath ?? "managed"] as const;

/** Session cache only; never writes preferences or changes a native installation. */
export function useCodexInstallSource() {
  const client = useQueryClient();
  const selected = useQuery<CodexInstallSource>({
    queryKey: CODEX_INSTALL_SOURCE_QUERY,
    queryFn: () =>
      client.getQueryData<CodexInstallSource>(CODEX_INSTALL_SOURCE_QUERY) ??
      "auto",
    initialData: "auto",
    enabled: false,
    staleTime: Infinity,
  });
  const source: CodexInstallSource =
    selected.data === "official" || selected.data === "osir_mirror"
      ? selected.data
      : "auto";
  return {
    source,
    setSource: (next: CodexInstallSource) => {
      if (next === "auto" || next === "official" || next === "osir_mirror")
        client.setQueryData(CODEX_INSTALL_SOURCE_QUERY, next);
    },
  };
}
