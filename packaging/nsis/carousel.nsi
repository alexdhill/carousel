; Carousel installer. Expects Carousel.exe and Carousel.ico beside this script.
; Build: makensis /DVERSION=<x.y.z> carousel.nsi

!ifndef VERSION
  !define VERSION "0.0.0"
!endif

Name "Carousel"
OutFile "Carousel-Setup-x86_64.exe"
Unicode true
InstallDir "$PROGRAMFILES64\Carousel"
InstallDirRegKey HKLM "Software\Carousel" "InstallDir"
RequestExecutionLevel admin

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section "Install"
  SetOutPath "$INSTDIR"
  File "Carousel.exe"
  File "Carousel.ico"

  ; Start-Menu shortcut
  CreateDirectory "$SMPROGRAMS\Carousel"
  CreateShortcut "$SMPROGRAMS\Carousel\Carousel.lnk" "$INSTDIR\Carousel.exe" "" "$INSTDIR\Carousel.ico"

  ; .deck association
  WriteRegStr HKLM "Software\Classes\.deck" "" "Carousel.Deck"
  WriteRegStr HKLM "Software\Classes\Carousel.Deck" "" "Slide Deck"
  WriteRegStr HKLM "Software\Classes\Carousel.Deck\DefaultIcon" "" "$INSTDIR\Carousel.ico"
  WriteRegStr HKLM "Software\Classes\Carousel.Deck\shell\open\command" "" '"$INSTDIR\Carousel.exe" "%1"'

  ; Uninstall registration
  WriteRegStr HKLM "Software\Carousel" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Carousel" "DisplayName" "Carousel"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Carousel" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Carousel" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\Carousel.exe"
  Delete "$INSTDIR\Carousel.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Carousel\Carousel.lnk"
  RMDir "$SMPROGRAMS\Carousel"
  DeleteRegKey HKLM "Software\Classes\.deck"
  DeleteRegKey HKLM "Software\Classes\Carousel.Deck"
  DeleteRegKey HKLM "Software\Carousel"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Carousel"
SectionEnd
