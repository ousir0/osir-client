import { useState } from "react";
import "./account-pages.css";
import { AccountResources } from "./AccountResources";
import { OsirUsagePage } from "./OsirUsagePage";
import { OsirTabs } from "./OsirTabs";
import type { KeyUsageTool } from "./OsirKeyDetails";

export function ResourcePage({
  page,
  onConnect,
  onViewUsage,
  initialUsageGroup,
  onOpenTool,
}: {
  page: "keys" | "subscriptions" | "usage";
  onConnect: () => void;
  onViewUsage?: (groupId?: number) => void;
  initialUsageGroup?: number;
  onOpenTool?: (tool: KeyUsageTool) => void;
}) {
  const [groupTab, setGroupTab] = useState(false);
  if (page === "usage")
    return (
      <OsirUsagePage>
        <AccountResources
          key={initialUsageGroup ?? "all"}
          page="usage"
          onConnect={onConnect}
          initialUsageGroup={initialUsageGroup}
        />
      </OsirUsagePage>
    );
  return (
    <div className="osir-account-pages">
      {page === "keys" && (
        <OsirTabs
          label="密钥资源"
          value={groupTab ? "groups" : "keys"}
          onChange={(value) => setGroupTab(value === "groups")}
          items={[
            { value: "keys", label: "我的密钥" },
            { value: "groups", label: "可用分组" },
          ]}
        />
      )}
      <AccountResources
        key={page}
        page={page === "keys" && groupTab ? "groups" : page}
        onConnect={onConnect}
        onViewUsage={onViewUsage}
        onOpenTool={onOpenTool}
      />
    </div>
  );
}
