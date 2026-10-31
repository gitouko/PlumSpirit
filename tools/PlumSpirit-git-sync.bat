@echo off
setlocal
rem PlumSpirit: imports the newest handoff bundle into the Git of the canonical
rem repository and checks out its branch WITHOUT touching any file of the working
rem tree (fetch + symbolic-ref + mixed reset). The result is written to
rem git-sync-log.txt next to this file. Keep this file in the PlumSpirit-source
rem folder (next to the bundles).
rem   v1.0-Launch bundles: PlumSpirit-v1.0-launch-handoff-*.bundle (HEAD -> v1.0-launch-dev)
rem     incremental: v1.0-launch-dev only, needs a commit this Git already has (1d89176,
rem     the v1.0-beta2 release); used first when present.
rem   v1.0-beta2 bundles: PlumSpirit-v1.0-beta2-handoff-*.bundle (HEAD -> v1.0-beta2-dev)
rem     incremental: v1.0-beta1-dev and v1.0-beta2-dev, need a commit this Git already
rem     has (8b65233); used first when present.
rem   v1.0-beta1 bundles: PlumSpirit-v1.0-beta1-handoff-*.bundle (HEAD -> v1.0-beta1-dev)
rem     full bundles carry v0.8-dev, v0.9-dev and v1.0-beta1-dev; incremental ones
rem     carry v1.0-beta1-dev only and need a commit this Git already has (b36c871).
rem     Only the branches a bundle lists are fetched (git bundle list-heads).
rem   v0.9 bundles:       PlumSpirit-v0.9-handoff-*.bundle (used only when no beta1 bundle exists)
cd /d "%~dp0"
set "REPO=%~dp0PlumSpirit-source-v0.7"
set "LOG=%~dp0git-sync-log.txt"
where git >nul 2>&1
if errorlevel 1 (
  echo Git for Windows was not found. Install it from https://git-scm.com and run this again.
  pause
  exit /b 1
)
if not exist "%REPO%\.git" (
  echo The repository folder was not found: %REPO%
  pause
  exit /b 1
)
set "B="
set "HEADREF=refs/heads/v1.0-launch-dev"
for /f "delims=" %%F in ('dir /b /o-d "PlumSpirit-v1.0-launch-handoff-*.bundle" 2^>nul') do if not defined B set "B=%%F"
if not defined B (
  set "HEADREF=refs/heads/v1.0-beta2-dev"
  for /f "delims=" %%F in ('dir /b /o-d "PlumSpirit-v1.0-beta2-handoff-*.bundle" 2^>nul') do if not defined B set "B=%%F"
)
if not defined B (
  set "HEADREF=refs/heads/v1.0-beta1-dev"
  for /f "delims=" %%F in ('dir /b /o-d "PlumSpirit-v1.0-beta1-handoff-*.bundle" 2^>nul') do if not defined B set "B=%%F"
)
if not defined B (
  set "HEADREF=refs/heads/v0.9-dev"
  for /f "delims=" %%F in ('dir /b /o-d "PlumSpirit-v0.9-handoff-*.bundle" 2^>nul') do if not defined B set "B=%%F"
)
if not defined B (
  echo No PlumSpirit handoff bundle was found in this folder.
  pause
  exit /b 1
)
> "%LOG%" echo PlumSpirit git sync %DATE% %TIME%
>>"%LOG%" echo bundle: %B%
>>"%LOG%" echo --- bundle verify
>>"%LOG%" 2>&1 git -C "%REPO%" bundle verify "%~dp0%B%"
if errorlevel 1 (
  >>"%LOG%" echo RESULT: BUNDLE NOT USABLE - nothing was changed. It is damaged, or it needs a commit this Git does not have yet.
  goto report
)
set "BRANCHES="
for /f "tokens=2" %%R in ('git bundle list-heads "%B%"') do call :addref %%R
if not defined BRANCHES (
  >>"%LOG%" echo RESULT: THE BUNDLE LISTS NO BRANCH - nothing was changed.
  goto report
)
>>"%LOG%" 2>&1 git -C "%REPO%" config core.filemode false
>>"%LOG%" echo --- fetch%BRANCHES%
>>"%LOG%" 2>&1 git -C "%REPO%" fetch --update-head-ok "%~dp0%B%"%BRANCHES%
if errorlevel 1 (
  >>"%LOG%" echo RESULT: FETCH REFUSED - nothing was changed. A local branch has commits the bundle does not have.
  goto report
)
>>"%LOG%" 2>&1 git -C "%REPO%" symbolic-ref HEAD %HEADREF%
>>"%LOG%" 2>&1 git -C "%REPO%" reset -q
>>"%LOG%" echo RESULT: OK
:report
>>"%LOG%" echo --- rev-parse --show-toplevel
>>"%LOG%" 2>&1 git -C "%REPO%" rev-parse --show-toplevel
>>"%LOG%" echo --- branch --show-current
>>"%LOG%" 2>&1 git -C "%REPO%" branch --show-current
>>"%LOG%" echo --- rev-parse HEAD
>>"%LOG%" 2>&1 git -C "%REPO%" rev-parse HEAD
>>"%LOG%" echo --- log --oneline -30
>>"%LOG%" 2>&1 git -C "%REPO%" log --oneline -30
>>"%LOG%" echo --- status --short
>>"%LOG%" 2>&1 git -C "%REPO%" status --short
>>"%LOG%" echo --- diff --stat
>>"%LOG%" 2>&1 git -C "%REPO%" diff --stat
>>"%LOG%" echo --- end
type "%LOG%"
echo.
echo The log above was saved to git-sync-log.txt.
pause
exit /b 0

:addref
set "R=%~1"
if /i "%R:~0,11%"=="refs/heads/" set "BRANCHES=%BRANCHES% %R:~11%:%R:~11%"
exit /b 0
