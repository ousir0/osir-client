/**
 * Only product UI copy is branded. Keep upstream attribution, promotions,
 * compatibility-format names, protocol keys, and stored provider IDs intact.
 */
const PRODUCT_COPY_KEYS = [
  "app.title",
  "firstRunNotice.title",
  "firstRunNotice.bodyDefault",
  "firstRunNotice.bodyOfficial",
  "provider.stackReselectHint",
  "provider.stackPartial",
  "provider.adoptCatalog",
  "provider.adoptCatalogDone",
  "provider.adoptCatalogFailed",
  "provider.openTerminalDialog.isolationBody",
  "provider.form.agy.envFileHint",
  "confirm.removeProviderMessage",
  "confirm.unifyCodexHistory.message",
  "settings.advanced.globalProxy.description",
  "settings.advanced.logConfig.enabledDescription",
  "settings.backupStorage.items.codexHistoryUnifyRestore.hint",
  "settings.backupStorage.items.codexHistoryProviderMigration.hint",
  "settings.backupStorage.items.liveFirstWrite.hint",
  "settings.backupStorage.items.proxyLiveBackup.hint",
  "settings.s3Sync.bucketPlaceholder",
  "settings.codexSessionCompressionDescription",
  "settings.browsePlaceholderApp",
  "settings.portableUpdateHint",
  "settings.toolUpgradeUnmanagedDetail",
  "settings.restartRequiredMessage",
  "pi.current.stateUnavailableHint",
  "pi.prompts.externalTitle",
  "managedAuth.remove.leadOne",
  "managedAuth.remove.leadAll",
  "opencode.headerValuePlaceholder",
  "usage.modelsDevAutoSync.description",
  "usage.modelsDevAutoSync.enableConfirmMessage",
  "usage.sources.scanHelp",
  "dbUpgrade.description",
  "mcp.unifiedPanel.noImportFound",
  "prompts.descriptionPlaceholder",
  "prompts.emptyFile",
  "prompts.toast.deletedSub",
  "skills.uninstallPiPreserved",
  "skills.importDescription",
  "skills.noUnmanagedFound",
  "skills.storageSheet.locationCcSwitch",
  "skills.storageSheet.moveConfirmBody",
  "deeplink.confirmImportDescription",
  "deeplink.providerEnabledWarning",
  "proxy.stackMode.codexStale.unverifiedHint",
  "nav.settingsHasUpdate",
  "appsPage.desktopHelp",
  "mode.dialog.routeLead",
  "mode.dialog.routeNoteFile",
  "mode.dialog.routeNoteKeepRunning",
  "mode.dialog.desktopRouteNoteKeepRunning",
  "mode.dialog.exitLead",
  "mode.help.direct",
  "mode.help.route",
  "routingSettings.clientHostHelp",
  "mcpPage.noFilesChanged",
  "mcpPage.deleteBodyApps",
  "mcpPage.deleteBody",
  "mcpPage.import.sameName",
  "mcpPage.drawer.displayNameHelp",
  "skillsPage.banner.unmanaged",
  "skillsPage.confirm.uninstallBody",
  "skillsPage.import.lead",
  "skillsPage.import.foundCcSwitch",
  "newLayoutNotice.title",
  "newLayoutNotice.body",
  "kimi.oauth.dismissConfirm",
  "omp.current.stateUnavailableHint",
  "omp.errors.manualEdit",
] as const;

type TranslationTree = { [key: string]: string | TranslationTree };

export function withOsirProductCopy<T extends TranslationTree>(source: T): T {
  const result = JSON.parse(JSON.stringify(source)) as T;
  for (const key of PRODUCT_COPY_KEYS) {
    const path = key.split(".");
    const leaf = path.pop()!;
    let parent: TranslationTree | undefined = result;
    for (const segment of path) {
      const next: string | TranslationTree | undefined = parent?.[segment];
      parent = typeof next === "object" ? next : undefined;
    }
    const value = parent?.[leaf];
    if (parent && typeof value === "string") {
      parent[leaf] = value
        .replace(/CC Switch/g, "OSIR Client")
        .replace(/cc-switch/g, "osir_client");
    }
  }
  return result;
}
