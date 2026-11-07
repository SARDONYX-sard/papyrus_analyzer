ScriptName PapyrusAnalyzer Hidden
{Built into papyrus_analyzer, for the scripts of the [test] sources of papyrus.toml:
what `papyrus_analyzer test` gives tests. A failed assertion fails the test.

# Examples

```papyrus
ScriptName MyQuestTests extends MyQuest
Import PapyrusAnalyzer

; #[test]
Function CountsKills()
    RecordKill()
    AssertIntEq(Kills, 1, "one kill")
EndFunction
```}

Function AssertIntEq(Int aiActual, Int aiExpected, String asMessage = "") Global Native
{Fails unless aiActual == aiExpected; asMessage is shown with the failure.

# Examples

```papyrus
AssertIntEq(2 + 2, 4)
AssertIntEq(Kills, 1, "one kill")
```}

Function AssertFloatEq(Float afActual, Float afExpected, Float afEpsilon = 0.0001, String asMessage = "") Global Native
{Fails unless afActual is within afEpsilon of afExpected.

# Examples

```papyrus
AssertFloatEq(1.0 / 3.0, 0.3333)
AssertFloatEq(Speed(), 2.5, 0.1)
```}

Function AssertStringEq(String asActual, String asExpected, String asMessage = "") Global Native
{Fails unless asActual == asExpected, ignoring case like Papyrus.

# Examples

```papyrus
AssertStringEq("Hello", "hello")
AssertStringEq(GetState(), "Busy")
```}

Function AssertBoolEq(Bool abActual, Bool abExpected, String asMessage = "") Global Native
{Fails unless abActual == abExpected.

# Examples

```papyrus
AssertBoolEq(IsReady(), False)
```}

Function AssertTrue(Bool abValue, String asMessage = "") Global Native
{Fails unless abValue is True.

# Examples

```papyrus
AssertTrue(Kills > 0, "at least one kill")
```}

Function AssertFalse(Bool abValue, String asMessage = "") Global Native
{Fails unless abValue is False.

# Examples

```papyrus
AssertFalse(IsDisabled())
```}

Function AssertNone(Form akForm, String asMessage = "") Global Native
{Fails unless akForm is None.

# Examples

```papyrus
AssertNone(akTarget as Actor)
```}

Function AssertNotNone(Form akForm, String asMessage = "") Global Native
{Fails when akForm is None.

# Examples

```papyrus
AssertNotNone(Game.GetPlayer())
```}

Function Fail(String asMessage) Global Native
{Fails the test.

# Examples

```papyrus
If Kills < 0
    Fail("negative kills")
EndIf
```}

Form Function NewObject(String asScript) Global Native
{A new object of asScript, after its OnInit. Properties and mocks are None in
tests: this gives them objects. None and a failure when no script has that name.

# Examples

```papyrus
Actor akVictim = NewObject("Actor") as Actor
Target = akVictim
KillTarget()
AssertCalledOn(akVictim, "Kill")
```}

Alias Function NewAlias(String asScript) Global Native
{Like NewObject, for the scripts extending Alias.

# Examples

```papyrus
ReferenceAlias akAlias = NewAlias("ReferenceAlias") as ReferenceAlias
```}

ActiveMagicEffect Function NewEffect(String asScript) Global Native
{Like NewObject, for the scripts extending ActiveMagicEffect.

# Examples

```papyrus
MyEffect akEffect = NewEffect("MyEffect") as MyEffect
```}

Int Function CallCount(String asNative) Global Native
{How many times this test called a native: `Disable`, or with its script,
`ObjectReference.Disable`. A script extending it counts too: `Actor.Disable`.

# Examples

```papyrus
Game.GetPlayer().Disable()
AssertIntEq(CallCount("Actor.Disable"), 1)
```}

Function AssertCalled(String asNative, Int aiTimes = -1, String asMessage = "") Global Native
{Fails unless the native was called aiTimes times; -1: at least once. See CallCount.

# Examples

```papyrus
CloseTheDoor()
AssertCalled("ObjectReference.Lock", 1)
AssertCalled("PlaySound")
```}

Function AssertNotCalled(String asNative, String asMessage = "") Global Native
{Fails when the native was called. See CallCount.

# Examples

```papyrus
AssertNotCalled("Delete", "the reference stays")
```}

Function AssertCalledOn(Form akObject, String asNative, Int aiTimes = -1, String asMessage = "") Global Native
{Like AssertCalled, for the calls on akObject only.

# Examples

```papyrus
Actor akActor = NewObject("Actor") as Actor
akActor.Disable()
AssertCalledOn(akActor, "Disable", 1)
AssertCalledOn(Game.GetPlayer(), "Disable", 0)
```}
