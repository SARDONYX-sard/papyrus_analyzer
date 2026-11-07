; # Examples
;
; ```papyrus,ignore
; ScriptName MyQuestTests extends MyQuest
; Import PapyrusAnalyzer
;
; ; #[test]
; Function CountsKills()
;     RecordKill()
;     AssertIntEq(Kills, 1, "one kill")
; EndFunction
; ```
ScriptName PapyrusAnalyzer Hidden
{Built into papyrus_analyzer, for the scripts of the [test] sources of papyrus.toml:
what `papyrus_analyzer test` gives tests. A failed assertion fails the test.}

; # Examples
;
; ```papyrus
; # Int Kills = 1
; AssertIntEq(2 + 2, 4)
; AssertIntEq(Kills, 1, "one kill")
; ```
Function AssertIntEq(Int aiActual, Int aiExpected, String asMessage = "") Global Native
{Fails unless aiActual == aiExpected; asMessage is shown with the failure.}

; # Examples
;
; ```papyrus
; AssertFloatEq(1.0 / 3.0, 0.3333)
; Float speed = 2.55
; AssertFloatEq(speed, 2.5, 0.1)
; ```
Function AssertFloatEq(Float afActual, Float afExpected, Float afEpsilon = 0.0001, String asMessage = "") Global Native
{Fails unless afActual is within afEpsilon of afExpected.}

; # Examples
;
; ```papyrus
; AssertStringEq("Hello", "hello")
; GoToState("Busy")
; AssertStringEq(GetState(), "busy")
; ```
Function AssertStringEq(String asActual, String asExpected, String asMessage = "") Global Native
{Fails unless asActual == asExpected, ignoring case like Papyrus.}

; # Examples
;
; ```papyrus
; Actor akActor = NewObject("Actor") as Actor
; AssertBoolEq(akActor.IsDisabled(), False)
; ```
Function AssertBoolEq(Bool abActual, Bool abExpected, String asMessage = "") Global Native
{Fails unless abActual == abExpected.}

; # Examples
;
; ```papyrus
; # Int Kills = 1
; AssertTrue(Kills > 0, "at least one kill")
; ```
Function AssertTrue(Bool abValue, String asMessage = "") Global Native
{Fails unless abValue is True.}

; # Examples
;
; ```papyrus
; Actor akActor = NewObject("Actor") as Actor
; AssertFalse(akActor.IsDead())
; ```
Function AssertFalse(Bool abValue, String asMessage = "") Global Native
{Fails unless abValue is False.}

; # Examples
;
; ```papyrus
; # Form akTarget = None
; AssertNone(akTarget as Actor)
; ```
Function AssertNone(Form akForm, String asMessage = "") Global Native
{Fails unless akForm is None.}

; # Examples
;
; ```papyrus
; AssertNotNone(Game.GetPlayer())
; ```
Function AssertNotNone(Form akForm, String asMessage = "") Global Native
{Fails when akForm is None.}

; # Examples
;
; ```papyrus
; # Int Kills = 1
; If Kills < 0
;     Fail("negative kills")
; EndIf
; ```
Function Fail(String asMessage) Global Native
{Fails the test.}

; # Examples
;
; ```papyrus
; Actor akVictim = NewObject("Actor") as Actor
; akVictim.Kill()
; AssertCalledOn(akVictim, "Kill")
; ```
Form Function NewObject(String asScript) Global Native
{A new object of asScript, after its OnInit. Properties and mocks are None in
tests: this gives them objects. None and a failure when no script has that name.}

; # Examples
;
; ```papyrus
; ReferenceAlias akAlias = NewAlias("ReferenceAlias") as ReferenceAlias
; ```
Alias Function NewAlias(String asScript) Global Native
{Like NewObject, for the scripts extending Alias.}

; # Examples
;
; ```papyrus
; ActiveMagicEffect akEffect = NewEffect("ActiveMagicEffect")
; AssertTrue(akEffect != None)
; ```
ActiveMagicEffect Function NewEffect(String asScript) Global Native
{Like NewObject, for the scripts extending ActiveMagicEffect.}

; # Examples
;
; ```papyrus
; Game.GetPlayer().Disable()
; AssertIntEq(CallCount("Actor.Disable"), 1)
; ```
Int Function CallCount(String asNative) Global Native
{How many times this test called a native: `Disable`, or with its script,
`ObjectReference.Disable`. A script extending it counts too: `Actor.Disable`.}

; # Examples
;
; ```papyrus
; ObjectReference akDoor = NewObject("ObjectReference") as ObjectReference
; akDoor.Lock()
; AssertCalled("ObjectReference.Lock", 1)
; AssertCalled("Lock")
; ```
Function AssertCalled(String asNative, Int aiTimes = -1, String asMessage = "") Global Native
{Fails unless the native was called aiTimes times; -1: at least once. See CallCount.}

; # Examples
;
; ```papyrus
; AssertNotCalled("Delete", "the reference stays")
; ```
Function AssertNotCalled(String asNative, String asMessage = "") Global Native
{Fails when the native was called. See CallCount.}

; # Examples
;
; ```papyrus
; Actor akActor = NewObject("Actor") as Actor
; akActor.Disable()
; AssertCalledOn(akActor, "Disable", 1)
; AssertCalledOn(Game.GetPlayer(), "Disable", 0)
; ```
Function AssertCalledOn(Form akObject, String asNative, Int aiTimes = -1, String asMessage = "") Global Native
{Like AssertCalled, for the calls on akObject only.}
