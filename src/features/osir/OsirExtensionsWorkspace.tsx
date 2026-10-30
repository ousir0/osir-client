import { useEffect, useState } from "react";
import UnifiedMcpPanel from "@/components/mcp/UnifiedMcpPanel";
import UnifiedSkillsPanel from "@/components/skills/UnifiedSkillsPanel";
import { OsirTabs } from "./OsirTabs";
import "./extensions-workspace.css";

/** A single destination for native extension configuration and its write guards. */
export function OsirExtensionsWorkspace({
  desktop,
  onNavigationBlockedChange,
}: {
  desktop: boolean;
  onNavigationBlockedChange?: (blocked: boolean) => void;
}) {
  const [tab, setTab] = useState("mcp");
  const [interactionBlocked, setInteractionBlocked] = useState(false);
  const [navigationBlocked, setNavigationBlocked] = useState(false);
  // Skills reports its read-only update check as an interaction lock only.
  // MCP's interaction callback already includes only editors and writes.
  const blocked = tab === "mcp" ? interactionBlocked : navigationBlocked;
  useEffect(() => {
    onNavigationBlockedChange?.(blocked);
  }, [blocked, onNavigationBlockedChange]);
  useEffect(
    () => () => onNavigationBlockedChange?.(false),
    [onNavigationBlockedChange],
  );

  return (
    <div className="osir-extensions-workspace">
      <fieldset className="osir-extensions-navigation" disabled={blocked}>
        <legend className="sr-only">扩展类型</legend>
        <OsirTabs
          label="扩展类型"
          value={tab}
          items={[
            { value: "mcp", label: "MCP 服务" },
            { value: "skills", label: "技能" },
          ]}
          onChange={(value) => {
            if (!blocked) {
              setInteractionBlocked(false);
              setNavigationBlocked(false);
              setTab(value);
            }
          }}
        />
      </fieldset>
      {blocked && (
        <p className="osir-extensions-status" role="status">
          请先保存或取消当前编辑，完成操作后再切换页面。
        </p>
      )}
      {!desktop ? (
        <div className="osir-extensions-preview" role="status">
          请在 OSIR 桌面客户端管理 MCP 服务与技能，预览不读取本机配置。
        </div>
      ) : tab === "mcp" ? (
        <UnifiedMcpPanel
          osirVariant
          onInteractionBlockedChange={setInteractionBlocked}
        />
      ) : (
        <UnifiedSkillsPanel
          osirVariant
          onInteractionBlockedChange={setInteractionBlocked}
          onNavigationBlockedChange={setNavigationBlocked}
        />
      )}
    </div>
  );
}
