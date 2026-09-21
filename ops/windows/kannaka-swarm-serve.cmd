@echo off
rem Kannaka ask responder (ADR-0026 KANNAKA.ask listener) for agent "Kannaka".
rem
rem Replaces a `while true; do ...; sleep 3; done` bash loop that had been
rem started from a Claude Code Bash call in some earlier session: it worked,
rem but nothing owned it, and it would have died silently with that session
rem leaving the advertised ask capability unanswered. Launched by the
rem KannakaSwarmServe scheduled task; sibling of KannakaSwarmJoin.
rem
rem KANNAKA_ADVERTISE_ASK is deliberately NOT set here -- the capability is
rem advertised by the join daemon's presence heartbeat (#835), and this is the
rem process that answers it. KANNAKA_READONLY is likewise not set, matching
rem what the bash loop ran: unlike join, this one may write.
setlocal enabledelayedexpansion

set "KBIN=%USERPROFILE%\.local\bin\kannaka.exe"
set "ENVFILE=%USERPROFILE%\.kannaka-nats.env"
set "LOGDIR=%USERPROFILE%\.kannaka\logs"
set "LOG=%LOGDIR%\swarm-serve.log"

if not exist "%LOGDIR%" mkdir "%LOGDIR%"

rem NATS credentials. Refuse rather than start a responder that cannot reach
rem the bus: an ask advertised and unanswered is worse than one not offered.
if not exist "%ENVFILE%" (
  echo [supervisor] %DATE% %TIME% missing %ENVFILE% -- refusing to start>>"%LOG%"
  exit /b 2
)
for /f "usebackq eol=# tokens=1,* delims==" %%A in ("%ENVFILE%") do set "%%A=%%B"
if not defined NATS_USER (
  echo [supervisor] %DATE% %TIME% %ENVFILE% defines no NATS_USER -- refusing>>"%LOG%"
  exit /b 2
)

:loop
rem Keep the log bounded; this runs for weeks at a time.
for %%F in ("%LOG%") do if %%~zF GTR 5242880 move /y "%LOG%" "%LOG%.1" >nul
echo [supervisor] %DATE% %TIME% starting swarm serve>>"%LOG%"
"%KBIN%" swarm serve --agent-id Kannaka --threshold 0.3 >>"%LOG%" 2>&1
echo [supervisor] %DATE% %TIME% serve exited (%ERRORLEVEL%), restarting in 3s>>"%LOG%"
timeout /t 3 /nobreak >nul
goto loop
