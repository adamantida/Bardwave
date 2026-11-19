@echo off
rem ==========================================================================
rem Bardwave - build menu for Windows (build.bat)
rem
rem   build.bat                  interactive menu
rem   build.bat run              cargo run --release
rem   build.bat release          release build, copied to dist\
rem   build.bat debug            debug build
rem   build.bat check            fmt --check, clippy -D warnings, tests
rem   build.bat package [zip/installer/exe/all]
rem   build.bat target TRIPLE    release build for one target, copied to dist\
rem   build.bat doctor, clean, dist
rem   old aliases: build, rel, test, fmt, clippy
rem   options: /yes -y --yes, /nocolor --no-color, /dry-run --dry-run,
rem            /help -h --help
rem
rem Needs no administrator rights. Project root = the folder of this script.
rem Keep this file ASCII-only with CRLF line endings.
rem ==========================================================================

setlocal EnableExtensions DisableDelayedExpansion
set "ROOT=%~dp0"
if not "%ROOT:!=%"=="%ROOT%" goto :bad_root
setlocal EnableExtensions EnableDelayedExpansion

if "!ROOT:~-1!"=="\" set "ROOT=!ROOT:~0,-1!"
set "SELF_NAME=%~nx0"
set "DIST=!ROOT!\dist"
set "OPT_YES="
set "OPT_DRY="
set "OPT_NOCOLOR="
set "TRIPLE_1=x86_64-pc-windows-msvc"
set "TRIPLE_2=aarch64-pc-windows-msvc"
set "TRIPLE_3=x86_64-unknown-linux-gnu"
set "TRIPLE_4=aarch64-apple-darwin"
set "TRIPLE_5=x86_64-apple-darwin"

pushd "!ROOT!" >nul 2>&1
if errorlevel 1 goto :bad_cd
call :main %*
set "FINAL_RC=!errorlevel!"
popd
exit /b !FINAL_RC!

:bad_root
echo [ERROR] The folder path of this script contains an exclamation mark, which cmd.exe cannot handle safely.
echo         Move the project to a folder without "!" in its path.
exit /b 1

:bad_cd
echo [ERROR] Cannot enter the project folder.
exit /b 1

rem ==========================================================================
rem main: argument parsing and dispatch
rem ==========================================================================
:main
set "BCMD="
set "SUB="
set "BAD_OPT="
set "BAD_ARG="
set "OPT_HELP="

:parse_args
if "%~1"=="" goto :parse_done
set "CUR=%~1"
if /i "!CUR!"=="/yes" goto :pa_yes
if /i "!CUR!"=="/y" goto :pa_yes
if /i "!CUR!"=="-y" goto :pa_yes
if /i "!CUR!"=="--yes" goto :pa_yes
if /i "!CUR!"=="/nocolor" goto :pa_nocolor
if /i "!CUR!"=="/no-color" goto :pa_nocolor
if /i "!CUR!"=="--nocolor" goto :pa_nocolor
if /i "!CUR!"=="--no-color" goto :pa_nocolor
if /i "!CUR!"=="/dry-run" goto :pa_dry
if /i "!CUR!"=="/dryrun" goto :pa_dry
if /i "!CUR!"=="--dry-run" goto :pa_dry
if /i "!CUR!"=="/help" goto :pa_help
if /i "!CUR!"=="/h" goto :pa_help
if /i "!CUR!"=="/?" goto :pa_help
if /i "!CUR!"=="-h" goto :pa_help
if /i "!CUR!"=="--help" goto :pa_help
if "!CUR:~0,1!"=="-" goto :pa_badopt
if "!CUR:~0,1!"=="/" goto :pa_badopt
if not defined BCMD goto :pa_cmd
if not defined SUB goto :pa_sub
set "BAD_ARG=!CUR!"
goto :pa_next
:pa_cmd
set "BCMD=!CUR!"
goto :pa_next
:pa_sub
set "SUB=!CUR!"
goto :pa_next
:pa_yes
set "OPT_YES=1"
goto :pa_next
:pa_nocolor
set "OPT_NOCOLOR=1"
goto :pa_next
:pa_dry
set "OPT_DRY=1"
goto :pa_next
:pa_help
set "OPT_HELP=1"
goto :pa_next
:pa_badopt
set "BAD_OPT=!CUR!"
:pa_next
shift
goto :parse_args

:parse_done
call :detect_windows
call :init_color
if defined OPT_HELP goto :do_help
if defined BAD_OPT goto :bad_option
if defined BAD_ARG goto :bad_args
if defined SUB if /i not "!BCMD!"=="package" if /i not "!BCMD!"=="target" goto :bad_args

call :gather_basic
if errorlevel 1 exit /b 1

if not defined BCMD goto :interactive

if /i "!BCMD!"=="help" goto :do_help
if /i "!BCMD!"=="run" call :cmd_run & goto :finish
if /i "!BCMD!"=="release" call :cmd_release & goto :finish
if /i "!BCMD!"=="rel" call :cmd_release & goto :finish
if /i "!BCMD!"=="debug" call :cmd_debug & goto :finish
if /i "!BCMD!"=="build" call :cmd_debug & goto :finish
if /i "!BCMD!"=="check" call :cmd_check & goto :finish
if /i "!BCMD!"=="test" call :cmd_test & goto :finish
if /i "!BCMD!"=="fmt" call :cmd_fmt & goto :finish
if /i "!BCMD!"=="clippy" call :cmd_clippy & goto :finish
if /i "!BCMD!"=="package" call :cmd_package "!SUB!" & goto :finish
if /i "!BCMD!"=="target" call :cmd_target "!SUB!" & goto :finish
if /i "!BCMD!"=="doctor" call :cmd_doctor & goto :finish
if /i "!BCMD!"=="clean" call :cmd_clean & goto :finish
if /i "!BCMD!"=="dist" call :cmd_dist & goto :finish
goto :unknown_cmd

