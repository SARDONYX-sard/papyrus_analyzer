ScriptName SampleBase extends ObjectReference Hidden
{Samples of the compiler: properties, variables, arithmetic, casts, strings,
control flow, arrays and states. The parent of Child, which calls it with `Parent.`.}

Import Utility

Int Property Count = 3 Auto
Float Property Speed = 1.5 AutoReadOnly Hidden
Actor Property PlayerRef Auto
Int counter = 10
String label = "basics"
Bool enabled = True Conditional

Event OnInit()
    Debug.Trace("Base: init")
EndEvent

Function Greet(String asName)
    Debug.Trace("Base: hello " + asName)
EndFunction

Int Function Rank()
    Return 1
EndFunction

Int Function Add(Int a, Int b = 2) Global
    Return a + b
EndFunction

Float Function Mix(Int i, Float f)
    Float r = i * f + i / 2 - i % 3
    r -= 1
    r *= 2.0
    Return -r
EndFunction

String Function Describe(Actor akActor, Int n)
    String s = "n=" + n + " ratio=" + (n as Float) / 3.0 + " ok=" + (n > 2)
    If akActor && n != 0 || !enabled
        s += " actor"
    ElseIf n >= 10
        s = s + " big"
    Else
        s = label
    EndIf
    Return s
EndFunction

Int Function Loop(Int limit)
    Int i = 0
    Int total = 0
    While i < limit && total <= 100
        total += Add(i, b = 3)
        i += 1
    EndWhile
    Return total
EndFunction

Function UseThings()
    Count = Count + 1
    counter -= Count
    Wait(0.5)
    Debug.Trace("count " + Count, 1)
    PlayerRef.Kill()
    Float mult = PlayerRef.GetActorValue("SpeedMult")
    Int[] values = New Int[5]
    values[0] = 4
    values[1] = values[0] * 2
    Int where = values.Find(8)
    Int size = values.Length
    Form f = PlayerRef as Form
    Actor a = f as Actor
    If a == None
        Return
    EndIf
    GoToState("Busy")
    String current = GetState()
    Debug.Trace(current + ": " + mult + " " + where + " " + size)
    RegisterForSingleUpdate(1.0)
EndFunction

State Busy
    Event OnUpdate()
        GoToState("")
    EndEvent

    Function UseThings()
        Debug.Notification("busy")
    EndFunction
EndState
