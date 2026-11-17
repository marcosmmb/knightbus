# Recognize only the dependency-only folder left by older uninstallers.
function Test-RetainedDriverDirectory([string]$Install, [string]$PackageDriver) {
 $entries=@(Get-ChildItem -LiteralPath $Install -Force -ErrorAction Stop)
 if ($entries.Count -ne 1 -or $entries[0].Name -ne 'driver' -or !$entries[0].PSIsContainer) {return $false}
 $driver=$entries[0]
 if ($driver.Attributes -band [IO.FileAttributes]::ReparsePoint) {return $false}
 $retained=@(Get-ChildItem -LiteralPath $driver.FullName -Force -ErrorAction Stop)
 $expected=@(Get-ChildItem -LiteralPath $PackageDriver -Force -File -ErrorAction Stop)
 if (!$expected.Count -or $retained.Count -ne $expected.Count) {return $false}
 foreach ($file in $retained) {
  if ($file.PSIsContainer -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint)) {return $false}
  $match=$expected | Where-Object {$_.Name -ceq $file.Name}
  if (!$match -or (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $match.FullName -Algorithm SHA256).Hash) {return $false}
 }
 return $true
}
