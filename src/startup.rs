//! Link-time constructor guard shared by startup and older generated bindings.
//!
//! This is a relocatable WebAssembly object, rather than an edit to linked
//! function bodies. LLD can consequently relocate its references and retain
//! debug information while wrapping every input reference to the constructor
//! dispatcher.

use std::borrow::Cow;

use wasm_encoder::{
    CodeSection, ConstExpr, CustomSection, Encode, EntityType, FunctionSection, GlobalSection,
    GlobalType, ImportSection, LinkingSection, Module, SymbolTable, TypeSection, ValType,
};

pub const WRAP_CTORS: &str = "__wrap___wasm_call_ctors";
pub const REAL_CTORS: &str = "__real___wasm_call_ctors";

/// Build an object for LLD's `--wrap=__wasm_call_ctors` option.
///
/// The wrapper's private mutable global belongs to the core module instance,
/// so startup and calls from independently generated bindings share one guard.
/// Set it before calling constructors to prevent recursive constructor entry.
pub fn object() -> Vec<u8> {
    let mut module = Module::new();

    let mut types = TypeSection::new();
    types.ty().function([], []);
    module.section(&types);

    let mut imports = ImportSection::new();
    imports.import("env", REAL_CTORS, EntityType::Function(0));
    module.section(&imports);

    let mut functions = FunctionSection::new();
    functions.function(0);
    module.section(&functions);

    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(0),
    );
    module.section(&globals);

    // Object-file relocations need five-byte LEB operands: the final global
    // and function indices are assigned by LLD and may exceed one byte.
    let mut body = vec![0]; // zero local declarations
    let get_guard = index_instruction(&mut body, 0x23); // global.get guard
    body.extend_from_slice(&[0x04, 0x40, 0x0f, 0x0b]); // if { return }
    body.extend_from_slice(&[0x41, 0x01]); // i32.const 1
    let set_guard = index_instruction(&mut body, 0x24); // global.set guard
    let call_ctors = index_instruction(&mut body, 0x10); // call original ctors
    body.push(0x0b); // end function

    // Relocation offsets are relative to the code section payload, including
    // its function count and the function's encoded byte length.
    let mut prefix = Vec::new();
    1_u32.encode(&mut prefix);
    (body.len() as u32).encode(&mut prefix);
    let body_offset = prefix.len() as u32;

    let mut code = CodeSection::new();
    code.raw(&body);
    module.section(&code);

    let mut symbols = SymbolTable::new();
    symbols.function(SymbolTable::WASM_SYM_VISIBILITY_HIDDEN, 1, Some(WRAP_CTORS));
    symbols.global(
        SymbolTable::WASM_SYM_BINDING_LOCAL,
        0,
        Some("__wasm_component_ctors_ran"),
    );
    symbols.function(SymbolTable::WASM_SYM_UNDEFINED, 0, None);
    let mut linking = LinkingSection::new();
    linking.symbol_table(&symbols);
    module.section(&linking);

    let mut relocations = Vec::new();
    4_u32.encode(&mut relocations); // code is the fifth section
    3_u32.encode(&mut relocations); // three relocation entries
    for (kind, offset, symbol) in [
        (7_u32, get_guard, 1_u32),  // R_WASM_GLOBAL_INDEX_LEB
        (7_u32, set_guard, 1_u32),  // R_WASM_GLOBAL_INDEX_LEB
        (0_u32, call_ctors, 2_u32), // R_WASM_FUNCTION_INDEX_LEB
    ] {
        kind.encode(&mut relocations);
        (body_offset + offset).encode(&mut relocations);
        symbol.encode(&mut relocations);
    }
    module.section(&CustomSection {
        name: Cow::Borrowed("reloc.CODE"),
        data: Cow::Owned(relocations),
    });

    module.finish()
}

/// Append an instruction with a relocation-sized index and return its offset.
fn index_instruction(body: &mut Vec<u8>, opcode: u8) -> u32 {
    body.push(opcode);
    let offset = body.len() as u32;
    body.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0]);
    offset
}

#[cfg(test)]
mod tests {
    use super::object;
    use wasmparser::{KnownCustom, Parser, Payload, RelocationType, Validator};

    #[test]
    fn object_has_valid_code_and_linker_relocations() {
        let wasm = object();
        Validator::new().validate_all(&wasm).unwrap();

        let mut symbols = 0;
        let mut relocations = Vec::new();
        let mut code_range = None;
        for payload in Parser::new(0).parse_all(&wasm) {
            match payload.unwrap() {
                Payload::CodeSectionStart { range, .. } => code_range = Some(range),
                Payload::CustomSection(section) => match section.as_known() {
                    KnownCustom::Linking(linking) => {
                        for subsection in linking {
                            if let wasmparser::Linking::SymbolTable(table) = subsection.unwrap() {
                                symbols = table.count();
                            }
                        }
                    }
                    KnownCustom::Reloc(reloc) => {
                        assert_eq!(reloc.section_index(), 4);
                        relocations.extend(reloc.entries().into_iter().map(Result::unwrap));
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        assert_eq!(symbols, 3);
        assert_eq!(relocations.len(), 3);
        let code_range = code_range.unwrap();
        for relocation in &relocations {
            let offset = code_range.start as usize + relocation.offset as usize;
            assert_eq!(&wasm[offset..offset + 5], &[0x80, 0x80, 0x80, 0x80, 0]);
        }
        assert_eq!(relocations[0].ty, RelocationType::GlobalIndexLeb);
        assert_eq!(relocations[0].index, 1);
        assert_eq!(relocations[1].ty, RelocationType::GlobalIndexLeb);
        assert_eq!(relocations[1].index, 1);
        assert_eq!(relocations[2].ty, RelocationType::FunctionIndexLeb);
        assert_eq!(relocations[2].index, 2);
    }
}
