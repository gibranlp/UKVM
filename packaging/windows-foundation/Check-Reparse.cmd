@echo off
setlocal
cd /d "%~dp0"
echo Requires existing Windows symbolic-link privilege or Developer Mode.
platform-tests.exe windows::state::tests::ancestor_and_record_reparse_points_are_rejected --exact --ignored --test-threads=1
if errorlevel 1 goto failed
echo Reparse fixture passed.
pause
exit /b 0
:failed
echo Reparse fixture failed or its native privilege was unavailable. Keep this output.
pause
exit /b 1
