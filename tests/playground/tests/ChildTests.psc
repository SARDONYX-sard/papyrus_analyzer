ScriptName ChildTests extends Child
{Tests of Child: extending it, they call its functions on Self. Run them with
`papyrus_analyzer test`, ▶ Run Test, or ⚙ Debug: with a breakpoint on a line
marked `watch:`, add the expressions of the comment to WATCH.}
Import PapyrusAnalyzer

; #[test]
Function RankAddsOne()
    AssertIntEq(Rank(), 2, "Base.Rank() + 1")
EndFunction

; #[test]
Function GreetCallsTheParent()
    Greet("Dovahkiin")
    AssertTrue(Rank() > 1)
EndFunction

; #[test]
Function FailsOnPurpose()
    AssertIntEq(Rank(), 3, "a failing test, to see the report: remove it")
EndFunction

; #[mock(Utility.RandomInt)] replaces the native in the tests of this script
Int Function FixedRoll(Int aiMin, Int aiMax)
    Return 4
EndFunction

; #[test]
Function RandomIsMocked()
    AssertIntEq(Utility.RandomInt(1, 6), 4, "FixedRoll")
EndFunction

Int updates

Event OnUpdate()
    updates += 1
    RegisterForSingleUpdate(1.0)
EndEvent

; #[test] Utility.Wait sends the OnUpdate events that are due
Function WaitSendsUpdates()
    RegisterForSingleUpdate(1.0)
    Utility.Wait(3.5)
    UnregisterForUpdate()
    Utility.Wait(5.0)
    AssertIntEq(updates, 3)
EndFunction

; #[test] NewObject gives the objects the game would; then, what was asked of it
Function ObjectsAndNativeCalls()
    Actor akActor = NewObject("Actor") as Actor
    AssertNotNone(akActor)
    akActor.Disable()
    AssertCalled("ObjectReference.Disable", 1)
    AssertCalledOn(akActor, "Disable")
    AssertCalledOn(Game.GetPlayer(), "Disable", 0)
    AssertNotCalled("Delete")
EndFunction

String received

Event OnTestEvent(String asName, String asArg, Float afNumber, Form akSender)
    received = asArg
EndEvent

; #[test] the callback of a custom event runs when it is sent
Function ModEventsCallBack()
    RegisterForModEvent("TestEvent", "OnTestEvent")
    SendModEvent("TestEvent", "hello")
    AssertStringEq(received, "hello")
EndFunction

; #[test]
Function SumsAnArray()
    Int[] values = New Int[4]
    Int i = 0
    While i < values.Length
        values[i] = (i + 1) * 10
        i += 1
    EndWhile
    Int total = 0
    i = 0
    While i < values.Length
        total += values[i] ; watch: total, i, values[i] * 2, values.Length, total >= 30 && i < 3
        i += 1
    EndWhile
    Count = total
    AssertIntEq(total, 100) ; watch: Count, Self.Count / 4, label + "!", values
EndFunction

; #[test] ▶ Run Test shows the Debug.Trace lines: objects as strings, like the game
Function ObjectsAsStrings()
    Form nothing
    Debug.Trace("Self: " + Self) ; watch: Self As String, "x" + Self
    Debug.Trace("None: " + nothing)
    AssertStringEq(nothing as String, "None")
EndFunction

; #[test]
Function ScalesAFloat()
    Float half = 0.5
    Float scaled = Scale(Count + 1, half)
    AssertFloatEq(scaled, 2.0)
EndFunction

Float Function Scale(Int aiValue, Float afBy)
    Return aiValue * afBy ; watch: aiValue * afBy, (aiValue As String) + "x", afBy As Int
EndFunction
