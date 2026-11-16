use std::{path::PathBuf, sync::Arc};

use expect_test::{Expect, expect};
use papyrus_ide::Workspace;
use papyrus_pex::{PexFile, disasm::DisasmOptions};

use crate::{CompileOptions, compile};

fn check(src: &str, options: CompileOptions, expect: Expect) {
    let mut ws = Workspace::new();
    let file = ws.set_open(PathBuf::from("/ws/T.psc"), Arc::from(src), 0);
    let pex = match compile(&ws, file, &options) {
        Ok(pex) => pex,
        Err(e) => panic!("{e:?}"),
    };
    // Through the bytes, to check that the file is well formed.
    let bytes = pex.to_bytes().unwrap();
    let pex = PexFile::from_bytes(&bytes).unwrap();
    let text = papyrus_pex::disasm::disassemble(
        &pex,
        DisasmOptions { skip_header: true, skip_lines: false },
    );
    // Only the code: the tables are covered by the comparisons with the
    // official compiler.
    let code: String = text
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with('.') || t.starts_with(".function") || t.starts_with(".local")
        })
        .map(|l| format!("{}\n", l.trim_start()))
        .collect();
    expect.assert_eq(&code);
}

#[test]
fn expressions_and_control_flow() {
    check(
        r#"ScriptName T
Int counter = 1
Int Property Count Auto
Float Function Mix(Int i, Float f)
    Float r = i * f
    r -= 1
    Return -r
EndFunction
Function Run(Int n)
    While n > 0 && counter != 3
        If n % 2 == 0
            Count += 1
        ElseIf n < 10
            counter = Add(n)
        Else
            Return
        EndIf
        n -= 1
    EndWhile
EndFunction
Int Function Add(Int a, Int b = 2) Global
    Return a + b
EndFunction
"#,
        CompileOptions::default(),
        expect![[r#"
            .function Mix
            .localTable
            .local ::temp0 Float
            .local r Float
            CAST ::temp0 i ;@line 5
            FMUL ::temp0 ::temp0 f ;@line 5
            ASSIGN r ::temp0 ;@line 5
            CAST ::temp0 1 ;@line 6
            FSUB ::temp0 r ::temp0 ;@line 6
            ASSIGN r ::temp0 ;@line 6
            FNEG ::temp0 r ;@line 7
            RETURN ::temp0 ;@line 7
            .function Run
            .localTable
            .local ::temp1 Bool
            .local ::temp2 Bool
            .local ::temp3 Int
            .local ::temp4 Bool
            label0:
            COMPAREGT ::temp1 n 0 ;@line 10
            CAST ::temp1 ::temp1 ;@line 10
            JUMPF ::temp1 label1 ;@line 10
            COMPAREEQ ::temp2 counter 3 ;@line 10
            NOT ::temp2 ::temp2 ;@line 10
            CAST ::temp1 ::temp2 ;@line 10
            label1:
            JUMPF ::temp1 label5 ;@line 10
            IMOD ::temp3 n 2 ;@line 11
            COMPAREEQ ::temp2 ::temp3 0 ;@line 11
            JUMPF ::temp2 label2 ;@line 11
            IADD ::temp3 ::Count_var 1 ;@line 12
            ASSIGN ::Count_var ::temp3 ;@line 12
            JUMP label4 ;@line 12
            label2:
            COMPARELT ::temp4 n 10 ;@line 13
            JUMPF ::temp4 label3 ;@line 13
            CALLSTATIC T Add ::temp3 2 n 2 ;@line 14
            ASSIGN counter ::temp3 ;@line 14
            JUMP label4 ;@line 14
            label3:
            RETURN none ;@line 16
            label4:
            ISUB ::temp3 n 1 ;@line 18
            ASSIGN n ::temp3 ;@line 18
            JUMP label0 ;@line 18
            label5:
            .function Add static
            .localTable
            .local ::temp5 Int
            IADD ::temp5 a b ;@line 22
            RETURN ::temp5 ;@line 22
            .function GotoState
            .localTable
            .local ::NoneVar None
            CALLMETHOD onEndState self ::NoneVar 0
            ASSIGN ::State newState
            CALLMETHOD onBeginState self ::NoneVar 0
            .function GetState
            .localTable
            RETURN ::State
        "#]],
    );
}

#[test]
fn optimizations() {
    check(
        r#"ScriptName T
Float Function F(Float x)
    If x > 1
        Return x - 1
    EndIf
    Return x
EndFunction
"#,
        CompileOptions { optimize: true, ..CompileOptions::default() },
        expect![[r#"
            .function F
            .localTable
            .local ::temp0 Float
            .local ::temp1 Bool
            COMPAREGT ::temp1 x 1.0 ;@line 3
            JUMPF ::temp1 label0 ;@line 3
            FSUB ::temp0 x 1.0 ;@line 4
            RETURN ::temp0 ;@line 4
            label0:
            RETURN x ;@line 6
            .function GotoState
            .localTable
            .local ::NoneVar None
            CALLMETHOD onEndState self ::NoneVar 0
            ASSIGN ::State newState
            CALLMETHOD onBeginState self ::NoneVar 0
            .function GetState
            .localTable
            RETURN ::State
        "#]],
    );
}

#[test]
fn header_follows_the_options() {
    let mut ws = Workspace::new();
    let file = ws.set_open(PathBuf::from("/ws/T.psc"), Arc::from("ScriptName T\n"), 0);
    let header = crate::PexHeader {
        user: Some(String::new()),
        computer: Some("CI".to_owned()),
        time: Some(1_706_702_400),
        source_time: Some(42),
    };
    let options = CompileOptions { header, ..CompileOptions::default() };
    let bytes = compile(&ws, file, &options).unwrap().to_bytes().unwrap();
    let pex = PexFile::from_bytes(&bytes).unwrap();
    assert_eq!(pex.user_name.decode(), "");
    assert_eq!(pex.machine_name.decode(), "CI");
    assert_eq!(pex.compilation_time, 1_706_702_400);
    assert_eq!(pex.debug_info.unwrap().modification_time, 42);
    // The same options, the same bytes.
    assert_eq!(compile(&ws, file, &options).unwrap().to_bytes().unwrap(), bytes);
}
