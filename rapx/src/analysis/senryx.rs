pub mod contracts;
#[allow(unused)]
pub mod dominated_chain;
pub mod generic_check;
pub mod inter_record;
pub mod matcher;
#[allow(unused)]
pub mod visitor;
pub mod visitor_check;
use crate::analysis::utils::fn_info::*;
use crate::{
    analysis::unsafety_isolation::{
        hir_visitor::{ContainsUnsafe, RelatedFnCollector},
        UnsafetyIsolationCheck,
    },
    rap_info, rap_warn,
};
use inter_record::InterAnalysisRecord;
use rustc_hir::def_id::DefId;
use rustc_middle::mir::{BasicBlock, Operand, TerminatorKind};
use rustc_middle::ty;
use rustc_middle::ty::TyCtxt;
use std::collections::{HashMap, HashSet};
use visitor::{BodyVisitor, CheckResult};

use super::core::alias::mop::MopAlias;
use super::core::alias::FnMap;

macro_rules! cond_print {
    ($cond:expr, $($t:tt)*) => {if $cond {rap_warn!($($t)*)} else {rap_info!($($t)*)}};
}

pub enum CheckLevel {
    High,
    Medium,
    Low,
}

pub struct SenryxCheck<'tcx> {
    pub tcx: TyCtxt<'tcx>,
    pub threshhold: usize,
    pub global_recorder: HashMap<DefId, InterAnalysisRecord<'tcx>>,
}

impl<'tcx> SenryxCheck<'tcx> {
    pub fn new(tcx: TyCtxt<'tcx>, threshhold: usize) -> Self {
        Self {
            tcx,
            threshhold,
            global_recorder: HashMap::new(),
        }
    }

    pub fn start(&mut self, check_level: CheckLevel, is_verify: bool) {
        let hir_map = self.tcx.hir().clone();
        let tcx = self.tcx;
        let mut mop = MopAlias::new(self.tcx);
        let fn_map = mop.start();
        let related_items = RelatedFnCollector::collect(tcx);
        for (_, &ref vec) in &related_items.clone() {
            for (body_id, _span) in vec {
                let (function_unsafe, block_unsafe) =
                    ContainsUnsafe::contains_unsafe(tcx, *body_id);
                let def_id = hir_map.body_owner_def_id(*body_id).to_def_id();
                if !Self::filter_by_check_level(tcx, &check_level, def_id) {
                    continue;
                }
                if block_unsafe
                    && is_verify
                    && get_all_std_unsafe_callees(self.tcx, def_id).len() > 0
                {
                    self.check_soundness(def_id, &fn_map);
                }
                if function_unsafe
                    && !is_verify
                    && get_all_std_unsafe_callees(self.tcx, def_id).len() > 0
                {
                    self.annotate_safety(def_id);
                    // let mutable_methods = get_all_mutable_methods(self.tcx, def_id);
                    // println!("mutable_methods: {:?}", mutable_methods);
                }
            }
        }
    }

    pub fn filter_by_check_level(
        tcx: TyCtxt<'tcx>,
        check_level: &CheckLevel,
        def_id: DefId,
    ) -> bool {
        match *check_level {
            CheckLevel::High => check_visibility(tcx, def_id),
            _ => true,
        }
    }

    pub fn check_soundness(&mut self, def_id: DefId, fn_map: &FnMap) {
        let check_results = self.body_visit_and_check(def_id, fn_map);
        let tcx = self.tcx;
        if check_results.len() > 0 {
            Self::show_check_results(tcx, def_id, check_results);
        }
    }

    pub fn annotate_safety(&self, def_id: DefId) {
        let annotation_results = self.get_annotation(def_id);
        if annotation_results.len() == 0 {
            return;
        }
        Self::show_annotate_results(self.tcx, def_id, annotation_results);
    }

    pub fn body_visit_and_check(&mut self, def_id: DefId, fn_map: &FnMap) -> Vec<CheckResult> {
        let mut body_visitor = BodyVisitor::new(self.tcx, def_id, self.global_recorder.clone(), 0);
        if get_type(self.tcx, def_id) == 1 {
            let func_cons = get_cons(self.tcx, def_id);
            for func_con in func_cons {
                let mut cons_body_visitor =
                    BodyVisitor::new(self.tcx, func_con.0, self.global_recorder.clone(), 0);
                cons_body_visitor.path_forward_check(fn_map);
                // TODO: cache and merge fields' states
            }
            // TODO: update method body's states by constructors' states

            // get mutable methods and TODO:update target method's states
            let _mutable_methods = get_all_mutable_methods(self.tcx, def_id);

            // analyze body's states
            body_visitor.path_forward_check(fn_map);
        } else {
            body_visitor.path_forward_check(fn_map);
        }
        return body_visitor.check_results;
    }

    pub fn body_visit_and_check_uig(&self, def_id: DefId) {
        let mut uig_checker = UnsafetyIsolationCheck::new(self.tcx);
        let func_type = get_type(self.tcx, def_id);
        if func_type == 1 && self.get_annotation(def_id).len() > 0 {
            let func_cons = uig_checker.search_constructor(def_id);
            for func_con in func_cons {
                if check_safety(self.tcx, func_con) {
                    Self::show_annotate_results(self.tcx, func_con, self.get_annotation(def_id));
                    // uphold safety to unsafe constructor
                }
            }
        }
    }

    pub fn get_annotation(&self, def_id: DefId) -> HashSet<String> {
        let mut results = HashSet::new();
        if !self.tcx.is_mir_available(def_id) {
            return results;
        }
        let body = self.tcx.optimized_mir(def_id);
        let basicblocks = &body.basic_blocks;
        for i in 0..basicblocks.len() {
            let iter = BasicBlock::from(i);
            let terminator = basicblocks[iter].terminator.clone().unwrap();
            match terminator.kind {
                TerminatorKind::Call {
                    ref func,
                    args: _,
                    destination: _,
                    target: _,
                    unwind: _,
                    call_source: _,
                    fn_span: _,
                } => match func {
                    Operand::Constant(c) => match c.ty().kind() {
                        ty::FnDef(id, ..) => {
                            if get_sp(self.tcx, *id).len() > 0 {
                                results.extend(get_sp(self.tcx, *id));
                            } else {
                                results.extend(self.get_annotation(*id));
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                },
                _ => {}
            }
        }
        results
    }

    pub fn show_check_results(tcx: TyCtxt<'tcx>, def_id: DefId, check_results: Vec<CheckResult>) {
        rap_info!(
            "--------In safe function {:?}---------",
            get_cleaned_def_path_name(tcx, def_id)
        );
        for check_result in &check_results {
            cond_print!(
                check_result.failed_contracts.len() > 0,
                "  Use unsafe api {:?}.",
                check_result.func_name
            );
            for failed_contract in &check_result.failed_contracts {
                cond_print!(
                    check_result.failed_contracts.len() > 0,
                    "      Argument {}'s failed Sps: {:?}",
                    failed_contract.0,
                    failed_contract.1
                );
            }
        }
    }

    pub fn show_annotate_results(
        tcx: TyCtxt<'tcx>,
        def_id: DefId,
        annotation_results: HashSet<String>,
    ) {
        rap_info!(
            "--------In unsafe function {:?}---------",
            get_cleaned_def_path_name(tcx, def_id)
        );
        rap_warn!("Lack safety annotations: {:?}.", annotation_results);
    }
}
