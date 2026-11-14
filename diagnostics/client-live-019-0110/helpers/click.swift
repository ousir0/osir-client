import AppKit
import ApplicationServices
import Foundation
let args = CommandLine.arguments
guard args.count == 4, let pid = Int32(args[1]) else { fatalError("Owned app arguments invalid") }
let expectedVersion = args[3]
guard expectedVersion == "0.1.10" else { fatalError("Exact target version required") }
let expectedURL = URL(fileURLWithPath:args[2]).resolvingSymlinksInPath().standardizedFileURL
let registrationDeadline=Date().addingTimeInterval(30)
var ownedApp:NSRunningApplication?
while Date()<registrationDeadline {
 if let candidate=NSRunningApplication(processIdentifier:pid), candidate.bundleIdentifier == "com.osir.client", candidate.bundleURL?.resolvingSymlinksInPath().standardizedFileURL == expectedURL { ownedApp=candidate;break }
 Thread.sleep(forTimeInterval:0.25)
}
guard let app=ownedApp else { fatalError("Owned app identity was not registered within 30 seconds") }
guard AXIsProcessTrusted() else { fatalError("Accessibility permission unavailable; no permission settings changed") }
app.activate(options:[.activateIgnoringOtherApps,.activateAllWindows])
let root=AXUIElementCreateApplication(pid)
func attribute(_ node:AXUIElement,_ key:String)->CFTypeRef? { var value:CFTypeRef?;guard AXUIElementCopyAttributeValue(node,key as CFString,&value) == .success else{return nil};return value }
let until=Date().addingTimeInterval(100)
while Date()<until {
 var queue:[AXUIElement]=[root];var index=0
 var button:AXUIElement?
 var offered=false
 while index<queue.count && index<10000 {
  let node=queue[index];index+=1
  let role=attribute(node,kAXRoleAttribute) as? String ?? ""
  let labels=[kAXTitleAttribute,kAXDescriptionAttribute,kAXValueAttribute].compactMap{attribute(node,$0) as? String}
  if labels.contains(where:{$0.contains(expectedVersion)}) { offered=true }
  if role == kAXButtonRole && labels.contains(where:{$0.trimmingCharacters(in:.whitespacesAndNewlines)=="更新并重启"}) {
   button=node
  }
  queue += attribute(node,kAXChildrenAttribute) as? [AXUIElement] ?? []
 }
 if offered, let button=button {
  guard AXUIElementPerformAction(button,kAXPressAction as CFString) == .success else {fatalError("Update button press failed")}
  let result:[String:Any] = ["realUiButton":"更新并重启","offeredVersion":expectedVersion,"pid":pid,"bundlePath":expectedURL.path,"clickedAt":ISO8601DateFormatter().string(from:Date())]
  FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject:result,options:[.sortedKeys]));exit(0)
 }
 Thread.sleep(forTimeInterval:1)
}
fatalError("Actual update button did not appear")
