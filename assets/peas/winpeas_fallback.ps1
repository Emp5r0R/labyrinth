Write-Output "[labyrinth] winpeas fallback enumeration"
Write-Output "timestamp: $([DateTime]::UtcNow.ToString('o'))"
Write-Output ""

Write-Output "== basic system =="
systeminfo
whoami /all
try { Get-LocalUser | Format-Table -AutoSize } catch { net user }
try {
  ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
} catch {
  Write-Output "admin check failed: $($_.Exception.Message)"
}
Write-Output ""

Write-Output "== network =="
ipconfig /all
route print
netstat -ano
Write-Output ""

Write-Output "== quick priv-esc hints =="
try { Get-Service | Where-Object { $_.Status -eq 'Running' } | Select-Object -First 200 } catch {}
try { Get-ChildItem -Path C:\ -Recurse -ErrorAction SilentlyContinue -Include *.config,*.ini,*.xml | Select-Object -First 200 } catch {}
