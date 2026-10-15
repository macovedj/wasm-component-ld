# `wasm-component-ld`

This crate contains a binary named `wasm-component-ld` which is a wrapper around
two pieces of functionality used to produce a [WebAssembly Component]

1. The `wasm-ld` linker driver provided by LLVM
2. The [`wit_component::ComponentEncoder`] type

This binary will first invoke `wasm-ld` and then run the componentization
process to produce a final component.

[WebAssembly Component]: https://component-model.bytecodealliance.org/
[`wit_component::ComponentEncoder`]: https://docs.rs/wit-component/latest/wit_component/struct.ComponentEncoder.html

## Installation

This repository provides [precompiled
binaries](https://github.com/bytecodealliance/wasm-component-ld/releases) of
`wasm-component-ld`. This repository can also be installed with [`cargo binstall`].

Installations of [wasi-sdk] have this binary packaged by default in the sysroot
and the Rust `wasm32-wasip2` target, upon reaching tier 2, will also come
packaged with this binary included.

This means that while a version can be installed manually it should not be
required to do so.

[`cargo binstall`]: https://github.com/cargo-bins/cargo-binstall
[wasi-sdk]: https://github.com/WebAssembly/wasi-sdk

## Options

The `wasm-component-ld` binary is suitable to use as a linker driver during
compilations. For Clang and Rust the `wasm32-wasip2` target will automatically
invoke this binary as the linker.

This means that `wasm-component-ld` forwards most of its arguments to `wasm-ld`.
Additionally all flags of `wasm-ld` are supported and forwarded to `wasm-ld`.
For example you can invoke the linker like `wasm-component-ld --max-memory=N
...`.

The `wasm-component-ld` binary has a few custom arguments for itself as well
which are not forwarded to `wasm-ld` and can be explored with `-h` or `--help`.

### Reactor initialization

Reactor components initialize constructors automatically, including components
using older bindings that call `__wasm_call_ctors` at export entry. The linker
uses LLD's `--wrap` support to route constructor calls through one guard per
core module instance. Component startup and existing bindings share that guard,
so calling an older bindings helper after startup does not rerun constructors.
Allocator exports do not run constructors.

Existing `_initialize` and `cm32p2_initialize` exports are preserved. If neither
exists, a `cm32p2_initialize` export invokes the guarded constructor dispatcher
once imports are connected. An empty constructor dispatcher needs no guard or
additional initialization module; the original linked core module is retained.

This requires no bindings option or application feature. Rust builds targeting
`wasm32-unknown-unknown` still need to select `wasm-component-ld` as their linker;
the target's default linker does not produce components.

Reactor builds perform a second link, including LTO, to add the shared guard
and suppress LLD's per-export constructor/destructor wrappers. Commands with
`_start`, explicit `--skip-wit-component`, shared-library output, and callers
explicitly wrapping `__wasm_call_ctors` keep their existing startup behavior.
Empty-constructor builds requesting linker maps, reproduction archives, or
saved intermediates perform one additional link to keep those artifacts in
sync with the retained module.

# License

This project is triple licenced under the Apache 2/ Apache 2 with LLVM exceptions/ MIT licences. The reasoning for this is:
- Apache 2/ MIT is common in the rust ecosystem.
- Apache 2/ MIT is used in the rust standard library, and some of this code may be migrated there.
- Some of this code may be used in compiler output, and the Apache 2 with LLVM exceptions licence is useful for this.

For more details see
- [Apache 2 Licence](LICENSE-APACHE)
- [Apache 2 Licence with LLVM exceptions](LICENSE-Apache-2.0_WITH_LLVM-exception)
- [MIT Licence](LICENSE-MIT)

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache 2/ Apache 2 with LLVM exceptions/ MIT licenses,
shall be licensed as above, without any additional terms or conditions.
