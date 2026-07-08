use std::env;
use std::fs::File;
use std::path::Path;
use std::process::Command;

use gl_generator::{Api, Fallbacks, GlobalGenerator, Profile, Registry};

fn main() {
    let mut version = String::from(env!("CARGO_PKG_VERSION"));
    if let Some(commit_hash) = commit_hash() {
        version = format!("{version} ({commit_hash})");
    }
    println!("cargo:rustc-env=VERSION={version}");

    let dest = env::var("OUT_DIR").unwrap();
    let mut file = File::create(Path::new(&dest).join("gl_bindings.rs")).unwrap();

    Registry::new(Api::Gl, (3, 3), Profile::Core, Fallbacks::All, [
        "GL_ARB_blend_func_extended",
        "GL_KHR_robustness",
        "GL_KHR_debug",
    ])
    .write_bindings(GlobalGenerator, &mut file)
    .unwrap();

    #[cfg(windows)]
    {
        // Re-embed the icon whenever the .ico OR .rc changes. `embed_resource`
        // emits a rerun-if-changed for the .rc only, which pins Cargo to that
        // file and makes it silently skip .ico-only updates — leaving the stale
        // Nebula icon embedded in the exe. Declaring the .ico here fixes that.
        println!("cargo:rerun-if-changed=windows/nebula.ico");
        println!("cargo:rerun-if-changed=windows/nebula.rc");
        embed_resource::compile("./windows/nebula.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();

        deploy_conpty();
    }
}

/// Copy the side-by-side new-ConPTY (`conpty.dll` + `OpenConsole.exe`) next to
/// the built exe, mirroring Windows Terminal's OpenConsole strategy.
///
/// `conpty.rs` first tries to load a `conpty.dll` found through the normal DLL
/// search path, which includes the executable directory. That DLL then launches
/// its adjacent `OpenConsole.exe`. If either copy fails or the assets are
/// absent, the loader falls back to the in-box Windows ConPTY. We prefer the
/// bundled 1.22+ passthrough host by default because the in-box host can reflow
/// and re-emit the viewport on resize, which makes full-screen TUIs leave resize
/// redraws in scrollback.
#[cfg(windows)]
fn deploy_conpty() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    // workspace_root/nebula_app -> workspace_root
    let workspace_root = Path::new(&manifest_dir).parent().unwrap();
    let src_dir = workspace_root.join("assets").join("windows").join("conhost");

    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out; walk up to target/<profile>.
    let out_dir = env::var("OUT_DIR").unwrap();
    let exe_dir = Path::new(&out_dir)
        .ancestors()
        .nth(3)
        .expect("OUT_DIR has the expected target/<profile>/build/... shape");

    for name in ["conpty.dll", "OpenConsole.exe"] {
        let src = src_dir.join(name);
        let dest = exe_dir.join(name);
        println!("cargo:rerun-if-changed={}", src.display());
        if let Err(e) = std::fs::copy(&src, &dest) {
            // A locked dest (app still running) or missing asset must not fail
            // the build; the app just falls back to the in-box ConPTY.
            println!(
                "cargo:warning=could not deploy {name} ({e}); resize reflow may \
                 use the in-box ConPTY"
            );
        }
    }
}

fn commit_hash() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|hash| hash.trim().into())
}
