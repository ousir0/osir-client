import { useId, useState } from "react";
import { Copy, Folder, MoreHorizontal, Play, Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Checkbox } from "@/components/ui/checkbox";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { HoverTip } from "@/components/ui/hover-tip";
import { AppGlyph } from "@/components/shell/AppGlyph";
import { cn } from "@/lib/utils";
import type { SessionMeta } from "@/types";
import {
  canDeleteSession,
  formatRelativeTime,
  formatSessionTitle,
  formatShortDateTime,
  getBaseName,
  getSessionLastText,
  highlightText,
  isArchivedSession,
  isSessionAppId,
} from "./utils";

const rowIconButton =
  "session-row-action inline-flex h-7 w-7 items-center justify-center rounded-control text-fg-2 transition-colors hover:bg-selected hover:text-fg-1";

export const sessionMenuItemClass = "h-8 rounded-control px-2.5 text-body";

interface SessionItemProps {
  session: SessionMeta;
  /** 选了「全部应用」时行首显示应用图标 */
  showAppIcon: boolean;
  /** 按时间模式在第二行先写项目目录名 */
  showDir: boolean;
  selectionMode: boolean;
  isChecked: boolean;
  searchQuery?: string;
  /** 一键恢复用的终端名；为空表示这个平台不能一键恢复 */
  launchTerminal: string | null;
  hideResume?: boolean;
  resumeAvailable?: boolean;
  resumePending?: boolean;
  osirVariant?: boolean;
  bordered: boolean;
  openButtonId: string;
  onOpen: () => void;
  onToggleChecked: (checked: boolean) => void;
  onStartSelect: () => void;
  onLaunch: () => void;
  onCopyResume: () => void;
  onCopyId: () => void;
  onCopySource: () => void;
  onDelete: () => void;
}

