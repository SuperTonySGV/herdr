@echo off
REM Rebuild Anthony's patched herdr (pinned spaces) on top of current upstream
REM and install it. This is the "yes, update" action for the notification that
REM the upstream check posts.
REM
REM Safe to run any time, including with herdr running: the installer renames
REM the live binary rather than overwriting it. The new build takes effect at
REM the next herdr restart.
powershell -NoProfile -ExecutionPolicy Bypass -File "C:\Users\Anthony\source\repos\herdr\.local\run-sync-logged.ps1" %*
