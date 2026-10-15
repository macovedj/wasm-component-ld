use anyhow::{bail, Context, Result};
use std::env;
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

fn compile(args: &[&str], src: &str) -> Vec<u8> {
    Project::new().compile(args, src)
}

struct Project {
    tempdir: tempfile::TempDir,
}

impl Project {
    fn new() -> Project {
        Project {
            tempdir: tempfile::TempDir::new().unwrap(),
        }
    }
    fn file(&self, name: &str, contents: &str) {
        fs::write(self.tempdir.path().join(name), contents.as_bytes()).unwrap();
    }

    fn compile(&self, args: &[&str], src: &str) -> Vec<u8> {
        self.try_compile(args, src, true).unwrap()
    }

    fn try_compile(&self, args: &[&str], src: &str, inherit_stderr: bool) -> Result<Vec<u8>> {
        self.try_compile_with_linker(args, src, inherit_stderr, true)
    }

    fn compile_core(&self, args: &[&str], src: &str) -> Vec<u8> {
        self.try_compile_with_linker(args, src, true, false)
            .unwrap()
    }

    fn try_compile_with_linker(
        &self,
        args: &[&str],
        src: &str,
        inherit_stderr: bool,
        component_linker: bool,
    ) -> Result<Vec<u8>> {
        let myself = env!("CARGO_BIN_EXE_wasm-component-ld");
        let mut rustc = Command::new("rustc");
        rustc
            .arg("--target")
            .arg("wasm32-wasip1")
            .arg("-")
            .arg("-o")
            .arg("-");
        if component_linker {
            rustc.arg("-C").arg(format!("linker={myself}"));
        }
        rustc
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .current_dir(self.tempdir.path());
        if !inherit_stderr {
            rustc.stderr(Stdio::piped());
        }
        let mut rustc = rustc.spawn().context("failed to spawn rustc")?;

        rustc
            .stdin
            .take()
            .context("stdin should be present")?
            .write_all(src.as_bytes())
            .context("failed to write stdin")?;
        let output = rustc
            .wait_with_output()
            .context("failed to wait for subprocess")?;
        if !output.status.success() {
            let mut error = format!("subprocess failed: {:?}", output.status);
            if !output.stdout.is_empty() {
                error.push_str(&format!(
                    "\nstdout: {}",
                    String::from_utf8_lossy(&output.stdout)
                ));
            }
            if !output.stderr.is_empty() {
                error.push_str(&format!(
                    "\nstderr: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            bail!("{error}")
        }
        Ok(output.stdout)
    }
}

fn assert_component(bytes: &[u8]) {
    assert!(wasmparser::Parser::is_component(&bytes));
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all())
        .validate_all(&bytes)
        .unwrap();
}

fn assert_module(bytes: &[u8]) {
    assert!(wasmparser::Parser::is_core_wasm(&bytes));
    wasmparser::Validator::new().validate_all(&bytes).unwrap();
}

fn core_exports(module: &[u8]) -> Vec<String> {
    wasmparser::Parser::new(0)
        .parse_all(module)
        .flat_map(|payload| match payload.unwrap() {
            wasmparser::Payload::ExportSection(exports) => exports
                .into_iter()
                .map(|export| export.unwrap().name.to_owned())
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn core_module_with_export<'a>(component: &'a [u8], name: &str) -> &'a [u8] {
    wasmparser::Parser::new(0)
        .parse_all(component)
        .find_map(|payload| match payload.unwrap() {
            wasmparser::Payload::ModuleSection {
                unchecked_range, ..
            } => {
                let module =
                    &component[unchecked_range.start as usize..unchecked_range.end as usize];
                core_exports(module)
                    .iter()
                    .any(|export| export == name)
                    .then_some(module)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no core module exports {name}"))
}

// Exercise missing startup even when rustc supplies a CRT for WASI cdylibs.
const REACTOR_ARGS: &[&str] = &[
    "--crate-type=cdylib",
    "-Cpanic=abort",
    "-Clink-self-contained=no",
];

const REACTOR_SOURCE: &str = r#"
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! { loop {} }

static mut CTOR_RUNS: u32 = 0;

unsafe extern "C" { fn __wasm_call_ctors(); }

#[cfg(constructor_import)]
#[link(wasm_import_module = "probe:startup/state")]
unsafe extern "C" { fn notify(); }

#[cfg(not(no_constructors))]
extern "C" fn ctor() {
    unsafe {
        CTOR_RUNS += 1;
        #[cfg(constructor_import)]
        notify();
    }
}

#[cfg(not(no_constructors))]
#[used]
#[unsafe(link_section = ".init_array")]
static CTOR: extern "C" fn() = ctor;

// The retained wit-bindgen helper sets its own guard after constructors run.
#[cfg(legacy_helper)]
fn run_ctors_once() {
    static mut RUN: bool = false;
    unsafe {
        if !RUN {
            __wasm_call_ctors();
            RUN = true;
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ctor_runs() -> u32 {
    #[cfg(legacy_helper)]
    run_ctors_once();
    unsafe { CTOR_RUNS }
}

#[cfg(legacy_helper)]
#[unsafe(no_mangle)]
pub extern "C" fn second_export() -> u32 {
    // A second set of generated bindings can have an independent RUN flag.
    static mut RUN: bool = false;
    unsafe {
        if !RUN {
            __wasm_call_ctors();
            RUN = true;
        }
        CTOR_RUNS
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn raw_ctor_count() -> u32 {
    unsafe { CTOR_RUNS }
}

#[unsafe(no_mangle)]
pub extern "C" fn cabi_realloc(_: u32, _: u32, _: u32, _: u32) -> u32 {
    // The body is immaterial: LLD must not attach constructor calls to it.
    16
}

#[cfg(existing_initializer)]
#[cfg_attr(standard_initializer, unsafe(export_name = "cm32p2_initialize"))]
#[cfg_attr(not(standard_initializer), unsafe(no_mangle))]
pub extern "C" fn _initialize() {
    unsafe {
        __wasm_call_ctors();
        CTOR_RUNS += 100;
    }
}
"#;

struct CoreInstance {
    instance: wasmi::Instance,
    store: wasmi::Store<u32>,
}

impl CoreInstance {
    fn new(module: &[u8]) -> Self {
        let engine = wasmi::Engine::default();
        let module = wasmi::Module::new(&engine, module).unwrap();
        let mut store = wasmi::Store::new(&engine, 0);
        let mut linker = wasmi::Linker::new(&engine);
        linker
            .func_wrap(
                "probe:startup/state",
                "notify",
                |mut caller: wasmi::Caller<'_, u32>| *caller.data_mut() += 1,
            )
            .unwrap();
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        Self { instance, store }
    }

    fn count(&mut self, export: &str) -> i32 {
        self.instance
            .get_typed_func::<(), i32>(&self.store, export)
            .unwrap()
            .call(&mut self.store, ())
            .unwrap()
    }

    fn allocate(&mut self) {
        let ptr = self
            .instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&self.store, "cabi_realloc")
            .unwrap()
            .call(&mut self.store, (0, 0, 4, 16))
            .unwrap();
        assert_ne!(ptr, 0);
    }

    fn initialize(&mut self) {
        let name = ["cm32p2_initialize", "_initialize"]
            .into_iter()
            .find(|name| self.instance.get_func(&self.store, name).is_some())
            .expect("reactor must export an initializer");
        self.instance
            .get_typed_func::<(), ()>(&self.store, name)
            .unwrap()
            .call(&mut self.store, ())
            .unwrap();
    }
}

fn assert_constructor_lifecycle(output: &[u8], legacy_helper: bool, expected: i32) {
    assert_component(output);
    let module = core_module_with_export(output, "ctor_runs");
    let mut core = CoreInstance::new(module);
    assert_eq!(core.count("raw_ctor_count"), 0);
    core.allocate();
    assert_eq!(core.count("raw_ctor_count"), 0, "realloc ran constructors");
    core.initialize();
    assert_eq!(core.count("raw_ctor_count"), expected);
    assert_eq!(
        core.count("ctor_runs"),
        expected,
        "legacy helper reran constructors"
    );
    assert_eq!(core.count("ctor_runs"), expected);
    if legacy_helper {
        assert_eq!(
            core.count("second_export"),
            expected,
            "independent helper reran constructors"
        );
    }
    core.allocate();
    assert_eq!(core.count("raw_ctor_count"), expected);
}

#[test]
fn reactor_startup_supports_bindings_with_and_without_constructor_helpers() {
    for legacy_helper in [false, true] {
        let mut args = REACTOR_ARGS.to_vec();
        if legacy_helper {
            args.push("--cfg=legacy_helper");
        }
        let output = compile(&args, REACTOR_SOURCE);
        assert_constructor_lifecycle(&output, legacy_helper, 1);
    }
}

#[test]
fn reactor_startup_preserves_existing_initializers() {
    for standard in [false, true] {
        for legacy_helper in [false, true] {
            let mut args = REACTOR_ARGS.to_vec();
            args.push("--cfg=existing_initializer");
            if standard {
                args.push("--cfg=standard_initializer");
            }
            if legacy_helper {
                args.push("--cfg=legacy_helper");
            }
            let output = compile(&args, REACTOR_SOURCE);
            // Preserve the custom initializer's additional side effect.
            assert_constructor_lifecycle(&output, legacy_helper, 101);
        }
    }
}

#[test]
fn reactor_startup_supports_standard_names() {
    // Missing startup, a legacy initializer, and a standard initializer must
    // all work with strict component naming and either generation of bindings.
    for (existing, standard) in [(false, false), (true, false), (true, true)] {
        for legacy_helper in [false, true] {
            let mut args = REACTOR_ARGS.to_vec();
            args.push("-Clink-arg=--reject-legacy-names");
            if existing {
                args.push("--cfg=existing_initializer");
            }
            if standard {
                args.push("--cfg=standard_initializer");
            }
            if legacy_helper {
                args.push("--cfg=legacy_helper");
            }
            let output = compile(&args, REACTOR_SOURCE);
            assert!(core_exports(core_module_with_export(&output, "ctor_runs"))
                .iter()
                .any(|name| name == "cm32p2_initialize"));
            assert_constructor_lifecycle(&output, legacy_helper, if existing { 101 } else { 1 });
        }
    }
}

#[test]
fn reactor_startup_legacy_helpers_work_with_fat_lto() {
    let mut args = REACTOR_ARGS.to_vec();
    args.extend(["--cfg=legacy_helper", "-Clto=fat", "-Copt-level=3"]);
    let output = compile(&args, REACTOR_SOURCE);
    assert_constructor_lifecycle(&output, true, 1);
}

#[test]
fn reactor_startup_guards_explicit_constructor_exports() {
    let mut args = REACTOR_ARGS.to_vec();
    args.push("-Clink-arg=--export=__wasm_call_ctors");
    let output = compile(&args, REACTOR_SOURCE);
    assert_constructor_lifecycle(&output, false, 1);

    let mut core = CoreInstance::new(core_module_with_export(&output, "ctor_runs"));
    core.initialize();
    let ctors = core
        .instance
        .get_typed_func::<(), ()>(&core.store, "__wasm_call_ctors")
        .unwrap();
    ctors.call(&mut core.store, ()).unwrap();
    ctors.call(&mut core.store, ()).unwrap();
    assert_eq!(
        core.count("raw_ctor_count"),
        1,
        "the explicit constructor export bypassed the shared guard"
    );
}

#[test]
fn reactor_startup_preserves_user_constructor_wrappers() {
    let source = format!(
        "{REACTOR_SOURCE}{}",
        r#"
unsafe extern "C" { fn __real___wasm_call_ctors(); }

#[unsafe(no_mangle)]
pub extern "C" fn __wrap___wasm_call_ctors() {
    static mut RUN: bool = false;
    unsafe {
        if !RUN {
            RUN = true;
            __real___wasm_call_ctors();
        }
    }
}
"#,
    );
    for flags in [
        &["-Clink-arg=--wrap=__wasm_call_ctors"][..],
        &["-Clink-arg=-wrap", "-Clink-arg=__wasm_call_ctors"][..],
        &["-Clink-arg=--wrap", "-Clink-arg=__wasm_call_ctors"][..],
    ] {
        let mut args = REACTOR_ARGS.to_vec();
        args.extend(["--cfg=existing_initializer", "--cfg=legacy_helper"]);
        args.extend(flags);
        let direct = Project::new().compile_core(&args, &source);
        let output = compile(&args, &source);
        assert_component(&output);
        assert_eq!(
            direct,
            core_module_with_export(&output, "ctor_runs"),
            "modified a user-provided constructor policy with {flags:?}"
        );
        assert_constructor_lifecycle(&output, true, 101);
    }
}

#[test]
fn reactor_startup_calls_imports_after_allocation() {
    let project = Project::new();
    project.file(
        "reactor.wit",
        r#"
package probe:startup;
interface state { notify: func(); }
world reactor { import state; }
"#,
    );
    let mut args = REACTOR_ARGS.to_vec();
    args.extend([
        "--cfg=constructor_import",
        "--cfg=legacy_helper",
        "-Clink-arg=--component-type=reactor.wit",
    ]);
    let output = project.compile(&args, REACTOR_SOURCE);
    assert_component(&output);
    let mut core = CoreInstance::new(core_module_with_export(&output, "ctor_runs"));
    assert_eq!(*core.store.data(), 0);
    core.allocate();
    assert_eq!(*core.store.data(), 0, "allocator called constructor import");
    core.initialize();
    assert_eq!(*core.store.data(), 1);
    assert_eq!(core.count("ctor_runs"), 1);
    assert_eq!(core.count("second_export"), 1);
    core.allocate();
    assert_eq!(*core.store.data(), 1);
}

#[test]
fn reactor_without_constructors_needs_no_startup() {
    let mut args = REACTOR_ARGS.to_vec();
    args.extend(["--cfg=no_constructors", "--cfg=legacy_helper"]);
    let output = compile(&args, REACTOR_SOURCE);
    assert_component(&output);
    let module = core_module_with_export(&output, "ctor_runs");
    assert!(!core_exports(module)
        .iter()
        .any(|name| name == "_initialize" || name == "cm32p2_initialize"));
    let modules = wasmparser::Parser::new(0)
        .parse_all(&output)
        .filter(|payload| matches!(payload, Ok(wasmparser::Payload::ModuleSection { .. })))
        .count();
    assert_eq!(modules, 1, "empty constructors added a startup module");
    let mut core = CoreInstance::new(module);
    core.allocate();
    assert_eq!(core.count("ctor_runs"), 0);
    assert_eq!(core.count("second_export"), 0);
}

#[test]
fn reactor_without_constructors_preserves_custom_initialization() {
    let mut args = REACTOR_ARGS.to_vec();
    args.extend([
        "--cfg=no_constructors",
        "--cfg=legacy_helper",
        "--cfg=existing_initializer",
    ]);
    let output = compile(&args, REACTOR_SOURCE);
    // Empty constructor lists do not make an application's initializer empty.
    assert_constructor_lifecycle(&output, true, 100);
}

#[test]
fn reactor_without_constructors_preserves_legacy_initialization_with_standard_names() {
    let mut args = REACTOR_ARGS.to_vec();
    args.extend([
        "--cfg=no_constructors",
        "--cfg=legacy_helper",
        "--cfg=existing_initializer",
        "-Clink-arg=--reject-legacy-names",
    ]);
    let output = compile(&args, REACTOR_SOURCE);
    let module = core_module_with_export(&output, "ctor_runs");
    assert!(core_exports(module)
        .iter()
        .any(|name| name == "cm32p2_initialize"));
    assert_constructor_lifecycle(&output, true, 100);
    // The empty constructor optimization must still arrange component startup
    // for the meaningful legacy initializer through its standard alias.
    let modules = wasmparser::Parser::new(0)
        .parse_all(&output)
        .filter(|payload| matches!(payload, Ok(wasmparser::Payload::ModuleSection { .. })))
        .count();
    assert_eq!(modules, 2, "component omitted its initializer module");
}

#[test]
fn reactor_without_constructors_keeps_its_link_map() {
    let project = Project::new();
    let mut args = REACTOR_ARGS.to_vec();
    args.extend(["--cfg=no_constructors", "-Clink-arg=--Map=reactor.map"]);
    let output = project.compile(&args, REACTOR_SOURCE);
    assert_component(&output);
    let module = core_module_with_export(&output, "ctor_runs");
    assert!(!core_exports(module)
        .iter()
        .any(|name| name == "cm32p2_initialize"));

    let map = fs::read_to_string(project.tempdir.path().join("reactor.map")).unwrap();
    assert!(map.contains("ctor_runs"));
    assert!(
        !map.contains("__wrap___wasm_call_ctors"),
        "link map describes the discarded startup probe: {map}"
    );
    assert!(
        !map.contains("__wasm_component_ctors_ran"),
        "link map describes a guard absent from the retained module: {map}"
    );
}

#[test]
fn reactor_startup_preserves_command_module() {
    let source = "fn main() {}";
    let direct = Project::new().compile_core(&[], source);
    let output = compile(&[], source);
    assert_component(&output);
    assert_eq!(direct, core_module_with_export(&output, "_start"));
}

#[test]
fn reactor_startup_does_not_change_skipped_components() {
    let direct = Project::new().compile_core(REACTOR_ARGS, REACTOR_SOURCE);
    let mut args = REACTOR_ARGS.to_vec();
    args.push("-Clink-arg=--skip-wit-component");
    let output = compile(&args, REACTOR_SOURCE);
    assert_module(&output);
    assert_eq!(direct, output);
}

#[test]
fn empty() {
    let output = compile(&["--crate-type", "cdylib"], "");
    assert_component(&output);
}

#[test]
fn empty_main() {
    let output = compile(&[], "fn main() {}");
    assert_component(&output);
}

#[test]
fn hello_world() {
    let output = compile(
        &[],
        r#"
fn main() {
    println!("hello!");
}
"#,
    );
    assert_component(&output);
}

#[test]
fn cdylib_arbitrary_export() {
    let output = compile(
        &["--crate-type", "cdylib"],
        r#"
#[no_mangle]
pub extern "C" fn foo() {
    println!("x");
}
        "#,
    );
    assert_component(&output);
}

#[test]
fn can_access_badfd() {
    let output = compile(
        &[],
        r#"
#[link(wasm_import_module = "wasi_snapshot_preview1")]
extern "C" {
    fn adapter_open_badfd(fd: &mut u32) -> u32;
}

fn main() {
    let mut fd = 0;
    let rc = unsafe {
        adapter_open_badfd(&mut fd)
    };
    assert_eq!(rc, 0);
    assert_eq!(fd, 3);
}
        "#,
    );
    assert_component(&output);
}

#[test]
fn linker_flags() {
    let output = compile(
        &[
            "-Clink-arg=--max-memory=65536",
            "-Clink-arg=-zstack-size=32",
            "-Clink-arg=--global-base=2048",
            "-Clink-arg=--append-lld-flag=--no-merge-data-segments",
        ],
        r#"
fn main() {
}
        "#,
    );
    assert_component(&output);

    let output = compile(
        &[
            "-Clink-arg=--max-memory=65536",
            "-Clink-arg=-zstack-size=32",
            "-Clink-arg=--global-base=2048",
            "-Clink-arg=--append-lld-flag=--no-merge-data-segments",
            "-Clink-arg=-allow-multiple-definition",
            "-Clink-arg=-soname",
            "-Clink-arg=foo",
        ],
        r#"
fn main() {
}
        "#,
    );
    assert_component(&output);
}

#[test]
fn invalid_lld_flag() {
    let result = Project::new().try_compile(
        &["-Clink-arg=--append-lld-flag=--nonexistent-lld-flag"],
        r#"
fn main() {
    println!("hello!");
}
"#,
        false,
    );
    let err = result.unwrap_err();
    let err = format!("{err:?}");
    println!("error: {err}");
    assert!(err.contains("error: unknown argument: --nonexistent-lld-flag"));
}

#[test]
fn component_type_wit_file() {
    let project = Project::new();
    project.file(
        "foo.wit",
        r#"
package foo:bar;

interface foo {
  bar: func(s: string) -> string;
  baz: get() -> s32;
}

world root {
  import foo;
  export foo;
}
"#,
    );
    let output = project.compile(
        &[
            "-Clink-arg=--component-type",
            "-Clink-arg=foo.wit",
            "-Clink-arg=--string-encoding",
            "-Clink-arg=utf16",
            "--crate-type",
            "cdylib",
        ],
        r#"
#[no_mangle]
pub extern "C" fn cabi_realloc(ptr: *mut u8, old_size: i32, align: i32, new_size: i32) -> *mut u8 {
    _ = (ptr, old_size, align, new_size);
    unreachable!()
}

#[link(wasm_import_module = "foo:bar/foo")]
extern "C" {
    #[link_name = "bar"]
    fn import1(ptr: *mut u8, len: i32, return_ptr: *mut *mut u8);
    #[link_name = "[get]baz"]
    fn import2() -> i32;
}

#[export_name = "foo:bar/foo#bar"]
pub unsafe extern "C" fn export1(ptr: *mut u8, len: i32) -> *mut u8 {
    let mut result = std::ptr::null_mut();
    import1(ptr, len, &mut result);
    result
}

#[export_name = "foo:bar/foo#[get]baz"]
pub unsafe extern "C" fn export2() -> i32 {
    import2()
}
"#,
    );
    assert_component(&output);
}

#[test]
fn skip_component() {
    let output = compile(
        &["-Clink-arg=--skip-wit-component"],
        r#"
fn main() {
}
        "#,
    );
    assert_module(&output);
}

#[test]
fn rustc_using_argfile() {
    let prefix = (0..200).map(|_| 'a').collect::<String>();
    let p = Project {
        tempdir: tempfile::TempDir::with_prefix(&prefix).unwrap(),
    };

    let mut src = String::new();
    for i in 0..1000 {
        src.push_str(&format!("mod m{i};\n"));
        p.file(
            &format!("m{i}.rs"),
            &format!("#[no_mangle] pub extern \"C\" fn f{i}() {{}}"),
        );
    }
    src.push_str("fn main() {}");

    p.file("args", "@args2");
    p.file("args2", "--skip-wit-component");
    let output = p.compile(&["-Ccodegen-units=1000", "-Clink-arg=@args"], &src);
    assert_module(&output);
}

#[test]
fn hello_world_with_realloc_as_memory_grow() {
    let output = compile(
        &["-Clink-arg=--realloc-via-memory-grow"],
        r#"
fn main() {
    println!("hello!");
}
"#,
    );
    assert_component(&output);
}

// The adapter uses legacy names, so this option will always result in an error
// right now.
#[test]
fn legacy_names_currently_required() {
    let result = Project::new().try_compile(
        &["-Clink-arg=--reject-legacy-names"],
        r#"
fn main() {
    println!("hello!");
}
"#,
        false,
    );
    let err = result.unwrap_err();
    let err = format!("{err:?}");
    println!("error: {err}");
    assert!(err.contains("unknown or invalid component model import syntax"));
}

#[test]
fn multiple_flags_accepted() {
    let output = compile(
        &[
            "-Clink-arg=--skip-wit-component",
            "-Clink-arg=--skip-wit-component",
        ],
        r#"
fn main() {
}
        "#,
    );
    assert_module(&output);
}
