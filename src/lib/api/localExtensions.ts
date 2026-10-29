import { invoke } from "@tauri-apps/api/core";
export interface LocalMcpEntry {
  id: string;
  name: string;
  app: string;
  sourcePath: string;
  scope: string;
  transport: string;
  configured: boolean;
  managed: boolean;
}
export interface LocalExtensionInventory {
  mcp: LocalMcpEntry[];
  sources: {
    app: string;
    path: string;
    status: "loaded" | "invalid" | "unreadable";
  }[];
}
export const scanLocalExtensions = () =>
  invoke<LocalExtensionInventory>("scan_local_extensions");
export const importLocalMcp = (entryId: string) =>
  invoke<void>("import_local_mcp", { entryId });
