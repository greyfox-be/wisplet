#Requires AutoHotkey v2.0
#SingleInstance Force
Persistent

; ===== Cacher la taskbar Windows en permanence =====
; yasb remplit le rôle de status bar visuellement. La taskbar Windows reste
; fonctionnelle (tray icons, notifications, ALT+TAB) mais n'apparaît plus,
; ni au hover bord d'écran ni sur tap LWin.

SetupTaskbarHiding()

SetupTaskbarHiding() {
    ; Attendre que Shell_TrayWnd existe (jusqu'à 30s — boot lent possible).
    trayHwnd := WinWait("ahk_class Shell_TrayWnd", , 30)
    if (!trayHwnd)
        return  ; vraiment introuvable → on abandonne

    ; WinHide peut être annulé par explorer.exe pendant qu'il finit son init :
    ; on retry quelques fois sur les premières secondes après détection.
    HidePrimaryTray()
    SetTimer HidePrimaryTray, -500
    SetTimer HideSecondaryTrays, -500

    ; Surveillance continue : Windows réinitialise parfois Shell_TrayWnd hors
    ; init (mise à jour notif, restart partiel explorer.exe, DPI change). Les
    ; SetTimer ci-dessus sont one-shot → sans ce watcher la taskbar revient.
    SetTimer WatchTaskbar, 3000
}

WatchTaskbar() {
    ; WinExist sans DetectHiddenWindows → renvoie hwnd seulement si VISIBLE.
    if WinExist("ahk_class Shell_TrayWnd")
        try WinHide "ahk_class Shell_TrayWnd"
    for h in WinGetList("ahk_class Shell_SecondaryTrayWnd")
        try WinHide "ahk_id " h
}

HidePrimaryTray() {
    static attempts := 0
    attempts++
    if WinExist("ahk_class Shell_TrayWnd") {
        ; DetectHiddenWindows OFF par défaut → si WinExist trouve, c'est visible
        try WinHide "ahk_class Shell_TrayWnd"
    }
    if (attempts < 10)
        SetTimer HidePrimaryTray, -1000
}

HideSecondaryTrays() {
    static attempts := 0
    attempts++
    if WinExist("ahk_class Shell_SecondaryTrayWnd")
        try WinHide "ahk_class Shell_SecondaryTrayWnd"
    if (attempts < 10)
        SetTimer HideSecondaryTrays, -1000
}

; ===== Tap LWin → toggle Wisplet =====
; Envoie F13 (touche absente du clavier physique → aucun conflit possible).
; LWin maintenu + autre touche (Win+L, Win+R, Win+X, ...) → passe normalement.
;
; Le shell n'ouvre le menu Démarrer que si AUCUNE autre touche n'a été vue
; entre le LWin down et le LWin up. On injecte donc une touche bidon pendant
; que Win est enfoncé : le tap ressemble alors à un combo côté shell.
;
; Le down est BLOQUÉ ($, pas de ~) puis rejoué par nous, phantom collé
; derrière dans le MÊME Send → un seul SendInput, atomique. C'est ça qui rend
; l'ordre vu par le shell déterministe : LWin down → F14 → ... → LWin up.
; Laisser le down passer en natif (~, ancien code) perdait la course — le up
; natif partait avant notre phantom et le menu s'ouvrait. Mettre le phantom
; au up seul ne suffisait pas non plus, pour la même raison.
;
; F14 et pas {vk07} : vk07 est un VK « undefined » que le shell Win11 ne
; compte pas comme une touche. F14 est une touche réelle, absente des
; claviers physiques, et Win+F14 n'a aucun binding OS.

global lwinRelayed := false

$LWin:: {
    global lwinRelayed
    if (lwinRelayed)  ; auto-repeat (LWin maintenu) → ne pas re-spammer le phantom
        return
    lwinRelayed := true
    Send "{Blind}{LWin down}{F14}"
}

$LWin up:: {
    global lwinRelayed
    ; A_PriorKey doit être lu AVANT nos propres Send, sinon on relit le phantom.
    tap := (A_PriorKey = "LWin" || A_PriorKey = "F14")
    lwinRelayed := false
    Send "{Blind}{LWin up}"
    if (tap)
        Send "{F13}"
}
