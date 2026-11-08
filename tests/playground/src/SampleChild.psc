ScriptName SampleChild extends SampleBase
{Overrides of Base, full properties and an auto state. Hover `Parent`: the
parent script, and the function it calls.}

Int _level = 1

Int Property Level
    Int Function Get()
        Return _level
    EndFunction
    Function Set(Int value)
        If value < 0
            value = 0
        EndIf
        _level = value
    EndFunction
EndProperty

Bool Property Ready Hidden
    Bool Function Get()
        Return Level > 2
    EndFunction
EndProperty

Event OnInit()
    Parent.OnInit()
    Debug.Trace("Child: init")
EndEvent

Function Greet(String asName)
    Parent.Greet(asName)
    Debug.Trace("Child: hello " + asName)
EndFunction

Int Function Rank()
    Return Parent.Rank() + 1
EndFunction

Function UseThings()
    Parent.UseThings()
    SampleBase other = Self as SampleBase
    other.Count = 7
    Int n = other.Count
    Bool r = Ready
    Self.Level = n
    Debug.Trace(Mix(n, 2.0) as String + " ready=" + r)
EndFunction

Auto State Idle
    Event OnBeginState()
        Level = Level + 1
    EndEvent
EndState
