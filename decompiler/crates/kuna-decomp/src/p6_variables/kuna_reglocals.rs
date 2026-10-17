//! (kuna `reglocals`) The locals the C declares outside the stack frame, on the
//! `decompile-all --json` `variables` surface (P6, JSON only).
//!
//! `extract_variables` reports the parameters and the stack frame.  A local the
//! C body declares whose storage is a register, a temporary, or nothing at all
//! (the `vN` of an optimized function) is in neither, so the surface reported no
//! row for it while the C declared it.  [`register_locals`] reports exactly the
//! declarations the printer just emitted that no parameter or frame row covers,
//! under the printed name and declarator, so a consumer can join each row to its
//! declaration line.  [`DeclarationHighs`] gives each row the uses of every
//! HighVariable its declaration covers, the same markup-backed evidence the
//! other rows carry.
//!
//! Read from the printer after the `print_c` of the same function: nothing here
//! feeds the IR or the emitted C.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;

use kuna_base::space::AddrSpace;

use crate::context::HighVariableId;
use crate::decompile_drive::VarInfo;
use crate::funcdata::Funcdata;
use crate::infra::architecture::Architecture;
use crate::printc::PrintC;

/// Is `high` already reported as part of the frame: no storage but the address
/// constants of a frame object, or a stack instance inside a reported frame row?
fn reported_in_frame(
    fd: &Funcdata,
    high: HighVariableId,
    stack_space: Option<&Rc<AddrSpace>>,
    rows: &[(i64, i64)],
) -> bool {
    let Some(h) = fd.high_bank().get(high) else { return true };
    let mut stored = false;
    for i in 0..h.num_instances() {
        let Some(v) = fd.vbank().get(h.get_instance(i)) else { continue };
        if v.is_constant() {
            continue;
        }
        stored = true;
        let Some(stack) = stack_space else { continue };
        if v.get_addr().get_space().map(|s| s.get_index()) != Some(stack.get_index()) {
            continue;
        }
        let lo = crate::decompile_drive::signed_space_offset(stack, v.get_offset());
        let hi = lo + i64::from(v.get_size());
        if rows.iter().any(|&(a, b)| a < hi && lo < b) {
            return true;
        }
    }
    !stored
}

fn widest_instance(fd: &Funcdata, high: HighVariableId) -> i64 {
    let Some(h) = fd.high_bank().get(high) else { return 0 };
    (0..h.num_instances())
        .filter_map(|i| fd.vbank().get(h.get_instance(i)))
        .map(|v| i64::from(v.get_size()))
        .max()
        .unwrap_or(0)
}

/// The rows for the locals `arch`'s printer declared for `fd` that are neither
/// frame-resident nor already in `reported`, in declaration order.
pub fn register_locals(arch: &Architecture, fd: &Funcdata, reported: &[VarInfo]) -> Vec<VarInfo> {
    let taken: HashSet<&str> = reported.iter().map(|v| v.name.as_str()).collect();
    let rows: Vec<(i64, i64)> = reported
        .iter()
        .filter_map(|v| v.stack_offset.map(|off| (off, off + v.size.max(1))))
        .collect();
    let stack_space = fd.get_scope_local().map(|scope| scope.get_space_id());
    arch.print()
        .declared_locals()
        .iter()
        .filter(|local| !taken.contains(local.name.as_str()))
        .filter(|local| !reported_in_frame(fd, local.high, stack_space, &rows))
        .map(|local| {
            let type_name = match &local.ty {
                Some(ty) if arch.byte_honest && crate::kuna_bytehonest::is_uncommitted_byte(ty) => {
                    "undefined1".to_string()
                }
                _ => local.decl.clone(),
            };
            let size = local
                .ty
                .as_ref()
                .map(|ty| i64::from(ty.get_size()))
                .unwrap_or_else(|| widest_instance(fd, local.high));
            VarInfo {
                name: local.name.clone(),
                type_name,
                stack_offset: None,
                size,
                is_param: false,
                arg_index: None,
                line_numbers: Vec::new(),
                addresses: Vec::new(),
            }
        })
        .collect()
}

/// Each printed declaration's name and the highs it covers: the declared high
/// and every high the printer declared through it.
pub struct DeclarationHighs(BTreeMap<String, Vec<HighVariableId>>);

impl DeclarationHighs {
    pub fn of(print: &PrintC) -> Self {
        let mut through = print.declared_through();
        DeclarationHighs(
            print
                .declared_locals()
                .iter()
                .map(|local| {
                    let mut highs = vec![local.high];
                    highs.extend(through.remove(&local.high).unwrap_or_default());
                    (local.name.clone(), highs)
                })
                .collect(),
        )
    }

    /// The varrefs of every instance of the highs the declaration `name` covers.
    pub fn varrefs(&self, fd: &Funcdata, name: &str) -> BTreeSet<u64> {
        let mut out = BTreeSet::new();
        for &high in self.0.get(name).into_iter().flatten() {
            let Some(h) = fd.high_bank().get(high) else { continue };
            for i in 0..h.num_instances() {
                if let Some(v) = fd.vbank().get(h.get_instance(i)) {
                    out.insert(v.get_create_index() as u64);
                }
            }
        }
        out
    }

    /// The instruction addresses of the ops that define or read an instance of
    /// the highs the declaration `name` covers.
    pub fn op_addresses(&self, fd: &Funcdata, name: &str) -> Vec<u64> {
        use kuna_num::opcodes::OpCode;
        let mut out = BTreeSet::new();
        for &high in self.0.get(name).into_iter().flatten() {
            let Some(h) = fd.high_bank().get(high) else { continue };
            for i in 0..h.num_instances() {
                let Some(v) = fd.vbank().get(h.get_instance(i)) else { continue };
                for op_id in v.get_def().into_iter().chain(v.descend_iter()) {
                    let Some(op) = fd.obank().get(op_id) else { continue };
                    if matches!(op.code(), OpCode::CPUI_MULTIEQUAL | OpCode::CPUI_INDIRECT) {
                        continue;
                    }
                    if op.get_addr().get_space().is_some() {
                        out.insert(op.get_addr().get_offset());
                    }
                }
            }
        }
        out.into_iter().collect()
    }
}
