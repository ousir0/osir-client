
$path = {}
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
$acl = [Security.AccessControl.DirectorySecurity]::new()
$acl.SetOwner($sid)
$acl.SetAccessRuleProtection($true, $false)
foreach ($who in @($sid, [Security.Principal.SecurityIdentifier]::new('S-1-5-18'))) {
  $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($who, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
}
# This helper always runs Windows PowerShell 5.1/.NET Framework. Call its ACL
# APIs directly: an inherited PowerShell 7 PSModulePath can break Security
# cmdlet autoloading even with -NoProfile and an absolute powershell.exe.
[IO.Directory]::SetAccessControl($path, $acl)
$actual = [IO.Directory]::GetAccessControl($path)
if (-not $actual.AreAccessRulesProtected -or $actual.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid.Value) { throw 'cache protection' }
'true'
