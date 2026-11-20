@echo off
setlocal
set "AFTERIMAGE_SOURCE=%~dp0afterimage.exe"
if not exist "%AFTERIMAGE_SOURCE%" (
  echo Extract the release ZIP before running Install.cmd.
  pause
  exit /b 1
)
where ffmpeg.exe >nul 2>nul || goto :missing
where ffprobe.exe >nul 2>nul || goto :missing
powershell.exe -NoProfile -Command "$ErrorActionPreference='Stop'; $dir=Join-Path $env:LOCALAPPDATA 'Programs\Afterimage\Rust-0.2.0'; $null=New-Item -ItemType Directory -Path $dir -Force; $exe=Join-Path $dir 'afterimage.exe'; if (!(Test-Path -LiteralPath $exe) -or (Get-FileHash -LiteralPath $exe).Hash -ne (Get-FileHash -LiteralPath $env:AFTERIMAGE_SOURCE).Hash) { Copy-Item -LiteralPath $env:AFTERIMAGE_SOURCE -Destination $exe -Force }; $shell=New-Object -ComObject WScript.Shell; foreach($kind in 'Startup','Programs'){ $lnk=$shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath($kind)) 'Afterimage.lnk')); $lnk.TargetPath=$exe; $lnk.WorkingDirectory=$dir; $lnk.Arguments=if($kind -eq 'Programs'){'--show'}else{''}; $lnk.IconLocation=$exe; $lnk.Save() }; if (!(Get-Process -Name afterimage -ErrorAction SilentlyContinue)) { Start-Process -FilePath $exe }; Write-Host 'Afterimage is installed. Windows will start it in the tray at sign-in.'"
if errorlevel 1 (
  echo Installation could not finish.
  pause
  exit /b 1
)
exit /b 0
:missing
echo Install FFmpeg, then open a fresh terminal or sign in again and rerun this installer.
echo With Windows Package Manager: winget install --id Gyan.FFmpeg -e
pause
exit /b 1
