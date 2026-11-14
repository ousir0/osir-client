// Observation only. Reuses AppKit/Accessibility used by the reviewed click helper.
import AppKit
import ApplicationServices
import CoreGraphics
import Foundation
let session = CGSessionCopyCurrentDictionary() as? [String: Any]
let result: [String: Any] = ["accessibilityTrusted": AXIsProcessTrusted(), "consoleSession": session?["kCGSessionOnConsoleKey"] as? Bool ?? false]
FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]))
