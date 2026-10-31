import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  SettingsBlock,
  SettingsCard,
  SettingsRow,
} from "@/components/settings/SettingsLayout";
import { Checkbox } from "@/components/ui/checkbox";
import {
  clientTelemetryQuery,
  hasNativeTelemetry,
  readClientTelemetry,
  setClientTelemetry,
  setClientActivity,
} from "./clientTelemetry";

export function OsirClientTelemetrySettings() {
  const native = hasNativeTelemetry();
  const client = useQueryClient();
  const status = useQuery({
    queryKey: clientTelemetryQuery,
    queryFn: readClientTelemetry,
    enabled: native,
    retry: false,
    refetchOnWindowFocus: false,
  });
  const [saving, setSaving] = useState(false);
  const [failure, setFailure] = useState(false);

  async function save(enabled: boolean, account = false) {
    if (saving || !status.data || !native) return;
    setSaving(true);
    setFailure(false);
    try {
      const saved = await (account
        ? setClientActivity(enabled)
        : setClientTelemetry(enabled));
      if ((account ? saved.accountEnabled : saved.enabled) !== enabled)
        throw new Error("Unconfirmed preference");
      client.setQueryData(clientTelemetryQuery, saved);
    } catch {
      setFailure(true);
    } finally {
      setSaving(false);
    }
  }

  return (
    <SettingsBlock title="客户端隐私">
      <SettingsCard>
        <SettingsRow
          label="匿名客户端统计"
          htmlFor="osir-anonymous-statistics"
          description="分享版本、平台、活跃情况和异常分类，不关联账户。"
          control={
            <Checkbox
              id="osir-anonymous-statistics"
              checked={status.data?.enabled ?? false}
              disabled={!native || !status.data || status.isError || saving}
              onCheckedChange={(enabled) => void save(enabled)}
            />
          }
        />
        <SettingsRow
          label="关联账户使用统计"
          htmlFor="osir-account-statistics"
          description="分享版本和最近使用时间，管理员可查看账户名称与邮箱。"
          control={
            <Checkbox
              id="osir-account-statistics"
              checked={status.data?.accountEnabled ?? false}
              disabled={!native || !status.data || status.isError || saving}
              onCheckedChange={(enabled) => void save(enabled, true)}
            />
          }
        />
        <div className="space-y-2 px-4 py-3 text-caption text-fg-2">
          <p>
            新安装默认开启，可随时关闭。不上传本地文件、密钥、对话内容或错误原文，不影响正常使用。
          </p>
          <details>
            <summary className="cursor-pointer">了解统计范围</summary>
            <div className="space-y-2 pt-2">
              <p>
                仅在登录后的正式发布版发送统计，每 15
                分钟最多一次。匿名统计使用随机安装标识，包含发行渠道；不发送账户凭据、硬件信息、设备名称、文件路径或模型。
              </p>
              <p>
                账户统计不发送安装标识，仅保留版本、平台及首次和最近使用时间，约
                90 天后清理。取消勾选后停止后续发送，已有记录按保留期限清理。
              </p>
            </div>
          </details>
          {!native ? (
            <p>请在桌面客户端中管理此偏好；浏览器预览不会发送统计。</p>
          ) : status.isPending ? (
            <p role="status">正在读取统计偏好…</p>
          ) : status.isError ? (
            <p role="alert">
              无法读取统计偏好，请重试。
              <button type="button" onClick={() => void status.refetch()}>
                重新读取
              </button>
            </p>
          ) : status.data && !status.data.available ? (
            <p>{status.data.reason ?? "当前发行版不发送客户端统计。"}</p>
          ) : null}
          {saving && <p role="status">正在保存统计偏好…</p>}
          {failure && <p role="alert">设置未保存，请重试。</p>}
        </div>
      </SettingsCard>
    </SettingsBlock>
  );
}
