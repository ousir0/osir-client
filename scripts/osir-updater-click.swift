import AppKit
import ApplicationServices
import Foundation
let args = CommandLine.arguments
guard args.count == 3, let pid = Int32(args[1]), let app = NSRunningApplication(processIdentifier:pid), app.bundleIdentifier == "com.osir.client", app.bundleURL?.path == args[2] else { fatalError("Owned app identity mismatch") }
guard AXIsProcessTrusted() else { fatalError("Accessibility permission unavailable; no permission settings changed") }
app.activate(options:[.activateIgnoringOtherApps,.activateAllWindows])
let root=AXUIElementCreateApplication(pid)
func attribute(_ node:AXUIElement,_ key:String)->CFTypeRef? { var value:CFTypeRef?;guard AXUIElementCopyAttributeValue(node,key as CFString,&value) == .success else{return nil};return value }
let until=Date().addingTimeInterval(100)
while Date()<until {
 var queue:[AXUIElement]=[root];var index=0
 while index<queue.count && index<10000 {
  let node=queue[index];index+=1
  let role=attribute(node,kAXRoleAttribute) as? String ?? ""
  let labels=[kAXTitleAttribute,kAXDescriptionAttribute,kAXValueAttribute].compactMap{attribute(node,$0) as? String}
  if role == kAXButtonRole && labels.contains(where:{$0.trimmingCharacters(in:.whitespacesAndNewlines)=="更新并重启"}) {
   guard AXUIElementPerformAction(node,kAXPressAction as CFString) == .success else {fatalError("Update button press failed")}
   print("real-update-button-clicked");exit(0)
  }
  queue += attribute(node,kAXChildrenAttribute) as? [AXUIElement] ?? []
 }
 Thread.sleep(forTimeInterval:1)
}
fatalError("Actual update button did not appear")
