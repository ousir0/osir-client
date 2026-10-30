export type WorkBuddyLaunch = {
  isolated: boolean;
  alreadyRunning: boolean;
  runtimeStatus?: "running" | "stopped" | "unavailable";
  foreground?: boolean;
  restarted?: boolean;
};

export function workBuddyLaunchNotice(
  result: WorkBuddyLaunch,
  saved = false,
  reload = false,
) {
  const name = result.isolated ? "开发隔离 WorkBuddy" : "WorkBuddy";
  if (result.foreground === false || result.runtimeStatus === "unavailable")
    return `${saved ? "模型已保存。" : ""}${name} 已收到${reload ? "重新载入" : "打开"}请求，但尚未确认前台窗口，请在应用中检查。`;
  if (result.restarted === true && result.foreground === true)
    return `${saved ? "模型已保存。" : ""}${name} 已重新打开；请在模型菜单中选择自定义模型，并在应用中确认是否可用。`;
  if (reload)
    return `${saved ? "模型已保存。" : ""}${name} 的重新载入尚未确认，请检查应用状态后重试。`;
  if (result.alreadyRunning)
    return saved
      ? `模型已保存。${name} 已在运行；重新载入后才会读取最新配置。`
      : `${name} 已在运行，打开操作不会重新载入配置。`;
  return `${saved ? "模型已保存，" : ""}${name} 已打开，请在模型菜单中选择自定义模型。`;
}
