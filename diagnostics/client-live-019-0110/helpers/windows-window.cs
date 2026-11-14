// Read-only subset of frozen r2 packaged smoke; no activation or input methods.
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class OsirRealWindow {
    public long Handle; public string Title; public string ClassName;
    public int Left; public int Top; public int Width; public int Height;
}
public static class OsirWindows {
    private delegate bool Callback(IntPtr h, IntPtr p);
    [StructLayout(LayoutKind.Sequential)] private struct Rect { public int Left,Top,Right,Bottom; }
    [DllImport("user32.dll")] private static extern bool EnumWindows(Callback cb, IntPtr p);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetWindowTextW(IntPtr h, StringBuilder s, int count);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetClassNameW(IntPtr h, StringBuilder s, int count);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out Rect rect);
    [DllImport("user32.dll")] private static extern int GetWindowLongW(IntPtr h, int index);
    public static OsirRealWindow[] ForProcess(int pid) {
        var result = new List<OsirRealWindow>();
        EnumWindows((h,p) => {
            uint owner; GetWindowThreadProcessId(h, out owner);
            if(owner!=(uint)pid || !IsWindowVisible(h)) return true;
            var title=new StringBuilder(1024); GetWindowTextW(h,title,title.Capacity);
            var cls=new StringBuilder(512); GetClassNameW(h,cls,cls.Capacity);
            Rect r; if(!GetWindowRect(h,out r)) return true;
            if(title.ToString().EndsWith("-siw") || (GetWindowLongW(h,-20)&0x80)!=0) return true;
            result.Add(new OsirRealWindow {Handle=h.ToInt64(),Title=title.ToString(),ClassName=cls.ToString(),Left=r.Left,Top=r.Top,Width=r.Right-r.Left,Height=r.Bottom-r.Top});
            return true;
        },IntPtr.Zero);
        return result.ToArray();
    }
}
