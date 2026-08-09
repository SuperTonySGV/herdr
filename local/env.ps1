# Local build environment for herdr on Anthony's Windows box.
# Dot-source before running cargo/just:  . .\.local\env.ps1
$machinePath = [Environment]::GetEnvironmentVariable('PATH', 'Machine')
$userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
$env:PATH = "$env:USERPROFILE\.cargo\bin;$userPath;$machinePath"

# herdr requires Zig 0.15.2 exactly for the vendored libghostty-vt build.
$env:ZIG = 'C:\Users\Anthony\tools\zig-x86_64-windows-0.15.2\zig.exe'

# Talk to the debug herdr-dev server, not the installed stable one.
Remove-Item Env:\HERDR_SOCKET_PATH -ErrorAction SilentlyContinue
Remove-Item Env:\HERDR_CLIENT_SOCKET_PATH -ErrorAction SilentlyContinue
