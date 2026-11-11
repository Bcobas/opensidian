; SPDX-License-Identifier: GPL-3.0-or-later

!define OPENSIDIAN_MARKDOWN_PROGID "dev.koto.opensidian.Markdown"

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr SHCTX "Software\Classes\${OPENSIDIAN_MARKDOWN_PROGID}" "" "Opensidian Markdown"
  WriteRegStr SHCTX "Software\Classes\${OPENSIDIAN_MARKDOWN_PROGID}" "AllowSilentDefaultTakeOver" ""
  WriteRegStr SHCTX "Software\Classes\${OPENSIDIAN_MARKDOWN_PROGID}\DefaultIcon" "" '$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0'
  WriteRegStr SHCTX "Software\Classes\${OPENSIDIAN_MARKDOWN_PROGID}\shell\open\command" "" '$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\"'
  WriteRegStr SHCTX "Software\Classes\.md\OpenWithProgids" "${OPENSIDIAN_MARKDOWN_PROGID}" ""
  WriteRegStr SHCTX "Software\Classes\.markdown\OpenWithProgids" "${OPENSIDIAN_MARKDOWN_PROGID}" ""
  !insertmacro UPDATEFILEASSOC
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegValue SHCTX "Software\Classes\.md\OpenWithProgids" "${OPENSIDIAN_MARKDOWN_PROGID}"
  DeleteRegValue SHCTX "Software\Classes\.markdown\OpenWithProgids" "${OPENSIDIAN_MARKDOWN_PROGID}"
  DeleteRegKey SHCTX "Software\Classes\${OPENSIDIAN_MARKDOWN_PROGID}"
  !insertmacro UPDATEFILEASSOC
!macroend
