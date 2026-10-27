use std::{env, path::PathBuf, process::Command};
fn main() {
    for path in [
        "assets/app.rc",
        "assets/app.manifest",
        "assets/MultiMachineRust.ico",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=WINDRES");
    let target = env::var("TARGET").unwrap();
    if !target.contains("windows") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let object = out.join(if target.ends_with("msvc") {
        "app.res"
    } else {
        "app.o"
    });
    let mut cmd = if target.ends_with("msvc") {
        let mut c = Command::new("rc.exe");
        c.arg("/nologo")
            .arg("/Iassets")
            .arg(format!("/fo{}", object.display()))
            .arg("assets/app.rc");
        c
    } else {
        let tool = env::var_os("WINDRES").unwrap_or_else(|| "x86_64-w64-mingw32-windres".into());
        let mut c = Command::new(tool);
        c.args(["-I", "assets", "-i", "assets/app.rc", "-O", "coff", "-o"])
            .arg(&object);
        c
    };
    let status = cmd
        .status()
        .expect("Windows resource compiler required (rc.exe or MinGW windres)");
    assert!(status.success(), "Resource compilation failed");
    println!("cargo:rustc-link-arg-bins={}", object.display());
}