:finish
exit /b !errorlevel!

:do_help
call :usage
exit /b 0

:bad_option
echo(!C_RED![ERROR]!C_RESET! Unknown option: !BAD_OPT!
call :usage
exit /b 2

:bad_args
echo(!C_RED![ERROR]!C_RESET! Unexpected argument for this command.
call :usage
exit /b 2

:unknown_cmd
echo(!C_RED![ERROR]!C_RESET! Unknown command: !BCMD!
call :usage
exit /b 2

rem ==========================================================================
rem usage
rem ==========================================================================
:usage
echo(
echo(Usage: build.bat [command] [options]
echo(
echo(Without a command the interactive menu is shown.
echo(
echo(Commands:
echo(  run                        cargo run --release
echo(  release                    release build for this machine, copied to dist\  (alias: rel)
echo(  debug                      debug build  (alias: build)
echo(  check                      fmt --check, clippy -D warnings, tests; stops at the first failure
echo(  package [zip^|installer^|exe^|all]
echo(                             zip = portable .zip with config.toml
echo(                             installer = Inno Setup installer (needs ISCC)
echo(                             exe = single renamed .exe
echo(                             all = everything available (default)
echo(  target ^<triple^>            release build for one target, copied to dist\
echo(                             x86_64-pc-windows-msvc, aarch64-pc-windows-msvc,
echo(                             x86_64-unknown-linux-gnu, aarch64-apple-darwin, x86_64-apple-darwin
echo(  doctor                     check the build environment and show how to install what is missing
echo(  clean                      cargo clean and delete dist\  (asks first)
echo(  dist                       open the dist folder in Explorer
echo(  test ^| fmt ^| clippy        old aliases: cargo test / fmt --check / clippy --all-targets
echo(
echo(Options:
echo(  /yes, -y, --yes            do not ask for confirmation
echo(  /nocolor, --no-color       no colors (also off when NO_COLOR is set)
echo(  /dry-run, --dry-run        print the commands but do not run them
echo(  /help, -h, --help          this text
echo(
echo(Exit codes: 0 = ok, 1 = a step failed, 2 = wrong usage.
exit /b 0

rem ==========================================================================
rem environment detection
rem ==========================================================================
:detect_windows
set "WINV="
set "WINMAJ="
set "WINBUILD="
for /f "tokens=2 delims=[]" %%v in ('ver') do for /f "tokens=2" %%w in ("%%v") do set "WINV=%%w"
if not defined WINV exit /b 0
for /f "tokens=1,3 delims=." %%a in ("!WINV!") do (set "WINMAJ=%%a" & set "WINBUILD=%%b")
exit /b 0

:init_color
set "C_RESET="
set "C_BOLD="
set "C_DIM="
set "C_RED="
set "C_GREEN="
set "C_YELLOW="
set "C_CYAN="
if defined OPT_NOCOLOR exit /b 0
if defined NO_COLOR exit /b 0
if not defined WINMAJ exit /b 0
if not defined WINBUILD exit /b 0
if !WINMAJ! LSS 10 exit /b 0
if !WINMAJ! EQU 10 if !WINBUILD! LSS 15063 exit /b 0
set "ESC="
for /f "delims=#" %%E in ('"prompt #$E# & for %%e in (1) do rem"') do set "ESC=%%E"
if not defined ESC exit /b 0
set "C_RESET=!ESC![0m"
set "C_BOLD=!ESC![1m"
set "C_DIM=!ESC![90m"
set "C_RED=!ESC![91m"
set "C_GREEN=!ESC![92m"
set "C_YELLOW=!ESC![93m"
set "C_CYAN=!ESC![96m"
exit /b 0

:gather_basic
rem Host architecture from the environment (refined from rustc later).
set "HOST_ARCH=x86_64"
set "PA=!PROCESSOR_ARCHITECTURE!"
if defined PROCESSOR_ARCHITEW6432 set "PA=!PROCESSOR_ARCHITEW6432!"
if /i "!PA!"=="ARM64" set "HOST_ARCH=aarch64"
if /i "!PA!"=="x86" set "HOST_ARCH=x86"
set "OSARCH=windows-!HOST_ARCH!"

rem Cargo from rustup, even when its folder is not in PATH.
where cargo >nul 2>&1
if not errorlevel 1 goto :gb_cargo_ok
if exist "!USERPROFILE!\.cargo\bin\cargo.exe" set "PATH=!USERPROFILE!\.cargo\bin;!PATH!"
:gb_cargo_ok

rem PowerShell (Windows PowerShell first, then PowerShell 7).
set "PSH="
where powershell >nul 2>&1
if not errorlevel 1 set "PSH=powershell"
if defined PSH goto :gb_ps_done
where pwsh >nul 2>&1
if not errorlevel 1 set "PSH=pwsh"
:gb_ps_done

rem Version from the [package] table of Cargo.toml (first line "version = ...").
set "VERSION="
if not exist "Cargo.toml" goto :gb_nocargotoml
for /f "usebackq tokens=2 delims== " %%v in (`findstr /b /c:"version = " "Cargo.toml"`) do if not defined VERSION set "VERSION=%%~v"
if not defined VERSION goto :gb_noversion
set "VER_NUM=!VERSION!.0"
for /f "tokens=1 delims=-+" %%n in ("!VERSION!") do set "VER_NUM=%%n.0"

rem Cargo target directory (CARGO_TARGET_DIR is honoured).
set "TARGET_DIR=!ROOT!\target"
if defined CARGO_TARGET_DIR for %%i in ("!CARGO_TARGET_DIR!") do set "TARGET_DIR=%%~fi"
set "REL_EXE=!TARGET_DIR!\release\soundshared.exe"
set "DBG_EXE=!TARGET_DIR!\debug\soundshared.exe"
exit /b 0

:gb_nocargotoml
echo(!C_RED![ERROR]!C_RESET! Cargo.toml was not found in the project folder: !ROOT!
exit /b 1

:gb_noversion
echo(!C_RED![ERROR]!C_RESET! Could not read the version from Cargo.toml.
exit /b 1

:gather_rust
if defined RUST_GATHERED exit /b 0
set "RUST_GATHERED=1"
set "RUSTC_VER="
set "HOST_TRIPLE="
set "SYSROOT="
where rustc >nul 2>&1
if errorlevel 1 goto :gr_done
for /f "tokens=2" %%v in ('rustc --version 2^>nul') do if not defined RUSTC_VER set "RUSTC_VER=%%v"
for /f "tokens=2" %%h in ('rustc -vV 2^>nul ^| findstr /b /c:"host:"') do set "HOST_TRIPLE=%%h"
for /f "delims=" %%s in ('rustc --print sysroot 2^>nul') do set "SYSROOT=%%s"
if not defined HOST_TRIPLE goto :gr_done
for /f "delims=-" %%a in ("!HOST_TRIPLE!") do set "HOST_ARCH=%%a"
if /i "!HOST_ARCH!"=="i686" set "HOST_ARCH=x86"
set "OSARCH=windows-!HOST_ARCH!"
:gr_done
if not defined HOST_TRIPLE set "HOST_TRIPLE=unknown"
if not defined RUSTC_VER set "RUSTC_VER=not found"
exit /b 0

:gather_git
if defined GIT_GATHERED exit /b 0
set "GIT_GATHERED=1"
set "GIT_INFO=not a git repository"
where git >nul 2>&1
if errorlevel 1 goto :gg_nogit
git rev-parse --is-inside-work-tree >nul 2>&1
if errorlevel 1 exit /b 0
set "GB="
set "GH="
for /f "delims=" %%b in ('git rev-parse --abbrev-ref HEAD 2^>nul') do set "GB=%%b"
for /f "delims=" %%h in ('git rev-parse --short HEAD 2^>nul') do set "GH=%%h"
if not defined GH set "GIT_INFO=no commits yet"
if defined GH set "GIT_INFO=!GB! @ !GH!"
exit /b 0
:gg_nogit
set "GIT_INFO=git not installed"
exit /b 0

rem ==========================================================================
rem small output helpers (message texts must not contain the characters and, pipe, less, greater)
rem ==========================================================================
:info
echo(!C_CYAN![INFO]!C_RESET! %~1
exit /b 0

:ok
echo(!C_GREEN![ OK ]!C_RESET! %~1
exit /b 0

:warn
echo(!C_YELLOW![WARN]!C_RESET! %~1
exit /b 0

:err
echo(!C_RED![ERROR]!C_RESET! %~1
exit /b 0

rem run a command (or only print it with /dry-run); returns its exit code
:run
if "!OPT_DRY!"=="1" goto :run_dry
echo(!C_DIM!^> %*!C_RESET!
%*
set "RUN_RC=!errorlevel!"
if !RUN_RC! LSS 0 set "RUN_RC=1"
exit /b !RUN_RC!
:run_dry
echo(!C_YELLOW![dry-run]!C_RESET! %*
exit /b 0

:fs_mkdir
if exist "%~1\" exit /b 0
echo(!C_DIM!^> mkdir "%~1"!C_RESET!
if "!OPT_DRY!"=="1" exit /b 0
mkdir "%~1"
exit /b !errorlevel!

:fs_rmdir
if not exist "%~1\" exit /b 0
echo(!C_DIM!^> rmdir /s /q "%~1"!C_RESET!
if "!OPT_DRY!"=="1" exit /b 0
rmdir /s /q "%~1"
exit /b !errorlevel!

:fs_copy
echo(!C_DIM!^> copy "%~1" "%~2"!C_RESET!
if "!OPT_DRY!"=="1" exit /b 0
copy /y "%~1" "%~2" >nul
exit /b !errorlevel!

:fs_del
if not exist "%~1" exit /b 0
echo(!C_DIM!^> del "%~1"!C_RESET!
if "!OPT_DRY!"=="1" exit /b 0
del /f /q "%~1"
exit /b !errorlevel!

rem y/N question; --yes and --dry-run answer yes
:confirm
if "!OPT_YES!"=="1" exit /b 0
if "!OPT_DRY!"=="1" exit /b 0
set "ANS="
set /p "ANS=%~1 [y/N]: "
if /i "!ANS!"=="y" exit /b 0
if /i "!ANS!"=="yes" exit /b 0
exit /b 1

rem read a menu answer into ANS; many empty answers in a row (stdin closed) turn into q
:ask
set "ANS="
set /p "ANS=%~1"
if defined ANS goto :ask_got
set /a EMPTY_N+=1
if !EMPTY_N! LSS 26 exit /b 0
if !EMPTY_N! EQU 26 echo(& echo(No input available, leaving the menu.
set "ANS=q"
exit /b 0
:ask_got
set "EMPTY_N=0"
exit /b 0

:need_cargo
where cargo >nul 2>&1
if not errorlevel 1 exit /b 0
call :err "cargo was not found in PATH."
echo(        Install Rust: winget install -e --id Rustlang.Rustup   ^(or https://rustup.rs^)
echo(        Then open a new terminal and run this script again.
exit /b 1

:fail_build
call :err "The build failed."
exit /b 1

rem ==========================================================================
rem header and menu
rem ==========================================================================
:header
call :gather_rust
call :gather_git
echo(
echo(+--------------------------------------------------------------------+
echo(^|  !C_BOLD!Bardwave - build menu!C_RESET!
echo(+--------------------------------------------------------------------+
echo(^|  Version : !VERSION!  (crate soundshared)
echo(^|  Host    : Windows !WINV! / !HOST_ARCH!  (!HOST_TRIPLE!)
echo(^|  rustc   : !RUSTC_VER!
echo(^|  git     : !GIT_INFO!
echo(^|  dist    : !DIST!
echo(+--------------------------------------------------------------------+
exit /b 0

:menu_print
echo(
echo(  1) Run                 cargo run --release
echo(  2) Build release       this machine, binary copied to dist\
echo(  3) Build debug
echo(  4) Check               fmt --check, clippy -D warnings, tests
echo(  5) Package             zip, installer, single exe
echo(  6) Targets             choose a target platform
echo(  7) Doctor              check the environment
echo(  8) Clean               cargo clean and remove dist\
echo(  9) Open dist folder
echo(  q) Quit
echo(
exit /b 0

:interactive
set "EMPTY_N=0"
:menu_loop
call :header
call :menu_print
call :ask "Select an option [1-9, q]: "
if not defined ANS goto :menu_loop
set "SKIP_PAUSE="
if /i "!ANS!"=="q" exit /b 0
if /i "!ANS!"=="quit" exit /b 0
if /i "!ANS!"=="exit" exit /b 0
if "!ANS!"=="1" call :cmd_run & goto :menu_after
if "!ANS!"=="2" call :cmd_release & goto :menu_after
if "!ANS!"=="3" call :cmd_debug & goto :menu_after
if "!ANS!"=="4" call :cmd_check & goto :menu_after
if "!ANS!"=="5" call :menu_package & goto :menu_after
if "!ANS!"=="6" call :menu_targets & goto :menu_after
if "!ANS!"=="7" call :cmd_doctor & goto :menu_after
if "!ANS!"=="8" call :cmd_clean & goto :menu_after
if "!ANS!"=="9" call :cmd_dist & goto :menu_after
echo(!C_YELLOW![WARN]!C_RESET! Unknown choice: !ANS!
goto :menu_loop

:menu_after
set "LAST_RC=!errorlevel!"
if "!SKIP_PAUSE!"=="1" goto :menu_loop
if not "!LAST_RC!"=="0" echo(!C_YELLOW![WARN]!C_RESET! The last action finished with exit code !LAST_RC!.
echo(
echo(Press any key to return to the menu...
pause >nul
goto :menu_loop

:menu_package
echo(
echo(!C_BOLD!Package!C_RESET!
echo(  [1] Portable .zip with config.toml
echo(  [2] Installer (Inno Setup, ISCC)
echo(  [3] Single .exe (renamed copy)
echo(  [a] All available
echo(  [0] Back
call :ask "Package [1/2/3/a/0]: "
if not defined ANS goto :menu_package
if "!ANS!"=="0" goto :mp_back
if /i "!ANS!"=="q" goto :mp_back
if /i "!ANS!"=="b" goto :mp_back
if "!ANS!"=="1" call :cmd_package zip & exit /b !errorlevel!
if "!ANS!"=="2" call :cmd_package installer & exit /b !errorlevel!
if "!ANS!"=="3" call :cmd_package exe & exit /b !errorlevel!
if /i "!ANS!"=="a" call :cmd_package all & exit /b !errorlevel!
echo(!C_YELLOW![WARN]!C_RESET! Unknown choice: !ANS!
goto :menu_package
:mp_back
set "SKIP_PAUSE=1"
exit /b 0

:menu_targets
call :gather_rust
echo(
echo(!C_BOLD!Targets!C_RESET!   host: !HOST_TRIPLE!
echo(
for /l %%i in (1,1,5) do call :target_line %%i
echo(    0) Back
echo(
call :ask "Select a target [1-5, 0]: "
if not defined ANS goto :menu_targets
if "!ANS!"=="0" goto :mt_back
if /i "!ANS!"=="q" goto :mt_back
if /i "!ANS!"=="b" goto :mt_back
set "TSEL="
for /l %%i in (1,1,5) do if "!ANS!"=="%%i" set "TSEL=!TRIPLE_%%i!"
if not defined TSEL goto :mt_bad
call :build_target "!TSEL!"
exit /b !errorlevel!
:mt_bad
echo(!C_YELLOW![WARN]!C_RESET! Unknown choice: !ANS!
goto :menu_targets
:mt_back
set "SKIP_PAUSE=1"
exit /b 0

:target_line
set "TLN=!TRIPLE_%~1!"
call :target_status "!TLN!"
set "TLPAD=!TLN!                              "
set "TLHOST="
if /i "!TLN!"=="!HOST_TRIPLE!" set "TLHOST= [host]"
echo(    %~1) !TLPAD:~0,28! !TS_COL!!TS_TXT!!C_RESET!!TLHOST!
exit /b 0

rem sets TS_CODE (1 installed, 2 not installed, 3 not buildable here), TS_TXT, TS_COL
:target_status
set "TS_CODE=3"
set "TS_TXT=not buildable on this host"
set "TS_COL=!C_RED!"
set "TSX=%~1"
if /i not "!TSX!"=="x86_64-pc-windows-msvc" if /i not "!TSX!"=="aarch64-pc-windows-msvc" exit /b 0
set "TS_CODE=2"
set "TS_TXT=not installed"
set "TS_COL=!C_YELLOW!"
if defined SYSROOT if exist "!SYSROOT!\lib\rustlib\!TSX!\lib\" set "TS_CODE=1" & set "TS_TXT=installed" & set "TS_COL=!C_GREEN!"
exit /b 0

rem ==========================================================================
rem commands
rem ==========================================================================
:cmd_run
call :need_cargo
if errorlevel 1 exit /b 1
call :info "Starting Bardwave: cargo run --release"
call :run cargo run --release
set "RC=!errorlevel!"
if not "!RC!"=="0" call :err "cargo run failed or the program exited with an error."
exit /b !RC!

:cmd_release
call :need_cargo
if errorlevel 1 exit /b 1
call :gather_rust
call :info "Building a release binary for this machine ..."
call :run cargo build --release
if errorlevel 1 goto :fail_build
call :copy_to_dist "!REL_EXE!" "!OSARCH!"
if errorlevel 1 exit /b 1
call :finalize
exit /b 0

:cmd_debug
call :need_cargo
if errorlevel 1 exit /b 1
call :info "Building a debug binary ..."
call :run cargo build
if errorlevel 1 goto :fail_build
call :ok "Debug build finished."
echo(        Binary: !DBG_EXE!
exit /b 0

:cmd_test
call :need_cargo
if errorlevel 1 exit /b 1
call :info "Running tests ..."
call :run cargo test --all-targets
if errorlevel 1 goto :test_failed
call :ok "All tests passed."
exit /b 0
:test_failed
call :err "Tests failed."
exit /b 1

:cmd_fmt
call :need_cargo
if errorlevel 1 exit /b 1
call :info "Checking formatting ..."
call :run cargo fmt --check
if errorlevel 1 goto :fmt_failed
call :ok "Formatting is fine."
exit /b 0
:fmt_failed
call :warn "Formatting issues found. Run cargo fmt to fix them."
exit /b 1

:cmd_clippy
call :need_cargo
if errorlevel 1 exit /b 1
call :info "Running clippy ..."
call :run cargo clippy --all-targets
if errorlevel 1 goto :clippy_failed
call :ok "Clippy is fine."
exit /b 0
:clippy_failed
call :err "Clippy found problems."
exit /b 1

:cmd_check
call :need_cargo
if errorlevel 1 exit /b 1
set "R_FMT=skipped"
set "R_CLP=skipped"
set "R_TST=skipped"
set "CHK_RC=0"
call :info "Check 1/3: cargo fmt --check"
call :run cargo fmt --check
if errorlevel 1 goto :chk_fail_fmt
set "R_FMT=PASS"
call :info "Check 2/3: cargo clippy --all-targets -- -D warnings"
call :run cargo clippy --all-targets -- -D warnings
if errorlevel 1 goto :chk_fail_clp
set "R_CLP=PASS"
call :info "Check 3/3: cargo test --all-targets"
call :run cargo test --all-targets
if errorlevel 1 goto :chk_fail_tst
set "R_TST=PASS"
goto :chk_report
:chk_fail_fmt
set "R_FMT=FAIL"
set "CHK_RC=1"
goto :chk_report
:chk_fail_clp
set "R_CLP=FAIL"
set "CHK_RC=1"
goto :chk_report
:chk_fail_tst
set "R_TST=FAIL"
set "CHK_RC=1"
:chk_report
if "!OPT_DRY!"=="1" set "R_FMT=dry-run" & set "R_CLP=dry-run" & set "R_TST=dry-run"
echo(
echo(!C_BOLD!Check results!C_RESET!
echo(  ------------------------------------------  -------
call :chk_row "cargo fmt --check" "!R_FMT!"
call :chk_row "cargo clippy --all-targets -- -D warnings" "!R_CLP!"
call :chk_row "cargo test --all-targets" "!R_TST!"
echo(  ------------------------------------------  -------
if not "!CHK_RC!"=="0" goto :chk_failed
call :ok "Check finished."
exit /b 0
:chk_failed
call :err "Check failed. The first failing step stopped the run."
exit /b 1

:chk_row
set "CRN=%~1                                          "
set "CRC=!C_YELLOW!"
if "%~2"=="PASS" set "CRC=!C_GREEN!"
if "%~2"=="FAIL" set "CRC=!C_RED!"
echo(  !CRN:~0,42!  !CRC!%~2!C_RESET!
exit /b 0

:cmd_dist
call :fs_mkdir "!DIST!"
call :info "Opening the dist folder."
echo(        !DIST!
call :run explorer "!DIST!"
exit /b 0

:cmd_clean
call :need_cargo
if errorlevel 1 exit /b 1
echo(
echo(This will run cargo clean and delete the folder:
echo(  !DIST!
call :confirm "Continue?"
if errorlevel 1 goto :clean_cancel
call :run cargo clean
if errorlevel 1 goto :clean_fail
call :fs_rmdir "!DIST!"
if errorlevel 1 goto :clean_fail
call :ok "Clean finished."
exit /b 0
:clean_cancel
call :warn "Cancelled. Nothing was deleted."
exit /b 1
:clean_fail
call :err "Clean failed."
exit /b 1

rem ==========================================================================
rem dist helpers: copy, SHA256SUMS, summary
rem ==========================================================================
rem copy_to_dist SOURCE_EXE LABEL: copies to dist, file name Bardwave-v(version)-(LABEL).exe
:copy_to_dist
set "CD_SRC=%~1"
set "CD_DST=!DIST!\Bardwave-v!VERSION!-%~2.exe"
if "!OPT_DRY!"=="1" goto :cd_do
if not exist "!CD_SRC!" goto :cd_missing
:cd_do
call :fs_mkdir "!DIST!"
call :fs_copy "!CD_SRC!" "!CD_DST!"
if errorlevel 1 goto :cd_failed
exit /b 0
:cd_missing
echo(!C_RED![ERROR]!C_RESET! Build output not found: !CD_SRC!
exit /b 1
:cd_failed
call :err "Could not copy the binary into dist."
exit /b 1

:finalize
if "!OPT_DRY!"=="1" goto :fin_dry
call :write_sums
call :summary
exit /b 0
:fin_dry
echo(
call :info "Dry run: nothing was built or written, no SHA256SUMS."
exit /b 0

rem SHA256SUMS for every file in dist (sha256sum format, lower-case hash, two spaces, bare name)
:write_sums
if not exist "!DIST!\*" exit /b 0
if not defined PSH goto :ws_nops
set "BW_DIST=!DIST!"
call :info "Writing dist\SHA256SUMS ..."
"!PSH!" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$ErrorActionPreference='Stop'; $d=$env:BW_DIST; $n=@(Get-ChildItem -LiteralPath $d -File | Where-Object { $_.Name -ne 'SHA256SUMS' } | ForEach-Object { $_.Name }); [Array]::Sort($n,[StringComparer]::Ordinal); $o=New-Object System.Text.StringBuilder; foreach($f in $n){ $h=(Get-FileHash -LiteralPath (Join-Path $d $f) -Algorithm SHA256).Hash.ToLower(); [void]$o.Append($h+'  '+$f+[char]10) }; [IO.File]::WriteAllText((Join-Path $d 'SHA256SUMS'),$o.ToString(),(New-Object System.Text.UTF8Encoding($false)))"
if errorlevel 1 call :warn "Could not write SHA256SUMS."
exit /b 0
:ws_nops
call :warn "PowerShell was not found, SHA256SUMS was not written."
exit /b 0

:summary
echo(
echo(!C_BOLD!Summary!C_RESET!
echo(  Output folder: !DIST!
if not exist "!DIST!\*" goto :sum_empty
for %%F in ("!DIST!\*") do call :sum_line "%%~nxF" %%~zF
echo(
exit /b 0
:sum_empty
echo(  dist is empty.
exit /b 0

:sum_line
set "SZ_NAME=%~1                                                                  "
set /a SZ_KB=%~2/1024
set /a SZ_MB=SZ_KB/1024
set /a SZ_MF=(SZ_KB %% 1024)*10/1024
set "SZ_TXT=!SZ_KB! KB"
if !SZ_MB! GTR 0 set "SZ_TXT=!SZ_MB!.!SZ_MF! MB"
echo(    !SZ_NAME:~0,56!  !SZ_TXT!
exit /b 0

rem ==========================================================================
rem package
rem ==========================================================================
:find_iscc
set "ISCC="
for /f "delims=" %%I in ('where iscc.exe 2^>nul') do if not defined ISCC set "ISCC=%%I"
if defined ISCC exit /b 0
for %%D in ("%ProgramFiles(x86)%" "%ProgramFiles%" "%LOCALAPPDATA%\Programs") do if exist "%%~D\Inno Setup 6\ISCC.exe" if not defined ISCC set "ISCC=%%~D\Inno Setup 6\ISCC.exe"
exit /b 0

:iscc_hint
echo(        Install Inno Setup 6 (no administrator rights needed for a per-user install):
echo(          winget install -e --id JRSoftware.InnoSetup
echo(          choco install innosetup
echo(          or download it from https://jrsoftware.org/isdl.php
exit /b 0

:cmd_package
set "PSUB=%~1"
if not defined PSUB set "PSUB=all"
set "PK_ALL="
set "PK_LIST="
if /i "!PSUB!"=="zip" set "PK_LIST=zip"
if /i "!PSUB!"=="installer" set "PK_LIST=installer"
if /i "!PSUB!"=="exe" set "PK_LIST=exe"
if /i "!PSUB!"=="all" set "PK_LIST=zip installer exe" & set "PK_ALL=1"
if not defined PK_LIST goto :pk_badsub
call :need_cargo
if errorlevel 1 exit /b 1
call :gather_rust
call :info "Building the release binary ..."
call :run cargo build --release
if errorlevel 1 goto :fail_build
set "PK_RC=0"
for %%S in (%PK_LIST%) do (
    call :pkg_%%S
    if errorlevel 1 set "PK_RC=1"
)
call :finalize
if not "!PK_RC!"=="0" call :err "One or more packaging steps failed."
exit /b !PK_RC!
:pk_badsub
echo(!C_RED![ERROR]!C_RESET! Unknown package type: !PSUB!
call :usage
exit /b 2

:pkg_exe
call :info "Packaging the single .exe ..."
call :copy_to_dist "!REL_EXE!" "!OSARCH!"
exit /b !errorlevel!

:pkg_zip
call :info "Packaging the portable .zip ..."
if not defined PSH goto :pz_nops
set "PZ_NAME=Bardwave-v!VERSION!-!OSARCH!"
set "PZ_ROOT=!TARGET_DIR!\bardwave-stage"
set "PZ_DIR=!PZ_ROOT!\!PZ_NAME!"
set "PZ_ZIP=!DIST!\!PZ_NAME!.zip"
set "PZ_BAD="
if "!OPT_DRY!"=="1" goto :pz_go
for %%F in ("!REL_EXE!" "!ROOT!\README.md" "!ROOT!\LICENSE" "!ROOT!\packaging\config.toml" "!ROOT!\packaging\README.txt") do if not exist "%%~F" call :pz_missing "%%~F"
if defined PZ_BAD exit /b 1
:pz_go
call :fs_rmdir "!PZ_ROOT!"
call :fs_mkdir "!PZ_DIR!"
call :fs_copy "!REL_EXE!" "!PZ_DIR!\Bardwave.exe"
if errorlevel 1 goto :pz_fail
call :fs_copy "!ROOT!\README.md" "!PZ_DIR!\README.md"
if errorlevel 1 goto :pz_fail
call :fs_copy "!ROOT!\LICENSE" "!PZ_DIR!\LICENSE"
if errorlevel 1 goto :pz_fail
call :fs_copy "!ROOT!\packaging\config.toml" "!PZ_DIR!\config.toml"
if errorlevel 1 goto :pz_fail
call :fs_copy "!ROOT!\packaging\README.txt" "!PZ_DIR!\README.txt"
if errorlevel 1 goto :pz_fail
call :fs_mkdir "!DIST!"
call :fs_del "!PZ_ZIP!"
set "BW_SRC=!PZ_DIR!"
set "BW_DST=!PZ_ZIP!"
call :run "!PSH!" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$ErrorActionPreference='Stop'; Compress-Archive -LiteralPath $env:BW_SRC -DestinationPath $env:BW_DST -Force"
set "PZ_RC=!errorlevel!"
call :fs_rmdir "!PZ_ROOT!"
if not "!PZ_RC!"=="0" goto :pz_fail
exit /b 0
:pz_fail
call :err "Could not create the zip."
exit /b 1
:pz_nops
call :err "PowerShell was not found, the zip cannot be created."
exit /b 1
:pz_missing
set "PZ_BAD=1"
set "PZ_M=%~1"
echo(!C_RED![ERROR]!C_RESET! Missing file: !PZ_M!
exit /b 0

:pkg_installer
call :info "Building the Inno Setup installer ..."
if /i not "!HOST_ARCH!"=="x86_64" goto :pi_arch
call :find_iscc
if not defined ISCC goto :pi_noiscc
call :fs_mkdir "!DIST!"
call :run "!ISCC!" "/DMyAppVersion=!VERSION!" "/DMyAppNumVersion=!VER_NUM!" "/O!DIST!" "!ROOT!\packaging\windows\bardwave.iss"
if errorlevel 1 goto :pi_fail
if "!OPT_DRY!"=="1" exit /b 0
set "PI_OUT=!DIST!\Bardwave-v!VERSION!-windows-x86_64-setup.exe"
if not exist "!PI_OUT!" goto :pi_noout
exit /b 0
:pi_arch
call :warn "The installer is x86_64 only; this host is not x86_64. Skipped."
if "!PK_ALL!"=="1" exit /b 0
exit /b 1
:pi_noiscc
call :warn "Inno Setup compiler (ISCC.exe) was not found."
call :iscc_hint
if "!PK_ALL!"=="1" exit /b 0
exit /b 1
:pi_fail
call :err "Inno Setup failed."
exit /b 1
:pi_noout
echo(!C_RED![ERROR]!C_RESET! Inno Setup finished but the expected file is missing: !PI_OUT!
exit /b 1

rem ==========================================================================
rem targets
rem ==========================================================================
:cmd_target
set "TG=%~1"
if not defined TG goto :tg_noarg
call :gather_rust
set "TG_OK="
for /l %%i in (1,1,5) do if /i "!TG!"=="!TRIPLE_%%i!" set "TG=!TRIPLE_%%i!" & set "TG_OK=1"
if not defined TG_OK goto :tg_unknown
call :build_target "!TG!"
exit /b !errorlevel!
:tg_noarg
call :err "The target command needs a target triple."
call :usage
exit /b 2
:tg_unknown
echo(!C_RED![ERROR]!C_RESET! Unknown target: !TG!
echo(        Known targets:
for /l %%i in (1,1,5) do echo(          !TRIPLE_%%i!
exit /b 2

:build_target
set "BT=%~1"
call :need_cargo
if errorlevel 1 exit /b 1
call :gather_rust
call :target_status "!BT!"
if "!TS_CODE!"=="3" goto :bt_unbuildable
if "!TS_CODE!"=="2" goto :bt_add
:bt_build
if /i "!BT!"=="aarch64-pc-windows-msvc" call :info "This target needs the MSVC ARM64 build tools (Visual Studio Installer, component C++ ARM64 build tools)."
call :info "Building release for !BT! ..."
call :run cargo build --release --target !BT!
if errorlevel 1 goto :fail_build
for /f "delims=-" %%a in ("!BT!") do set "BT_LABEL=windows-%%a"
call :copy_to_dist "!TARGET_DIR!\!BT!\release\soundshared.exe" "!BT_LABEL!"
if errorlevel 1 exit /b 1
call :finalize
exit /b 0
:bt_add
echo(!C_YELLOW![WARN]!C_RESET! The Rust target !BT! is not installed.
where rustup >nul 2>&1
if errorlevel 1 goto :bt_norustup
call :confirm "Run rustup target add !BT! ?"
if errorlevel 1 goto :bt_declined
call :run rustup target add !BT!
if errorlevel 1 goto :bt_addfail
goto :bt_build
:bt_norustup
call :err "rustup was not found, so the target cannot be added."
echo(        Install rustup: winget install -e --id Rustlang.Rustup   ^(or https://rustup.rs^)
echo(        Then run: rustup target add !BT!
exit /b 1
:bt_declined
call :warn "Cancelled. The target was not added and nothing was built."
exit /b 1
:bt_addfail
call :err "rustup target add failed."
exit /b 1
:bt_unbuildable
echo(!C_RED![ERROR]!C_RESET! !BT! is not buildable on this host.
echo(        Cross-compiling to another operating system is not supported by this script:
echo(        it needs that platform's linker and SDK. Nothing was built.
echo(        Build it on a matching machine instead (macOS or Linux: ./build.sh target !BT!).
exit /b 1

rem ==========================================================================
rem doctor
rem ==========================================================================
:find_msvc
set "MSVC_PATH="
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "!VSWHERE!" set "VSWHERE=%ProgramFiles%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "!VSWHERE!" goto :fm_link
set "VSOUT=!TEMP!\bardwave-vswhere-!RANDOM!.txt"
"!VSWHERE!" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath > "!VSOUT!" 2>nul
if exist "!VSOUT!" set /p MSVC_PATH=<"!VSOUT!"
if exist "!VSOUT!" del /f /q "!VSOUT!" >nul 2>&1
if defined MSVC_PATH exit /b 0
:fm_link
for /f "delims=" %%L in ('where link.exe 2^>nul') do call :chk_link "%%L"
exit /b 0
:chk_link
rem Ignore the link.exe of Git for Windows (usr\bin), it is not the MSVC linker.
set "LK=%~1"
set "LK2=!LK:\usr\bin\=!"
if "!LK2!"=="!LK!" if not defined MSVC_PATH set "MSVC_PATH=!LK!"
exit /b 0

rem row NAME STATUS, detail text in ROW_D
:row
set "RN=%~1                                        "
set "RS=%~2          "
set "RCOL=!C_GREEN!"
if /i "%~2"=="MISSING" set "RCOL=!C_RED!"
echo(  !RN:~0,26!  !RCOL!!RS:~0,8!!C_RESET!  !ROW_D!
exit /b 0

:hint
echo(!C_YELLOW!      how to install:!C_RESET! %~1
exit /b 0

:cmd_doctor
call :gather_rust
call :gather_git
set "DOC_BAD=0"
echo(
echo(!C_BOLD!Doctor: build environment!C_RESET!
echo(
echo(  Component                   Status    Detail
echo(  --------------------------  --------  ------------------------------------------
rem --- rustc, at least 1.85
if "!RUSTC_VER!"=="not found" goto :doc_rustc_missing
set "RV_MAJ=0"
set "RV_MIN=0"
for /f "tokens=1,2 delims=." %%a in ("!RUSTC_VER!") do (set "RV_MAJ=%%a" & set "RV_MIN=%%b")
set /a RV_N=RV_MAJ*1000+RV_MIN
if !RV_N! LSS 1085 goto :doc_rustc_old
set "ROW_D=!RUSTC_VER!"
call :row "rustc >= 1.85" OK
goto :doc_cargo
:doc_rustc_old
set "ROW_D=found !RUSTC_VER!, need 1.85 or newer"
call :row "rustc >= 1.85" MISSING
call :hint "rustup update stable"
set "DOC_BAD=1"
goto :doc_cargo
:doc_rustc_missing
set "ROW_D=not found in PATH"
call :row "rustc >= 1.85" MISSING
call :hint "winget install -e --id Rustlang.Rustup   (or https://rustup.rs)"
set "DOC_BAD=1"
:doc_cargo
rem --- cargo
set "CV="
where cargo >nul 2>&1
if errorlevel 1 goto :doc_cargo_missing
for /f "tokens=2" %%v in ('cargo --version 2^>nul') do set "CV=%%v"
set "ROW_D=cargo !CV!"
call :row "cargo" OK
goto :doc_git
:doc_cargo_missing
set "ROW_D=not found in PATH"
call :row "cargo" MISSING
call :hint "winget install -e --id Rustlang.Rustup   (or https://rustup.rs)"
set "DOC_BAD=1"
:doc_git
rem --- git
where git >nul 2>&1
if errorlevel 1 goto :doc_git_missing
set "GV="
for /f "tokens=3" %%v in ('git --version 2^>nul') do set "GV=%%v"
set "ROW_D=git !GV!"
call :row "git" OK
goto :doc_msvc
:doc_git_missing
set "ROW_D=not found in PATH (optional, only for the menu header)"
call :row "git" MISSING
call :hint "winget install -e --id Git.Git"
:doc_msvc
rem --- MSVC build tools
call :find_msvc
if not defined MSVC_PATH goto :doc_msvc_missing
set "ROW_D=!MSVC_PATH!"
call :row "MSVC build tools" OK
goto :doc_iscc
:doc_msvc_missing
set "ROW_D=link.exe / Visual Studio C++ tools not found"
call :row "MSVC build tools" MISSING
call :hint "winget install -e --id Microsoft.VisualStudio.2022.BuildTools"
echo(        then add the workload "Desktop development with C++" (https://visualstudio.microsoft.com/visual-cpp-build-tools/)
set "DOC_BAD=1"
:doc_iscc
rem --- ISCC (Inno Setup)
call :find_iscc
if not defined ISCC goto :doc_iscc_missing
set "ROW_D=!ISCC!"
call :row "iscc (Inno Setup 6)" OK
goto :doc_ps
:doc_iscc_missing
set "ROW_D=ISCC.exe not found (optional, only for the installer)"
call :row "iscc (Inno Setup 6)" MISSING
call :hint "winget install -e --id JRSoftware.InnoSetup   (or choco install innosetup)"
:doc_ps
rem --- PowerShell
if not defined PSH goto :doc_ps_missing
set "ROW_D=!PSH!"
call :row "powershell" OK
goto :doc_comp
:doc_ps_missing
set "ROW_D=not found in PATH"
call :row "powershell" MISSING
call :hint "built into Windows; or winget install -e --id Microsoft.PowerShell"
set "DOC_BAD=1"
:doc_comp
rem --- rustfmt and clippy (used by Check)
where cargo >nul 2>&1
if errorlevel 1 goto :doc_end
cargo fmt --version >nul 2>&1
if errorlevel 1 goto :doc_fmt_missing
set "ROW_D=cargo fmt (for Check)"
call :row "rustfmt" OK
goto :doc_clippy
:doc_fmt_missing
set "ROW_D=component not installed (optional, for Check)"
call :row "rustfmt" MISSING
call :hint "rustup component add rustfmt"
:doc_clippy
cargo clippy --version >nul 2>&1
if errorlevel 1 goto :doc_clippy_missing
set "ROW_D=cargo clippy (for Check)"
call :row "clippy" OK
goto :doc_end
:doc_clippy_missing
set "ROW_D=component not installed (optional, for Check)"
call :row "clippy" MISSING
call :hint "rustup component add clippy"
:doc_end
echo(
if "!DOC_BAD!"=="1" goto :doc_bad
call :ok "All required tools were found."
exit /b 0
:doc_bad
call :err "Some required tools are missing (see above)."
exit /b 1
