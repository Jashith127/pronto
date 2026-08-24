; Older Pronto builds could leave the bundled speech server alive after the
; parent app closed. Stop it before NSIS replaces runtime DLLs so upgrades do
; not produce one "file in use" dialog per CUDA dependency.
!macro StopProntoSpeechRuntime
  nsis_tauri_utils::KillProcessCurrentUser "nemo-speech.exe"
  Pop $R0
  Sleep 750
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro StopProntoSpeechRuntime
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro StopProntoSpeechRuntime
!macroend
