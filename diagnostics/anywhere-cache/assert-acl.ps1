
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$directory = [IO.Directory]::GetAccessControl({})
$file = [IO.File]::GetAccessControl({})
if (-not $directory.AreAccessRulesProtected -or $directory.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid) { throw 'directory protection' }
foreach ($entry in @($directory, $file)) {
  $rules = @($entry.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
  if ($rules.Count -ne 2) { throw 'unexpected grant count' }
  foreach ($rule in $rules) {
    if ($rule.IdentityReference.Value -notin @($sid, 'S-1-5-18') -or $rule.AccessControlType -ne 'Allow' -or $rule.FileSystemRights -ne 'FullControl') { throw 'unexpected grant' }
  }
  foreach ($who in @($sid, 'S-1-5-18')) {
    if (@($rules | Where-Object { $_.IdentityReference.Value -eq $who }).Count -ne 1) { throw 'missing principal' }
  }
}
foreach ($rule in $directory.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
  if ($rule.IsInherited -or $rule.InheritanceFlags -ne 'ContainerInherit,ObjectInherit' -or $rule.PropagationFlags -ne 'None') { throw 'directory inheritance' }
}
foreach ($rule in $file.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
  if (-not $rule.IsInherited) { throw 'file inheritance' }
}
'true'