export function SessionItem({
  session,
  showAppIcon,
  showDir,
  selectionMode,
  isChecked,
  searchQuery,
  launchTerminal,
  hideResume = false,
  resumeAvailable = false,
  resumePending = false,
  osirVariant = false,
  bordered,
  openButtonId,
  onOpen,
  onToggleChecked,
  onStartSelect,
  onLaunch,
  onCopyResume,
  onCopyId,
  onCopySource,
  onDelete,
}: SessionItemProps) {
  const { t } = useTranslation();
  const metaId = useId();
  const whyId = useId();
  const checkboxId = useId();
  const [menuOpen, setMenuOpen] = useState(false);

  const title = formatSessionTitle(session);
  const untitled = !session.title && !session.projectDir;
  const lastText = getSessionLastText(session);
  const archived = isArchivedSession(session);
  const deletable = canDeleteSession(session);
  const blocked = selectionMode && !deletable;
  const hasCommand = !hideResume && Boolean(session.resumeCommand);
  const lastActive = session.lastActiveAt ?? session.createdAt;
  const dirName = session.projectDir
    ? getBaseName(session.projectDir)
    : t("sessionManager.unknownDirectory", { defaultValue: "未知目录" });
  const hasMeta =
    showDir || Boolean(lastText) || archived || blocked || session.readOnly;
  const appId = isSessionAppId(session.providerId) ? session.providerId : null;

  const tip = [
    title,
    lastText,
    t("sessionManager.rowTimes", {
      defaultValue: "最近活跃 {{last}} · 创建 {{created}}",
      last: formatShortDateTime(lastActive) || "-",
      created: formatShortDateTime(session.createdAt) || "-",
    }),
  ]
    .filter(Boolean)
    .join("\n");

  const deleteReason = session.readOnly
    ? "此会话来自只读来源，请在原工具中管理。"
    : !deletable
      ? t(
          session.providerId === "kimi"
            ? "kimi.deleteSessionHint"
            : session.providerId === "agy"
              ? "sessionManager.agyDeleteHint"
              : "sessionManager.mcodeDeleteHint",
          {
            defaultValue: "请在 MiniMax Code 中删除",
          },
        )
      : null;

  return (
    <div
      className={cn(
        "session-list-row group relative flex h-14 items-center gap-2.5 bg-surface pe-3 ps-4 transition-colors hover:bg-subtle focus-within:bg-subtle",
        bordered && "border-t border-border",
        menuOpen && "bg-subtle",
      )}
    >
      {selectionMode ? (
        <>
          <label
            htmlFor={checkboxId}
            title={tip}
            aria-hidden="true"
            className={cn(
              "absolute inset-0",
              deletable ? "cursor-pointer" : "cursor-not-allowed",
            )}
          />
          <Checkbox
            id={checkboxId}
            checked={isChecked}
            disabled={!deletable}
            aria-label={t("sessionManager.selectSessionLabel", {
              defaultValue: "选择 {{title}}",
              title,
            })}
            aria-disabled={deletable ? undefined : true}
            aria-describedby={deletable ? undefined : whyId}
            onCheckedChange={(checked) => {
              if (deletable) onToggleChecked(checked);
            }}
            className={cn(
              "session-row-checkbox relative z-[1]",
              !deletable && "opacity-45",
            )}
          />
        </>
      ) : (
        <>
          <button
            type="button"
            id={openButtonId}
            aria-label={title}
            aria-describedby={hasMeta ? metaId : undefined}
            title={tip}
            onClick={onOpen}
            className="session-row-open absolute inset-0 cursor-pointer rounded-none focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
          />
          {deletable ? (
            <Checkbox
              tabIndex={-1}
              checked={false}
              aria-label={t("sessionManager.selectSessionLabel", {
                defaultValue: "选择 {{title}}",
                title,
              })}
              onCheckedChange={onStartSelect}
              className="session-row-checkbox relative z-[1] opacity-0 transition-opacity group-hover:opacity-100"
            />
          ) : (
            <span aria-hidden="true" className="w-4 shrink-0" />
          )}
        </>
      )}

      <div className="pointer-events-none flex min-w-0 flex-1 flex-col">
        <div className="flex min-w-0 items-center gap-2">
          {showAppIcon && appId && (
            <AppGlyph
              app={appId}
              size={16}
              badgeClassName="bg-surface group-hover:bg-subtle group-focus-within:bg-subtle"
            />
          )}
          <span
            className={cn(
              "session-row-title min-w-0 truncate text-body font-medium text-fg-1",
              untitled && "font-mono",
            )}
          >
            {searchQuery ? highlightText(title, searchQuery) : title}
          </span>
        </div>
        {hasMeta && (
          <div
            id={metaId}
            className={cn(
              "session-row-preview flex min-h-[18px] min-w-0 items-center gap-1.5 whitespace-nowrap text-caption text-fg-2",
              showAppIcon && "ps-6",
            )}
          >
            {showDir && (
              <>
                <Folder className="h-3.5 w-3.5 shrink-0" strokeWidth={1.5} />
                <span className="shrink-0">{dirName}</span>
                {lastText && (
                  <span aria-hidden="true" className="shrink-0">
                    ·
                  </span>
                )}
              </>
            )}
            {lastText && (
              <span className="min-w-0 truncate">
                {t("sessionManager.lastPrefix", {
                  defaultValue: "最后：{{text}}",
                  text: lastText,
                })}
              </span>
            )}
            {archived && (
              <span className="inline-flex h-[18px] shrink-0 items-center rounded-full border border-border-strong px-1.5 text-badge text-fg-2">
                {t("sessionManager.archived", { defaultValue: "已归档" })}
              </span>
            )}
            {session.readOnly && (
              <span className="session-source-readonly">只读来源</span>
            )}
            {blocked && (
              <span id={whyId} className="ms-auto shrink-0">
                {deleteReason}
              </span>
            )}
          </div>
        )}
      </div>

      <div className="relative w-[92px] shrink-0 self-stretch">
        <span
          className={cn(
            "pointer-events-none absolute end-0 whitespace-nowrap text-caption tabular-nums text-fg-2",
            hasMeta ? "top-[9px]" : "top-[18px]",
            !selectionMode &&
              "group-focus-within:invisible group-hover:invisible",
            !selectionMode && menuOpen && "invisible",
          )}
        >
          {lastActive ? formatRelativeTime(lastActive, t) : ""}
        </span>
        {!selectionMode && (
          <div
            className={cn(
              "absolute -end-0.5 top-3.5 z-[1] flex gap-0.5 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100",
              menuOpen && "opacity-100",
            )}
          >
            {(resumeAvailable || (launchTerminal && hasCommand)) && (
              <HoverTip
                content={
                  resumeAvailable
                    ? "续聊该会话"
                    : t("sessionManager.resumeIn", {
                        defaultValue: "在 {{terminal}} 中恢复",
                        terminal: launchTerminal,
                      })
                }
              >
                <button
                  type="button"
                  aria-label={
                    resumeAvailable
                      ? `续聊「${title}」`
                      : t("sessionManager.resumeInFor", {
                          defaultValue: "在 {{terminal}} 中恢复 {{title}}",
                          terminal: launchTerminal,
                          title,
                        })
                  }
                  onClick={onLaunch}
                  disabled={resumePending}
                  aria-busy={resumePending}
                  className={rowIconButton}
                >
                  {resumePending ? (
                    <Loader2 className="h-[15px] w-[15px] animate-spin" />
                  ) : (
                    <Play className="h-[15px] w-[15px]" strokeWidth={1.5} />
                  )}
                </button>
              </HoverTip>
            )}
            {hasCommand && (
              <HoverTip
                content={t("sessionManager.copyResumeCommand", {
                  defaultValue: "复制恢复命令",
                })}
              >
                <button
                  type="button"
                  aria-label={t("sessionManager.copyResumeFor", {
                    defaultValue: "复制「{{title}}」的恢复命令",
                    title,
                  })}
                  onClick={onCopyResume}
                  className={rowIconButton}
                >
                  <Copy className="h-[15px] w-[15px]" strokeWidth={1.5} />
                </button>
              </HoverTip>
            )}
            <DropdownMenu open={menuOpen} onOpenChange={setMenuOpen}>
              <HoverTip content={t("common.more", { defaultValue: "更多" })}>
                <DropdownMenuTrigger asChild>
                  <button
                    type="button"
                    aria-label={t("sessionManager.moreFor", {
                      defaultValue: "{{title}} 的更多操作",
                      title,
                    })}
                    className={rowIconButton}
                  >
                    <MoreHorizontal
                      className="h-[15px] w-[15px]"
                      strokeWidth={1.5}
                    />
                  </button>
                </DropdownMenuTrigger>
              </HoverTip>
              <DropdownMenuContent
                align="end"
                className={cn(
                  "w-[210px] rounded-panel p-1 shadow-v7-md",
                  osirVariant && "osir-drawer osir-session-menu",
                )}
              >
                {resumeAvailable && (
                  <DropdownMenuItem
                    className={sessionMenuItemClass}
                    disabled={resumePending}
                    onSelect={onLaunch}
                  >
                    <Play className="h-3.5 w-3.5" />
                    续聊该会话
                  </DropdownMenuItem>
                )}
                <DropdownMenuItem
                  className={sessionMenuItemClass}
                  onSelect={onCopyId}
                >
                  {t("sessionManager.copySessionId", {
                    defaultValue: "复制会话 ID",
                  })}
                </DropdownMenuItem>
                <DropdownMenuItem
                  className={sessionMenuItemClass}
                  disabled={!session.sourcePath}
                  onSelect={onCopySource}
                >
                  {t("sessionManager.copySourcePath", {
                    defaultValue: "复制源文件路径",
                  })}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  className={cn(
                    sessionMenuItemClass,
                    deletable ? "text-danger-text" : "text-fg-3",
                  )}
                  disabled={!deletable}
                  onSelect={onDelete}
                >
                  {t("sessionManager.deleteEllipsis", {
                    defaultValue: "删除…",
                  })}
                </DropdownMenuItem>
                {deleteReason && (
                  <p className="px-2.5 pb-1.5 text-caption text-fg-2">
                    {deleteReason}
                  </p>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        )}
      </div>
    </div>
  );
}
