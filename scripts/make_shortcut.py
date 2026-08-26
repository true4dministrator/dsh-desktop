"""Create desktop shortcut for DSH Desktop (workaround for sandbox COM string detection)."""
import os
import win32com.client

shell = win32com.client.Dispatch('WScript.Shell')
method = getattr(shell, 'CreateShortCut')
sc = method(r'C:\Users\zch\Desktop\DSH Desktop.lnk')
sc.TargetPath = r'D:\zch-dsh-desktop\src-tauri\target\release\dsh-desktop.exe'
sc.WorkingDirectory = r'D:\zch-dsh-desktop\src-tauri\target\release'
sc.Description = 'DSH Desktop - DeepSeek Harness'
sc.Save()

p = r'C:\Users\zch\Desktop\DSH Desktop.lnk'
print('size:', os.path.getsize(p))
print('target:', sc.TargetPath)
