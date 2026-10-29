// Development-only fixture; excluded from production HTML entry points.
// All native calls are intercepted; unknown calls and writes fail closed.
import React from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { TooltipProvider } from "@/components/ui/tooltip";
import { ThemeProvider } from "@/components/theme-provider";
import { OsirToaster } from "./features/osir/OsirToaster";
import { initializeInputModality } from "@/lib/inputModality";
import type { Provider } from "@/types";
import { sessionsApi } from "@/lib/api/sessions";
import { buildTurnIndex, buildTurns } from "@/components/sessions/reader/turns";
import { UpdateProvider } from "@/contexts/UpdateContext";
import "./index.css";
import { configureProductTranslations } from "./i18n";
configureProductTranslations(true);
const generation = 777;
const profile = {
  id: 777,
  username: "合成视觉示例",
  email: "visual@example.invalid",
  balance: 0,
};
const recoveryFixture = new URLSearchParams(location.search).has(
  "keychain-recovery",
);
const restoringFixture = new URLSearchParams(location.search).has(
  "login-restoring",
);
const frontendErrorFixture = new URLSearchParams(location.search).has(
  "frontend-error",
);
const session = {
  generation,
  connected: !recoveryFixture && !restoringFixture,
  canCreateKeys: false,
  profile,
  ...(restoringFixture
    ? { recovery: "restoring" as const }
    : recoveryFixture
      ? { recovery: "unlock_required" as const }
      : {}),
};
// Synthetic invoice records are visual fixtures only; submissions and downloads
// remain blocked, just like configuration writes below.
const invoiceOrders = [
  {
    id: 701,
    out_trade_no: "SYNTHETIC-ORDER-701",
    order_type: "balance",
    payment_type: "fixture",
    pay_amount: 600,
    currency: "CNY",
    paid_at: "2026-10-27T08:00:00Z",
  },
];
const invoiceRecords = [
  {
    id: 801,
    ticket_no: "SYNTHETIC-INV-801",
    status: "pending_admin",
    invoice_amount: 600,
    invoice_currency: "CNY",
    invoice_order_count: 1,
    invoice_type: "vat_ordinary",
    invoice_service_name: "technical_service",
    user_unread: false,
    created_at: "2026-10-28T01:00:00Z",
    updated_at: "2026-10-28T01:00:00Z",
  },
  {
    id: 802,
    ticket_no: "SYNTHETIC-INV-802",
    status: "pending_user",
    invoice_amount: 1200,
    invoice_currency: "CNY",
    invoice_order_count: 2,
    invoice_type: "vat_ordinary",
    invoice_service_name: "technical_service",
    user_unread: true,
    created_at: "2026-10-26T01:00:00Z",
    updated_at: "2026-10-28T02:00:00Z",
  },
  {
    id: 803,
    ticket_no: "SYNTHETIC-INV-803",
    status: "resolved",
    invoice_amount: 800,
    invoice_currency: "CNY",
    invoice_order_count: 1,
    invoice_type: "vat_ordinary",
    invoice_service_name: "technical_service",
    user_unread: false,
    created_at: "2026-10-20T01:00:00Z",
    updated_at: "2026-10-25T01:00:00Z",
  },
];
const roles = {
  ANTHROPIC_MODEL: "fixture-model-primary",
  ANTHROPIC_DEFAULT_HAIKU_MODEL: "fixture-model-fast",
  ANTHROPIC_DEFAULT_SONNET_MODEL: "fixture-model-primary",
  ANTHROPIC_DEFAULT_OPUS_MODEL: "fixture-model-advanced",
};
const provider: Provider = {
  id: "visual-provider",
  name: "日常编程连接",
  category: "custom",
  settingsConfig: {
    env: { ANTHROPIC_BASE_URL: "https://example.invalid", ...roles },
  },
  meta: {
    osirBinding: {
      source: "managed_account",
      masked_key: "••••demo",
      reference: { version: 1, id: "00000000-0000-4000-8000-000000000000" },
      binding: {
        account_origin: "https://example.invalid",
        account_id: "777",
        key_id: 1,
        group_id: 17,
        tool: "claude_code",
        provider_id: "visual-provider",
        gateway_origin: "https://example.invalid",
      },
    },
  },
};
const codexProvider: Provider = {
  ...provider,
  id: "visual-codex-provider",
  name: "合成编程连接",
  settingsConfig: {
    config:
      'model_provider = "synthetic"\nmodel = "synthetic-code-primary"\n[model_providers.synthetic]\nname = "合成编程连接"\nbase_url = "https://example.invalid/v1"\nwire_api = "responses"\nrequires_openai_auth = true\n',
    modelCatalog: {
      models: [
        { model: "synthetic-code-primary" },
        { model: "synthetic-code-fast" },
        { model: "gpt-image-2" },
      ],
    },
  },
  meta: {
    ...provider.meta,
    osirBinding: {
      ...provider.meta!.osirBinding!,
      binding: {
        ...provider.meta!.osirBinding!.binding,
        provider_id: "visual-codex-provider",
        tool: "codex",
      },
    },
  },
};
const resources: Record<string, unknown> = {
  profile,
  groups: [
    {
      id: 17,
      name: "合成模型分组",
      status: "active",
      subscription_type: "standard",
      platform: "anthropic",
    },
    { id: 18, name: "合成生图分组甲", status: "active", platform: "openai" },
    { id: 19, name: "合成生图分组乙", status: "active", platform: "openai" },
  ],
  keys: {
    total: 1,
    page: 1,
    page_size: 20,
    items: [
      {
        id: 1,
        name: "合成工作密钥",
        masked_key: "••••demo",
        group_id: 17,
        status: "active",
        quota: 0,
        quota_used: 0,
      },
    ],
  },
  group_rates: [],
  subscriptions: [
    {
      id: 701,
      group_id: 17,
      group: {
        id: 17,
        name: "合成编程订阅",
        platform: "anthropic",
        subscription_type: "subscription",
        description: "日常开发与代码审阅",
        daily_limit_usd: 10,
        weekly_limit_usd: 50,
        monthly_limit_usd: 150,
        rate_multiplier: 1,
      },
      status: "active",
      starts_at: "2026-10-01T00:00:00Z",
      expires_at: "2026-11-01T00:00:00Z",
      daily_usage_usd: 3.2,
      weekly_usage_usd: 20,
      monthly_usage_usd: 72,
    },
  ],
  announcements: [
    {
      id: 7001,
      notify_mode: new URLSearchParams(location.search).has(
        "announcement-popup",
      )
        ? "popup"
        : "silent",
      title: "合成公告 · 客户端功能更新",
      content:
        "此内容仅用于通知公告的视觉检查。\n正式消息由 OSIRAPI 管理员发布。",
      starts_at: "2026-10-27T00:00:00Z",
      read_at: null,
    },
    {
      id: 7002,
      title: "合成公告 · 服务说明",
      content: "请在个人中心查看账户与服务信息。",
      read_at: "2026-10-27T00:00:00Z",
    },
  ],
  usage: {
    total: 3,
    page: 1,
    page_size: 20,
    items: [
      {
        id: 1,
        created_at: "2026-10-28T06:24:12Z",
        model: "synthetic-code-model",
        api_key_id: 1,
        api_key: { id: 1, name: "合成工作密钥" },
        group_id: 17,
        group: { id: 17, name: "合成模型分组", platform: "anthropic" },
        billing_type: 1,
        billing_mode: "token",
        request_type: "stream",
        input_tokens: 16400,
        output_tokens: 2400,
        cache_read_tokens: 12000,
        cache_creation_tokens: 6000,
        cache_creation_1h_tokens: 6000,
        actual_cost: 0.00482,
        total_cost: 0.006025,
        rate_multiplier: 0.8,
        first_token_ms: 420,
        duration_ms: 12480,
        service_tier: "priority",
        reasoning_effort: "high",
      },
      {
        id: 2,
        created_at: "2026-10-28T06:18:05Z",
        model: "synthetic-chat-model",
        api_key_id: 2,
        api_key: { id: 2, name: "合成日常密钥" },
        group_id: 18,
        group: { id: 18, name: "合成按量分组", platform: "openai" },
        billing_type: 0,
        billing_mode: "token",
        request_type: "ws_v2",
        input_tokens: 800,
        output_tokens: 120,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        actual_cost: 0.000012,
        total_cost: 0.000012,
        rate_multiplier: 1,
        first_token_ms: 210,
        duration_ms: 3600,
        service_tier: "standard",
      },
      {
        id: 3,
        created_at: "2026-10-28T05:49:22Z",
        model: "synthetic-image-model",
        api_key_id: 3,
        api_key: { id: 3, name: "合成生图密钥" },
        group_id: 19,
        group: { id: 19, name: "合成生图分组", platform: "openai" },
        billing_type: 0,
        billing_mode: "image",
        request_type: "sync",
        image_count: 2,
        image_size: "2K",
        actual_cost: 0.048,
        total_cost: 0.06,
        rate_multiplier: 0.8,
        duration_ms: 28200,
      },
    ],
  },
  usage_stats: {
    total_requests: 3,
    total_tokens: 37720,
    total_input_tokens: 17200,
    total_output_tokens: 2520,
    total_cache_tokens: 18000,
    total_cache_creation_tokens: 6000,
    total_cache_read_tokens: 12000,
    total_actual_cost: 0.052832,
    total_cost: 0.066037,
    average_duration_ms: 14760,
  },
  usage_snapshot: {
    trend: [],
    models: [
      {
        model: "synthetic-code-model",
        requests: 1,
        total_tokens: 36800,
        actual_cost: 0.00482,
        cost: 0.006025,
      },
      {
        model: "synthetic-chat-model",
        requests: 1,
        total_tokens: 920,
        actual_cost: 0.000012,
        cost: 0.000012,
      },
      {
        model: "synthetic-image-model",
        requests: 1,
        total_tokens: 0,
        actual_cost: 0.048,
        cost: 0.06,
      },
    ],
    groups: [],
  },
};
const backupFiles = [
  {
    path: "/synthetic/.codex/config.toml",
    hash: "a".repeat(64),
    sizeBytes: 128,
  },
  {
    path: "/synthetic/.codex/models.json",
    hash: "b".repeat(64),
    sizeBytes: 256,
  },
];
const backupSnapshot = {
  id: "00000000-0000-4000-8000-000000000001",
  tool: "codex",
  createdAt: "2026-10-28T08:00:00Z",
  safety: false,
  files: backupFiles,
};
mockWindows("main");
mockIPC(
  async (command, payload) => {
    const args = (payload ?? {}) as Record<string, unknown>;
    switch (command) {
      case "osir_account_session":
        return session;
      case "osir_client_telemetry_status":
        return {
          enabled: false,
          accountEnabled: false,
          available: false,
          reason: "合成视觉示例不发送客户端统计。",
        };
      case "osir_client_telemetry_sync":
      case "set_window_theme":
        return;
      case "osir_invoice_list": {
        if (new URLSearchParams(location.search).has("invoice-error"))
          throw { code: "scope_required" };
        const items = new URLSearchParams(location.search).has("invoice-empty")
          ? []
          : invoiceRecords.filter(
              (item) => !args.status || item.status === args.status,
            );
        return {
          generation,
          data: { items, total: items.length, page: 1, page_size: 20 },
        };
      }
      case "osir_invoice_eligibility":
        return {
          generation,
          data: {
            enabled: true,
            eligible: true,
            amount: 600,
            threshold: 500,
            currency: "CNY",
            order_count: invoiceOrders.length,
            orders: invoiceOrders,
          },
        };
      case "osir_invoice_attempt":
        return { generation, data: { state: "none" } };
      case "osir_invoice_detail": {
        const ticket = invoiceRecords.find((item) => item.id === args.id);
        if (!ticket) throw new Error("合成示例未提供此申请");
        return {
          generation,
          data: {
            ticket,
            invoice_orders: invoiceOrders,
            messages: [
              {
                id: 901,
                sender_role: "admin",
                content: "这是合成开票记录，仅供界面布局检查。",
                created_at: ticket.updated_at,
                attachments:
                  ticket.id === 803
                    ? [
                        {
                          id: 902,
                          file_name: "synthetic-invoice.pdf",
                          content_type: "application/pdf",
                          size_bytes: 16384,
                        },
                      ]
                    : [],
              },
            ],
          },
        };
      }
      case "osir_account_key_models":
        return {
          generation,
          models: ["synthetic-code-primary", "synthetic-code-fast"].map(
            (id) => ({ id }),
          ),
        };
      case "osir_workbuddy_config":
        return {
          path: "/synthetic/home/.workbuddy/models.json",
          revision: "a".repeat(64),
          isolated: true,
          models: [],
        };
      case "osir_detect_tools":
      case "osir_desktop_runtime":
        return [
          "codex",
          "claude",
          "claude-desktop",
          "opencode",
          "gemini",
          "grok",
          "openclaw",
          "hermes",
          "pi",
          "mcode",
          "kimi",
          "agy",
          "omp",
          "workbuddy",
        ]
          .filter(
            (id) =>
              command === "osir_detect_tools" ||
              ["codex", "claude-desktop", "workbuddy"].includes(id),
          )
          .map((id) => {
            const installed =
              [
                "codex",
                "claude",
                "claude-desktop",
                "opencode",
                "gemini",
                "openclaw",
                "workbuddy",
              ].includes(id) &&
              !new URLSearchParams(location.search).has("tools-empty");
            return {
              id,
              installed,
              version: installed ? "1.2.3" : null,
              path: null,
              status: installed ? "installed" : "not_installed",
              error: null,
              kind: ["codex", "workbuddy", "claude-desktop"].includes(id)
                ? "desktop"
                : "cli",
              runtimeStatus: !installed
                ? "unavailable"
                : id === "codex" || id === "claude"
                  ? "running"
                  : id === "gemini"
                    ? "unavailable"
                    : "stopped",
              runtimeError: null,
              launchSupported: installed,
              closeSupported: installed && id === "codex",
              restartSupported: installed && id === "codex",
            };
          });
      case "osir_desktop_auth_capability":
        return { status: "unavailable", reason: "合成 fixture 不执行账户授权" };
      case "osir_image_get":
        return {
          status: "configured",
          selection: {
            source: "saved",
            provider_id: "synthetic-image-a",
            endpoint: "https://image.example.invalid/v1",
            model: "synthetic-image-model",
            has_key: true,
          },
          candidates: [
            {
              provider_id: "synthetic-image-a",
              name: "合成生图连接甲",
              masked_key: "••••imgA",
              group_id: 18,
            },
            {
              provider_id: "synthetic-image-b",
              name: "合成生图连接乙",
              masked_key: "••••imgB",
              group_id: 19,
            },
          ],
        };
      case "osir_codex_desktop_detection":
        return {
          status: "detected",
          comparisonComplete: true,
          installations: [
            {
              path: "/synthetic/Codex.app",
              version: "1.2.3",
              build: "123",
            },
          ],
          message: "合成安装信息，仅用于布局检查",
        };
      case "osir_codex_install_check":
        return {
          source: args.source,
          actualSource: args.source === "auto" ? "osir_mirror" : args.source,
          installedBuild: 123,
          highestDetectedBuild: 123,
          comparisonComplete: true,
          release: {
            build: 124,
            version: "1.2.4",
            fullSize: 150_000_000,
            updateAvailable: true,
          },
          targetPath: "/synthetic/Codex.app",
          restorePoints: [],
        };
      case "osir_codex_install_local":
        if (args.action !== "inspect")
          throw new Error("合成 fixture 禁止安装操作");
        return {
          installedBuild: 123,
          targetPath: "/synthetic/Codex.app",
          restorePoints: [],
          recoveryRequired: false,
        };
      case "osir_codex_install_status":
        return new URLSearchParams(location.search).has("installer-active")
          ? {
              operationId: "synthetic-operation",
              phase: "verifying_application",
            }
          : { operationId: null, phase: null };
      case "scan_unmanaged_skills":
        return [
          {
            name: "项目规范示例",
            directory: "project-guidelines",
            path: "/synthetic/.codex/skills/project-guidelines",
            foundIn: ["codex", "agents"],
            description: "合成示例：本机工具与共享目录中的技能",
          },
          {
            name: "界面设计示例",
            directory: "interface-design",
            path: "/synthetic/.claude/plugins/cache/example/skills/interface-design",
            foundIn: ["claude-plugins"],
            description: "合成示例：本地插件自带技能",
          },
        ];
      case "get_providers":
        return args.app === "claude"
          ? { [provider.id]: provider }
          : args.app === "claude-desktop"
            ? {
                "synthetic-desktop-provider": {
                  ...provider,
                  id: "synthetic-desktop-provider",
                  name: "Claude 桌面工作连接",
                  settingsConfig: {
                    env: {
                      ANTHROPIC_BASE_URL: "https://example.invalid",
                      ANTHROPIC_MODEL: "fixture-model-primary",
                    },
                  },
                  meta: {
                    ...provider.meta,
                    claudeDesktopMode: "proxy",
                    claudeDesktopModelRoutes: {
                      "claude-sonnet-osir-0": {
                        model: "fixture-model-primary",
                        labelOverride: "fixture-model-primary",
                      },
                      "claude-sonnet-osir-1": {
                        model: "fixture-model-fast",
                        labelOverride: "fixture-model-fast",
                      },
                    },
                    osirBinding: {
                      ...provider.meta!.osirBinding!,
                      binding: {
                        ...provider.meta!.osirBinding!.binding,
                        tool: "claude_desktop",
                        provider_id: "synthetic-desktop-provider",
                      },
                    },
                  },
                },
              }
            : args.app === "codex"
              ? { [codexProvider.id]: codexProvider }
              : {};
      case "get_claude_desktop_status":
        return {
          supported: true,
          installed: true,
          configured: true,
          appliedId: "synthetic-fixed-profile-uuid",
          activeProviderId: "synthetic-desktop-provider",
          mode: "proxy",
          expectedBaseUrl: "http://127.0.0.1:15721/claude-desktop",
          actualBaseUrl: "http://127.0.0.1:15721/claude-desktop",
          proxyRunning: true,
          staleRawModels: false,
          missingRouteMappings: false,
          gatewayTokenConfigured: true,
        };
      case "get_current_provider":
        return args.app === "claude"
          ? provider.id
          : args.app === "codex"
            ? codexProvider.id
            : "";
      case "osir_provider_models":
        return (
          args.tool === "claude"
            ? Object.values(roles)
            : ["synthetic-code-primary", "synthetic-code-fast", "gpt-image-2"]
        ).map((id) => ({ id }));
      case "get_mcp_servers":
        return {
          filesystem: {
            id: "filesystem",
            name: "文件系统",
            server: { type: "stdio", command: "filesystem" },
            apps: {
              codex: true,
              claude: true,
              opencode: false,
              gemini: false,
              "claude-desktop": false,
              openclaw: false,
              hermes: false,
            },
          },
          browser: {
            id: "browser",
            name: "浏览器",
            server: { type: "stdio", command: "browser" },
            apps: {
              codex: true,
              claude: false,
              opencode: true,
              gemini: false,
              "claude-desktop": false,
              openclaw: false,
              hermes: false,
            },
          },
        };
      case "scan_local_extensions":
        return {
          mcp: [
            {
              id: "synthetic-local-codex-docs",
              name: "项目文档",
              app: "codex",
              sourcePath: "/synthetic/.codex/config.toml",
              scope: "user",
              transport: "stdio",
              configured: true,
              managed: false,
            },
            {
              id: "synthetic-local-claude-browser",
              name: "浏览器工具",
              app: "claude",
              sourcePath: "/synthetic/.claude.json",
              scope: "user",
              transport: "http",
              configured: true,
              managed: false,
            },
            {
              id: "synthetic-project-local",
              name: "项目开发工具与较长的来源名称示例",
              app: "claude",
              sourcePath: "/synthetic/projects/example/.mcp.json",
              scope: "project",
              transport: "stdio",
              configured: false,
              managed: true,
            },
          ],
          sources: [
            {
              app: "codex",
              path: "/synthetic/.codex/config.toml",
              status: "loaded",
            },
          ],
        };
      case "get_installed_skills":
        return [
          {
            id: "local:docs",
            name: "文档处理",
            description: "整理与转换项目文档",
            directory: "/synthetic/skills/docs",
            apps: {
              codex: true,
              claude: true,
              opencode: false,
              gemini: false,
              "claude-desktop": false,
              openclaw: false,
              hermes: false,
              pi: false,
            },
            installedAt: 1793164800,
            updatedAt: 1793164800,
          },
          {
            id: "local:review",
            name: "代码检查",
            description: "按项目规则检查代码变更",
            directory: "/synthetic/skills/review",
            apps: {
              codex: true,
              claude: false,
              opencode: false,
              gemini: false,
              "claude-desktop": false,
              openclaw: false,
              hermes: false,
              pi: false,
            },
            installedAt: 1793164800,
            updatedAt: 1793164800,
          },
        ];
      case "get_tool_versions":
        return new URLSearchParams(location.search).has("tool-updates")
          ? ["claude", "opencode", "gemini", "openclaw"].map((name) => ({
              name,
              version: "1.2.3",
              latest_version: name === "claude" ? "1.2.4" : "1.2.3",
              error: null,
              installed_but_broken: false,
              env_type: "macos",
              wsl_distro: null,
              update_blocked_by_node: null,
            }))
          : [];
      case "probe_tool_installations":
      case "list_tool_installations":
        if (
          new URLSearchParams(location.search).has("tool-update-source-confirm")
        )
          return [
            {
              tool: "claude",
              installs: [
                {
                  path: "/synthetic/npm/bin/claude",
                  version: "1.2.3",
                  runnable: true,
                  error: null,
                  source: "npm",
                  is_path_default: true,
                },
                {
                  path: "/synthetic/alternate/bin/claude",
                  version: "1.2.2",
                  runnable: true,
                  error: null,
                  source: "npm",
                  is_path_default: false,
                },
              ],
              is_conflict: true,
              needs_confirmation: true,
              command: "synthetic-source-confirmation-only",
              anchored: true,
              unmanaged: false,
            },
          ];
        if (new URLSearchParams(location.search).has("tool-update-failure"))
          return [];
        throw new Error("合成 fixture 未配置安装来源");
      case "run_tool_lifecycle_action":
        if (new URLSearchParams(location.search).has("tool-update-failure"))
          throw new Error(
            "TOOL_INSTALL_DNS: 无法解析下载地址，请检查网络或代理后重试。",
          );
        throw new Error("合成 fixture 禁止安装操作");
      case "list_sessions":
        if (new URLSearchParams(location.search).has("history-error"))
          throw new Error("合成会话读取失败");
        if (new URLSearchParams(location.search).has("history-empty"))
          return [];
        return [
          {
            providerId: args.providerId ?? "codex",
            sessionId: "synthetic-session-1",
            title: "合成会话 · 客户端界面优化",
            summary: "仅用于检查会话列表布局，不含真实会话内容。",
            projectDir: "/synthetic/projects/osir-client",
            sourcePath: "/synthetic/sessions/session-1.jsonl",
            createdAt: Date.now() - 7_200_000,
            lastActiveAt: Date.now() - 1_800_000,
            readOnly: true,
          },
          {
            providerId: args.providerId ?? "codex",
            sessionId: "synthetic-session-2",
            title: "合成会话 · 接口字段核对",
            projectDir: "/synthetic/projects/osir-client",
            sourcePath: "/synthetic/sessions/session-2.jsonl",
            createdAt: Date.now() - 86_400_000,
            lastActiveAt: Date.now() - 43_200_000,
            readOnly: true,
          },
        ];
      case "get_settings":
        return {};
      case "plugin:app|version":
        return "4.0.20";
      case "get_update_distribution":
        return {
          kind: "osir",
          updatesEnabled: false,
          reason: "合成视觉示例不检查或安装客户端更新。",
        };
      case "is_portable_mode":
        return false;
      case "get_app_config_dir_override":
        return null;
      case "get_config_dir":
        return "/synthetic/config";
      case "plugin:path|resolve_directory":
        if (args.directory === 21) return "/synthetic/home";
        throw new Error("合成 fixture 未提供此系统目录");
      case "plugin:path|join": {
        const paths = args.paths;
        if (
          !Array.isArray(paths) ||
          !paths.every((part) => typeof part === "string")
        )
          throw new Error("合成 fixture 收到无效路径");
        return paths
          .map((part, index) =>
            index === 0 ? part.replace(/\/+$/, "") : part.replace(/^\/+/, ""),
          )
          .join("/");
      }
      case "list_db_backups":
      case "list_backup_locations":
        return [];
      case "osir_tool_backups_list":
        return { snapshots: [backupSnapshot], pendingRecovery: false };
      case "osir_tool_backup_preview":
        if (args.snapshotId !== backupSnapshot.id)
          throw new Error("合成预览只允许使用固定示例快照");
        return {
          previewId: "synthetic-preview",
          snapshotId: backupSnapshot.id,
          tool: "codex",
          files: backupFiles.map((file, index) => ({
            path: file.path,
            currentHash: index === 0 ? "c".repeat(64) : file.hash,
            snapshotHash: file.hash,
            currentSize: file.sizeBytes + (index === 0 ? 8 : 0),
            snapshotSize: file.sizeBytes,
            changed: index === 0,
          })),
        };
      case "osir_legacy_snapshot":
        return null;
      case "osir_get_connection_mode":
        return {
          mode: new URLSearchParams(location.search).has(
            "connection-unavailable",
          )
            ? "unavailable"
            : "osir_api",
          canRestoreNative: false,
        };
      case "get_app_mode":
        return { mode: "direct", attached: false };
      case "get_proxy_stack":
        return { active: false, members: [], notice: null };
      case "osir_connection_details":
        if (
          ![provider.id, "synthetic-desktop-provider"].includes(
            String(args.providerId),
          ) ||
          args.generation !== generation
        )
          throw new Error("合成详情只允许当前示例连接");
        return {
          generation,
          providerId: args.providerId,
          keyId: 1,
          groupId: 17,
          keyName: "合成工作密钥",
          groupName: "合成模型分组",
          maskedKey: "••••demo",
          groupChanged: false,
        };
      case "osir_account_resource": {
        if (!(String(args.resource) in resources))
          throw new Error("合成资源未提供");
        const params = (args.parameters ?? {}) as Record<string, unknown>;
        let data = resources[String(args.resource)];
        if (
          args.resource === "keys" &&
          new URLSearchParams(location.search).has("many-keys")
        ) {
          data = {
            total: 12,
            page: 1,
            page_size: 20,
            items: Array.from({ length: 12 }, (_, index) => ({
              id: index + 1,
              name: `合成工作密钥 ${index + 1}`,
              masked_key: "••••demo",
              group_id: 17,
              group: { name: "合成模型分组" },
              status: "active",
            })),
          };
        }
        if (args.resource === "usage_snapshot") {
          const start = String(params.start_date);
          const end = String(params.end_date);
          const days = Math.min(
            367,
            Math.floor((Date.parse(end) - Date.parse(start)) / 86_400_000) + 1,
          );
          const sparse = new URLSearchParams(location.search).has(
            "sparse-usage",
          );
          data = {
            ...(resources.usage_snapshot as object),
            ...(new URLSearchParams(location.search).has("many-models")
              ? {
                  models: Array.from({ length: 24 }, (_, index) => ({
                    model: `synthetic-chat-model-${String(index + 1).padStart(2, "0")}`,
                    requests: index + 1,
                    total_tokens: (index + 1) * 120000,
                  })),
                }
              : {}),
            start_date: start,
            end_date: end,
            granularity: params.granularity,
            trend: Number.isFinite(days)
              ? Array.from({ length: days }, (_, index) => ({
                  date: new Date(Date.parse(start) + index * 86_400_000)
                    .toISOString()
                    .slice(0, 10),
                  total_tokens: params.model
                    ? (index % 5) * (sparse ? 240000 : 2400)
                    : (index % 9) * (sparse ? 840000 : 8400),
                })).filter((_, index) => !sparse || index >= days - 35)
              : [],
          };
        }
        return {
          generation,
          fetched_at: new Date().toISOString(),
          data,
        };
      }
      case "osir_key_management":
        return {
          generation,
          keyId: args.keyId,
          source: "saved_provider_metadata",
          localUsages: [
            {
              tool: "claude_code",
              providerId: provider.id,
              providerName: provider.name,
            },
          ],
          capabilities: { rename: false, changeStatus: false },
        };
      case "osir_tool_backup_create":
      case "osir_tool_backup_restore":
      case "osir_tool_backup_recover":
      default:
        throw new Error("合成 fixture 禁止外网和配置写入");
    }
  },
  { shouldMockEvents: true },
);
window.fetch = async () => {
  throw new Error("合成 fixture 禁止网络请求");
};
const client = new QueryClient({
  defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } },
});
client.setQueryData(["osir-account", "session"], session);
// Exercise the real reader and virtual list with synthetic messages. The
// fixture has no native channel, disk reads, conversation or network access.
const visualMessages = Array.from({ length: 8 }, (_, index) => [
  {
    role: "user",
    id: `visual-question-${index}`,
    content: `第 ${index + 1} 个合成问题：请核对客户端的布局和交互。`,
    ts: Date.now() - 3_600_000 + index * 60_000,
  },
  {
    role: "assistant",
    id: `visual-reply-${index}`,
    content: `### 布局核对 ${index + 1}\n\n这段内容仅用于视觉验收。左侧目录应在最小窗口保留，正文可以独立滚动。\n\n- 工具栏集中显示常用操作\n- 会话内容自然换行\n- 关闭后返回原列表位置`,
    ts: Date.now() - 3_590_000 + index * 60_000,
  },
]).flat();
sessionsApi.streamMessages = async (_provider, _path, onChunk) => {
  onChunk({
    type: "header",
    total: visualMessages.length,
    turns: buildTurnIndex(buildTurns(visualMessages)),
    cached: false,
    parseMs: 0,
  });
  onChunk({ type: "messages", start: 0, messages: visualMessages });
  onChunk({ type: "done", payloadBytes: 0 });
};
initializeInputModality();
const { default: OsirWorkbench } = await import(
  "./features/osir/OsirWorkbench"
);
const { OsirAccountPanel } = await import("./features/osir/OsirAccountPanel");
const { default: OsirRoot } = await import("./features/osir/OsirRoot");
const { FrontendErrorBoundary } = await import(
  "./components/FrontendErrorBoundary"
);
function ThrowVisualError(): React.ReactNode {
  throw new Error("Synthetic visual fixture error");
}
createRoot(document.getElementById("root")!).render(
  frontendErrorFixture ? (
    <FrontendErrorBoundary>
      <ThrowVisualError />
    </FrontendErrorBoundary>
  ) : (
    <QueryClientProvider client={client}>
      <ThemeProvider defaultTheme="light" storageKey="osir-visual-review-theme">
        <UpdateProvider>
          <TooltipProvider>
            <div
              style={{
                position: "fixed",
                top: 0,
                left: "50%",
                transform: "translateX(-50%)",
                pointerEvents: "none",
                zIndex: 200,
                fontSize: 10,
                padding: "2px 6px",
                color: "#60748f",
                background: "#fff",
              }}
            >
              合成视觉示例 · 无网络、凭据与工具写入
            </div>
            {restoringFixture ? (
              <OsirRoot />
            ) : recoveryFixture ? (
              <OsirAccountPanel available reason="" welcome />
            ) : (
              <OsirWorkbench />
            )}
            <OsirToaster />
          </TooltipProvider>
        </UpdateProvider>
      </ThemeProvider>
    </QueryClientProvider>
  ),
);
