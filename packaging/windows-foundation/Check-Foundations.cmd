@echo off
setlocal
cd /d "%~dp0"
echo UKVM Windows 11 foundation checks - temporary fixtures only
ukvm.exe doctor --json
if errorlevel 1 goto failed
platform-tests.exe windows:: --test-threads=1
if errorlevel 1 goto failed
runtime-tests.exe ownership:: --test-threads=1
if errorlevel 1 goto failed
network-tests.exe --test-threads=1
if errorlevel 1 goto failed
echo Foundation checks passed. Native KVM input remains unimplemented.
pause
exit /b 0
:failed
echo Foundation checks failed. Keep this output for review.
pause
exit /b 1
