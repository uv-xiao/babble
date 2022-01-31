//! extract::partial implements a non-ILP-based extractor based on partial
//! orderings of learned library sets.
use egg::{Analysis, DidMerge, EGraph, Id};
use std::{collections::HashSet, fmt::Debug};

use crate::{
    ast_node::{Arity, AstNode},
    teachable::{BindingExpr, Teachable},
};

/// A `CostSet` is a set of pairs; each pair contains a set of library
/// functions paired with the cost of the current expression/eclass
/// without the lib fns, and the cost of the lib fns themselves.
#[derive(Debug, Clone)]
pub struct CostSet {
    // Invariant: always sorted in ascending order of
    // expr_cost + libs_cost
    set: Vec<LibSel>,
}

impl CostSet {
    pub fn intro_op() -> CostSet {
        println!("intro_op");
        CostSet {
            set: vec![LibSel::intro_op()],
        }
    }

    pub fn cross(&self, other: &CostSet) -> CostSet {
        println!("cross");
        let mut set = Vec::new();

        for ls1 in &self.set {
            for ls2 in &other.set {
                let ls = ls1.combine(ls2);

                match set.binary_search_by_key(&ls.full_cost, |ls: &LibSel| ls.full_cost) {
                    Ok(pos) => set.insert(pos, ls),
                    Err(pos) => set.insert(pos, ls),
                }
            }
        }

        CostSet { set }
    }

    // Combination without unification
    pub fn combine(&mut self, other: CostSet) {
        println!("combine");
        for elem in other.set {
            match self
                .set
                .binary_search_by_key(&elem.full_cost, |ls| ls.full_cost)
            {
                Ok(pos) => self.set.insert(pos, elem),
                Err(pos) => self.set.insert(pos, elem),
            }
        }
    }

    pub fn unify(&mut self) {
        println!("unify");
        // We already know s is in ascending order of cost.
        let mut i = 0;

        while i < self.set.len() {
            let mut j = i + 1;

            while j < self.set.len() {
                let ls1 = &self.set[i];
                let ls2 = &self.set[j];
                let mut rem = false;

                if ls1.is_subset(&ls2) {
                    rem = true;
                } else {
                    j += 1;
                }

                if rem {
                    self.set.remove(j);
                }
            }
            i += 1;
        }
    }

    pub fn inc_cost(&mut self) {
        println!("inc_cost");
        for ls in &mut self.set {
            ls.inc_cost();
        }
    }

    pub fn add_lib(&self, lib: Id, cost: &CostSet) -> CostSet {
        println!("add_lib");
        // To add a lib, we do a modified cross.
        let mut set = Vec::new();

        for ls1 in &cost.set {
            for ls2 in &self.set {
                let ls = ls2.add_lib(lib, ls1);

                match set.binary_search_by_key(&ls.full_cost, |ls: &LibSel| ls.full_cost) {
                    Ok(pos) => set.insert(pos, ls),
                    Err(pos) => set.insert(pos, ls),
                }
            }
        }

        CostSet { set }
    }

    pub fn prune(&mut self, n: usize) {
        println!("prune");
        // Only preserve the n best `LibSel`s in the set.
        if self.set.len() > n {
            self.set.drain(n..);
        }
    }
}

/// A `LibSel` is a selection of library functions, paired with two
/// corresponding cost values: the cost of the expression without the library
/// functions, and the cost of the library functions themselves
#[derive(Debug, Default, Clone)]
pub struct LibSel {
    libs: HashSet<(Id, usize)>,
    expr_cost: usize,
    // Memoized expr_cost + sum({ l.1 for l in libs })
    full_cost: usize,
}

impl LibSel {
    pub fn intro_op() -> LibSel {
        LibSel {
            libs: HashSet::new(),
            expr_cost: 1,
            full_cost: 1,
        }
    }

    pub fn is_subset(&self, other: &LibSel) -> bool {
        self.libs.is_subset(&other.libs) && self.expr_cost <= other.expr_cost
    }

    /// Combines two `LibSel`s. Unions the lib sets, adds
    /// the expr
    pub fn combine(&self, other: &LibSel) -> LibSel {
        let libs: HashSet<_> = self.libs.union(&other.libs).cloned().collect();
        let expr_cost = self.expr_cost + other.expr_cost;
        let libs_cost: usize = libs.iter().map(|x| x.1).sum();
        let full_cost = expr_cost + libs_cost;

        LibSel {
            libs,
            expr_cost,
            full_cost,
        }
    }

    pub fn add_lib(&self, lib: Id, cost: &LibSel) -> LibSel {
        let mut res = self.clone();

        if res.libs.insert((lib, cost.expr_cost)) {
            res.full_cost += cost.expr_cost;
        }

        res
    }

    pub fn inc_cost(&mut self) {
        self.expr_cost += 1;
        self.full_cost += 1;
    }
}

// --------------------------------
// --- The actual Analysis part ---
// --------------------------------

#[derive(Debug, Clone, Copy)]
pub struct PartialLibCost {
    /// The number of `LibSel`s to keep per EClass.
    beam_size: usize,
    // TODO: intermediate beam size for while we cross?
}

impl PartialLibCost {
    pub fn new(beam_size: usize) -> PartialLibCost {
        PartialLibCost { beam_size }
    }
}

impl<Op> Analysis<AstNode<Op>> for PartialLibCost
where
    Op: Ord + std::hash::Hash + Debug + Teachable + Arity + Eq + Clone + Send + Sync + 'static,
{
    type Data = CostSet;

    fn merge(&self, to: &mut Self::Data, from: Self::Data) -> DidMerge {
        // Merging consists of combination, followed by unification and beam
        // pruning.
        to.combine(from);
        to.unify();
        to.prune(self.beam_size);

        DidMerge(true, true)
    }

    fn make(egraph: &EGraph<AstNode<Op>, Self>, enode: &AstNode<Op>) -> Self::Data {
        let x = |i: &Id| &egraph[*i].data;

        match Teachable::as_binding_expr(enode) {
            Some(BindingExpr::Let(f, b)) => {
                // This is a lib binding!
                // cross e1, e2 and introduce a lib!
                let mut e = x(b).add_lib(*f, x(f));
                e.unify();
                // TODO: don't hardcore this
                e.prune(20); 
                e
            }
            Some(_) | None => {
                // This is some other operation of some kind.
                // We test the arity of the function

                if enode.is_empty() {
                    // 0 args. Return intro.
                    CostSet::intro_op()
                } else if enode.args().len() == 1 {
                    // 1 arg. Get child cost set, inc, and return.
                    let mut e = x(&enode.args()[0]).clone();
                    e.inc_cost();
                    e
                } else {
                    // 2+ args. Cross/unify time!
                    let mut e = x(&enode.args()[0]).clone();

                    for cs in &enode.args()[1..] {
                        e = e.cross(x(cs));
                        // Intermediate prune.
                        // TODO: don't hardcode this
                        e.prune(1000);
                    }

                    // TODO: intermediate unify/beam size reduction for each crossing step?
                    // do perf testing on this
                    e.unify();
                    // TODO: dont hardcode this
                    e.prune(20);
                    e
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}