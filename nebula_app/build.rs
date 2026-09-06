use std::env;
use std::path::Path;
use std::process::Command;

#[path = "build/i18n.rs"]
mod i18n;

#[cfg(feature = "legacy-shell")]
use gl_generator::{Api, Fallbacks, GlobalGenerator, Profile, Registry};
#[cfg(feature = "legacy-shell")]
use std::fs::File;

fn main() {
    i18n::generate(
        &Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("i18n"),
        &Path::new(&env::var("OUT_DIR").unwrap()).join("translations.rs"),
    )
    .unwrap_or_else(|error| panic!("translation catalog validation failed: {error}"));

    let mut version = String::from(env!("CARGO_PKG_VERSION"));
    if let Some(commit_hash) = commit_hash() {
        version = format!("{version} ({commit_hash})");
    }
    println!("cargo:rustc-env=VERSION={version}");

    #[cfg(feature = "legacy-shell")]
    generate_legacy_gl_bindings();

    // build script 里的 `cfg(windows)` 是**宿主**平台；交叉到 macOS/Linux 时
    // 宿主仍是 Windows，必须再看目标（`CARGO_CFG_WINDOWS` 只在目标是 Windows
    // 时设置），否则会往 Mach-O/ELF 里嵌 .rc 资源、给 Unix 包部署 ConPTY。
    #[cfg(windows)]
    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // Re-embed the icon whenever the .ico OR .rc changes. `embed_resource`
        // emits a rerun-if-changed for the .rc only, which pins Cargo to that
        // file and makes it silently skip .ico-only updates — leaving the stale
        // Nebula icon embedded in the exe. Declaring the .ico here fixes that.
        println!("cargo:rerun-if-changed=windows/nebula.ico");
        println!("cargo:rerun-if-changed=windows/nebula.rc");
        println!("cargo:rerun-if-changed=windows/nebula.manifest");
        let defines: &[&str] =
            if cfg!(feature = "gpui-shell") { &["NEBULA_GPUI_MANIFEST"] } else { &[] };
        embed_resource::compile("./windows/nebula.rc", defines).manifest_required().unwrap();

        deploy_conpty();
    }
}

#[cfg(feature = "legacy-shell")]
fn generate_legacy_gl_bindings() {
    let dest = env::var("OUT_DIR").unwrap();
    let mut file = File::create(Path::new(&dest).join("gl_bindings.rs")).unwrap();

    Registry::new(
        Api::Gl,
        (3, 3),
        Profile::Core,
        Fallbacks::All,
        ["GL_ARB_blend_func_extended", "GL_KHR_robustness", "GL_KHR_debug"],
    )
    .write_bindings(GlobalGenerator, &mut file)
    .unwrap();
}

/// Copy the side-by-side new-ConPTY (`conpty.dll` + `OpenConsole.exe`) next to
/// the built exe so the matching OpenConsole host is selected at runtime.
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
