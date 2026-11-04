import type { NativeConfiguration } from "./connectionMode";

export function NativeConfigurationView({
  configuration,
  loading,
  failed,
  refreshing,
  onRefresh,
}: {
  configuration?: NativeConfiguration;
  loading: boolean;
  failed: boolean;
  refreshing?: boolean;
  onRefresh: () => void;
}) {
  const source = configuration?.source;
  return (
    <section aria-label="本机原有配置" className="osir-native-configuration">
      <div className="osir-section-title">
        <h2>本机原有配置</h2>
        <button disabled={loading || refreshing} onClick={onRefresh}>
          {refreshing ? "正在刷新…" : "刷新配置"}
        </button>
      </div>
      <div className="osir-empty" role={failed ? "alert" : "status"}>
        {loading ? (
          <p>正在读取本机配置…</p>
        ) : failed ? (
          <p>本机配置读取失败，请刷新后重试。</p>
        ) : (
          <>
            <strong>{configuration?.label || "暂无法确认本机原有配置"}</strong>
            <span className="osir-badge">
              {source === "official_login"
                ? "官方登录"
                : source === "local_config"
                  ? "本机已有配置"
                  : "来源暂无法确认"}
            </span>
            {configuration?.detail && <p>{configuration.detail}</p>}
            {configuration?.status === "unavailable" && (
              <p>请先在对应工具中完成登录或配置，再刷新查看。</p>
            )}
          </>
        )}
      </div>
    </section>
  );
}
