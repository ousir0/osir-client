// CI helper: observe only the process owned by the smoke launcher. No AX,
// Apple events, activation, permission prompts, window mutations or app quits.
import AppKit
import CoreGraphics
import Foundation

let args = CommandLine.arguments
guard args.count == 3, let pid = Int32(args[1]), pid > 0 else {
    fputs("Usage: window-probe <pid> <bundle-identifier>\n", stderr)
    exit(2)
}
let identifier = args[2]
let rows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
    as? [[String: Any]]
let app = NSRunningApplication(processIdentifier: pid)
let windows: [[String: Any]] = (rows ?? []).compactMap { row in
    guard (row[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid,
          (row[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
          let id = row[kCGWindowNumber as String] as? NSNumber,
          let bounds = row[kCGWindowBounds as String] as? [String: Any],
          let width = bounds["Width"] as? NSNumber,
          let height = bounds["Height"] as? NSNumber,
          width.doubleValue >= 900, height.doubleValue >= 600,
          (row[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 0 > 0
    else { return nil }
    return [
        "id": id, "pid": pid, "width": width, "height": height,
        "title": row[kCGWindowName as String] as? String ?? "",
        "onScreen": true, "layer": 0,
    ]
}
let otherPids = NSWorkspace.shared.runningApplications.filter {
    $0.bundleIdentifier == identifier && $0.processIdentifier != pid
}.map { $0.processIdentifier }
let report: [String: Any] = [
    "schemaVersion": 1, "pid": pid, "quartzSessionAvailable": rows != nil,
    "screenCaptureAllowed": CGPreflightScreenCaptureAccess(),
    "bundleIdentifier": app?.bundleIdentifier ?? "",
    "bundlePath": app?.bundleURL?.resolvingSymlinksInPath().path ?? "",
    "otherInstancePids": otherPids, "windows": windows,
]
let data = try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
FileHandle.standardOutput.write(data)
FileHandle.standardOutput.write(Data([10]))
