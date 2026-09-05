; Pronto installer hooks: speech-runtime guard + speech-model provisioning.
;
; Source of truth for model identity: src-tauri/model.sha256.
; Keep the PRONTO_MODEL_* defines below in sync with that file.
;
; The model (~681 MB) is NOT bundled. The installer downloads it to
; $INSTDIR\models at install time, unless a file with a matching SHA256
; is already there (updates/reinstalls skip the download entirely).
; Download + hashing run through stock Windows PowerShell (BITS with an
; Invoke-WebRequest fallback), so no third-party NSIS plugins are needed.

!define PRONTO_MODEL_FILE "parakeet-tdt-0.6b-v3.q8_0.gguf"
!define PRONTO_MODEL_URL "https://github.com/Jashith127/pronto/releases/download/parakeet-model-v1/parakeet-tdt-0.6b-v3.q8_0.gguf"
!define PRONTO_MODEL_SHA256 "e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e"
!define PRONTO_MODEL_KB "697242"
!define PRONTO_FETCH_SCRIPT "$TEMP\pronto-model-fetch.ps1"
!define PRONTO_HASH_RESULT "$TEMP\pronto-model-hash.txt"
!define PRONTO_DONE_RESULT "$TEMP\pronto-model-done.txt"

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
  ; On updates the new installer reuses the existing model (hash-checked),
  ; so only remove it on a real uninstall.
  ${If} $UpdateMode <> 1
    Delete "$INSTDIR\models\${PRONTO_MODEL_FILE}"
    RMDir "$INSTDIR\models"
  ${EndIf}
!macroend

; Writes the PowerShell helper used for hashing and downloading.
; Clobbers $9; all other registers preserved.
Function ProntoWriteFetchScript
  FileOpen $9 "${PRONTO_FETCH_SCRIPT}" w
  FileWrite $9 "param($$Mode,$$Path,$$Out,$$Url)$\r$\n"
  FileWrite $9 "$$ErrorActionPreference='Stop'$\r$\n"
  FileWrite $9 "try {$\r$\n"
  FileWrite $9 "  if ($$Mode -eq 'hash') {$\r$\n"
  FileWrite $9 "    (Get-FileHash -Algorithm SHA256 -LiteralPath $$Path).Hash.ToLower() | Out-File -LiteralPath $$Out -NoNewline -Encoding ascii$\r$\n"
  FileWrite $9 "  } else {$\r$\n"
  FileWrite $9 "    try { Start-BitsTransfer -Source $$Url -Destination $$Path -ErrorAction Stop }$\r$\n"
  FileWrite $9 "    catch { Invoke-WebRequest -Uri $$Url -OutFile $$Path }$\r$\n"
  FileWrite $9 "    'OK' | Out-File -LiteralPath $$Out -NoNewline -Encoding ascii$\r$\n"
  FileWrite $9 "  }$\r$\n"
  FileWrite $9 "} catch { exit 1 }$\r$\n"
  FileClose $9
FunctionEnd

; Checks whether $R9 (full model path) exists and matches the expected hash.
; Pushes 1 on match, 0 otherwise. Preserves $R9; clobbers $R8/$R7/$0.
Function ProntoModelHashMatches
  Push $R8
  Push $R7
  StrCpy $R7 0
  IfFileExists "$R9" pronto_hash_run pronto_hash_done
  pronto_hash_run:
  Delete "${PRONTO_HASH_RESULT}"
  nsExec::Exec 'powershell -NoProfile -ExecutionPolicy Bypass -File $\"${PRONTO_FETCH_SCRIPT}$\" hash $\"$R9$\" $\"${PRONTO_HASH_RESULT}$\"'
  Pop $0
  IfFileExists "${PRONTO_HASH_RESULT}" pronto_hash_read pronto_hash_done
  pronto_hash_read:
  FileOpen $R8 "${PRONTO_HASH_RESULT}" r
  FileRead $R8 $R7
  FileClose $R8
  Delete "${PRONTO_HASH_RESULT}"
  StrCmp $R7 "${PRONTO_MODEL_SHA256}" pronto_hash_match pronto_hash_done
  pronto_hash_match:
  StrCpy $R7 1
  pronto_hash_done:
  Exch $R7
  Exch
  Pop $R8
FunctionEnd

!macro ProntoEnsureModel
  CreateDirectory "$INSTDIR\models"
  Call ProntoWriteFetchScript
  StrCpy $R9 "$INSTDIR\models\${PRONTO_MODEL_FILE}"
  Call ProntoModelHashMatches
  Pop $0
  StrCmp $0 1 pronto_model_present pronto_model_download
  pronto_model_download:
  DetailPrint "Speech model not found locally. Downloading ${PRONTO_MODEL_FILE} (681 MB). This can take several minutes..."
  pronto_model_retry:
  Delete "${PRONTO_DONE_RESULT}"
  nsExec::Exec 'powershell -NoProfile -ExecutionPolicy Bypass -File $\"${PRONTO_FETCH_SCRIPT}$\" download $\"$R9$\" $\"${PRONTO_DONE_RESULT}$\" $\"${PRONTO_MODEL_URL}$\"'
  Pop $0
  IfFileExists "${PRONTO_DONE_RESULT}" pronto_model_verify pronto_model_dl_failed
  pronto_model_dl_failed:
  Delete "$R9"
  Delete "${PRONTO_DONE_RESULT}"
  DetailPrint "Speech model download failed."
  IfSilent pronto_model_abort_dl pronto_model_ask_dl
  pronto_model_ask_dl:
  MessageBox MB_ICONEXCLAMATION|MB_RETRYCANCEL "Pronto could not download the speech model. Check your connection and try again." IDRETRY pronto_model_retry
  Delete "$R9"
  Abort "Pronto could not download the speech model. Check your connection and run the installer again."
  pronto_model_abort_dl:
  Abort "Pronto could not download the speech model. Check your connection and run the installer again."
  pronto_model_verify:
  Delete "${PRONTO_DONE_RESULT}"
  Call ProntoModelHashMatches
  Pop $0
  StrCmp $0 1 pronto_model_ok pronto_model_bad_hash
  pronto_model_bad_hash:
  Delete "$R9"
  DetailPrint "Downloaded speech model failed verification."
  IfSilent pronto_model_abort_hash pronto_model_ask_hash
  pronto_model_ask_hash:
  MessageBox MB_ICONEXCLAMATION|MB_RETRYCANCEL "The downloaded file failed verification. Download it again?" IDRETRY pronto_model_retry
  Abort "The downloaded speech model failed verification. Run the installer again."
  pronto_model_abort_hash:
  Abort "The downloaded speech model failed verification. Run the installer again."
  pronto_model_ok:
  DetailPrint "Speech model ready."
  ; The template's EstimatedSize was computed without the model; add it back
  ; so Add/Remove Programs reports a truthful size. Best effort only.
  ClearErrors
  ReadRegDWORD $0 SHCTX "${UNINSTKEY}" "EstimatedSize"
  IfErrors pronto_model_done pronto_model_size
  pronto_model_size:
  IntOp $0 $0 + ${PRONTO_MODEL_KB}
  WriteRegDWORD SHCTX "${UNINSTKEY}" "EstimatedSize" $0
  Goto pronto_model_done
  pronto_model_present:
  DetailPrint "Speech model already present. Skipping download."
  pronto_model_done:
  Delete "${PRONTO_FETCH_SCRIPT}"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro ProntoEnsureModel
!macroend
