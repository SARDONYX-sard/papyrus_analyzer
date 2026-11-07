; #![allow(all)] silences every lint of this file: comment it out to see them
ScriptName Lints extends ObjectReference
{One sample of each lint (`papyrus_analyzer lint --list`), then the attributes
that allow them. Hover the attributes, complete them after `(` or `,`.
The pedantic lints (unchecked_*, get_player_in_loop) are allowed by default: enable
them in [lints] of papyrus.toml. The lints about the game (unknown-event,
repeating-update, get-player-in-loop) need its scripts: the imports of papyrus.toml.}

; unused-import (suspicious): an Import whose global functions are never called:
; every call here is written `Debug.Trace`. Quick fix: remove it.
Import Debug

; unused-script-variable (suspicious): a script variable whose value is never read.
Int NeverRead

Function UnusedScriptVariable()
    NeverRead = 1
EndFunction

; index-out-of-bounds (correctness, deny): a constant index outside of an
; array of known length.
Function IndexOutOfBounds()
    Int[] values = New Int[4]
    values[3] = 1  ; fine: 0-3
    values[4] = 1  ; error: `values` has 4 elements (0-3)
    Int last = values[-1]  ; error
    Debug.Trace(last)
EndFunction

; unknown-state (suspicious): GoToState with a state declared nowhere. Quick fix:
; the closest name. Inside the quotes: completion of the states, go to definition.
Function UnknownState()
    GoToState("Waitng")  ; warning: did you mean `Waiting`?
    GoToState("Waiting") ; fine; go to definition
    GoToState("")        ; fine: the empty state
    GoToState("Ready")   ; fine: a flag, compared with GetState() below
    If GetState() == "Ready"
        Debug.Trace("ready")
    EndIf
    If GetState() == "Wating" ; warning too; `"Waiting"` below: hover, references, rename
        Debug.Trace("waiting")
    ElseIf GetState() == "Waiting"
        Debug.Trace("waiting")
    EndIf
EndFunction

State Waiting
EndState

; unknown-event (suspicious): an event spelled almost like one of the parents, and
; that nothing refers to: the game never sends it. Quick fix: the closest name.
Event OnActivte(ObjectReference akActionRef) ; warning: did you mean `OnActivate`?
    Debug.Trace("never")
EndEvent

; A new event, not close to any: fine.
Event OnMyModEvent(String asName, String asArg, Float afArg, Form akSender)
    Debug.Trace(asName)
EndEvent

; unknown-mod-callback, unknown-mod-event (suspicious, SKSE): the strings of custom
; events. Inside the quotes: completion. Hover the names: who registers and sends
; them; go to definition goes from one to the other. The callback: hover, go to
; definition, references, rename, like the name of `OnMyModEvent` above.
Function ModEvents()
    RegisterForModEvent("MyModEvent", "OnMyModEvent") ; fine
    SendModEvent("MyModEvent")                        ; fine
    RegisterForModEvent("MyOther", "OnMyModEvnt")     ; warning: did you mean `OnMyModEvent`?
    RegisterForModEvent("Finishd", "OnMyModEvent")    ; warning: did you mean `Finished`?
    SendModEvent("Finished")                          ; warning: did you mean `Finishd`?
EndFunction

; unused-local (suspicious): a local variable whose value is never read.
; Quick fix: remove it. Denied by `unused-local = "deny"` in this papyrus.toml.
Function UnusedLocal()
    Int unused = 1
EndFunction

; unreachable-code (suspicious): statements after a Return.
Int Function UnreachableCode()
    Return 1
    Debug.Trace("never")
EndFunction

; repeating-update (suspicious): updates come whether the last one is done or not.
Function RepeatingUpdate()
    RegisterForUpdate(1.0)       ; warning
    RegisterForSingleUpdate(1.0) ; fine: register again at the end of OnUpdate
EndFunction

; bool-literal-comparison (complexity): a comparison with True or False.
; Quick fix: `If abFlag`. Allowed by `complexity = "warn"` in this papyrus.toml.
Function BoolLiteralComparison(Bool abFlag)
    If abFlag == True
        Debug.Trace("flag")
    EndIf
EndFunction

; self-assignment (complexity): a variable assigned to itself. Quick fix: remove it.
Function SelfAssignment(Int aiCount)
    aiCount = aiCount
EndFunction

; redundant-cast (complexity): a cast to the type the value already has.
; Quick fix: remove it.
Function RedundantCast(Actor akActor)
    Actor same = akActor as Actor   ; warning
    (same as Actor).Kill()          ; warning
    ObjectReference up = akActor as ObjectReference ; fine: another type
    Debug.Trace(up)
EndFunction

; unchecked-cast (pedantic): the result of a cast used without checking None.
; Quick fix: wrap it in `If`.
Function UncheckedCast(ObjectReference akRef)
    Actor akActor = akRef as Actor
    akActor.Kill() ; hover `akActor`: may be None here
    If akActor
        akActor.Kill() ; hover `akActor`: not None here
    EndIf
EndFunction

; unchecked-call-result (pedantic): an object returned by a call used without
; checking None.
Function UncheckedCallResult()
    GetLinkedRef().Disable()
EndFunction

; get-player-in-loop (pedantic): a variable set before the loop needs no call.
Function GetPlayerInLoop()
    Int i = 0
    While i < 3
        Game.GetPlayer().Kill()
        i += 1
    EndWhile
EndFunction

; #[allow(unused_local)] outer attribute: the whole function below
Function AllowedInFunction()
    Int notRead = 1
EndFunction

Function AllowedPerStatement()
    ; #[allow(unused_local, suspicious)] the next statement
    Int b = 2
    Int c = 3 ; reported: `c` is never read
EndFunction

Function SkippedByTheFormatter()
    ; #[fmt::skip] aligned by hand, kept as written
    Int   width    =   10
    Debug.Trace("width = " + width)
EndFunction

; Mistakes in attributes, which `allow` cannot silence: remove the first `;`
; of these lines to see the warnings.
; ; #[allow(invalid_cast)] a compile error: cannot be allowed
; ; #[allow(unused_lokal)] unknown lint
; ; #[alow] unknown attribute
; ; #![allow(all)] misplaced: `#![...]` goes before the code (an error)
Function Mistakes()
    Int a = 1 ; ; #[allow(unused_local)] misplaced: after code, applies to nothing
    Debug.Trace(a)
EndFunction
