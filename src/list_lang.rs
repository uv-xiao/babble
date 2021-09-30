//! The language of list transformations.

use crate::{
    ast_node::{Arity, AstNode},
    free_vars::{and, not_free_in, FreeVarAnalysis, FreeVars},
    learn::LearnedLibrary,
    lift_lib::LiftLib,
    teachable::{BindingExpr, DeBruijnIndex, Teachable},
};
use babble_macros::rewrite_rules;
use egg::{AstSize, Extractor, Rewrite, Runner, Symbol};
use lazy_static::lazy_static;
use std::{
    collections::HashSet,
    convert::Infallible,
    fmt::{self, Display, Formatter},
    str::FromStr,
};

/// List operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ListOp {
    /// Add an element to the front of a list
    Cons,
    /// A boolean literal
    Bool(bool),
    /// A conditional expression
    If,
    /// An integer literal
    Int(i32),
    /// A function application
    Apply,
    /// A de Bruijn-indexed variable
    Var(DeBruijnIndex),
    /// An identifier
    Ident(Symbol),
    /// An anonymous function
    Lambda,
    /// A let-expression
    Let,
    /// A library function binding
    Lib,
    /// A list
    List,
}

impl Arity for ListOp {
    fn min_arity(&self) -> usize {
        match self {
            Self::Bool(_) | Self::Int(_) | Self::Var(_) | Self::Ident(_) | Self::List => 0,
            Self::Lambda => 1,
            Self::Cons | Self::Apply => 2,
            Self::If | Self::Let | Self::Lib => 3,
        }
    }

    fn max_arity(&self) -> Option<usize> {
        match self {
            Self::List => None,
            other => Some(other.min_arity()),
        }
    }
}

impl Display for ListOp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Cons => "cons",
            Self::If => "if",
            Self::Apply => "@",
            Self::Lambda => "λ",
            Self::Let => "let",
            Self::Lib => "lib",
            Self::List => "list",
            Self::Bool(b) => {
                return write!(f, "{}", b);
            }
            Self::Int(i) => {
                return write!(f, "{}", i);
            }
            Self::Var(index) => {
                return write!(f, "{}", index);
            }
            Self::Ident(ident) => {
                return write!(f, "{}", ident);
            }
        };
        f.write_str(s)
    }
}
impl FromStr for ListOp {
    type Err = Infallible;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let op = match input {
            "cons" => Self::Cons,
            "if" => Self::If,
            "apply" | "@" => Self::Apply,
            "lambda" | "λ" => Self::Lambda,
            "let" => Self::Let,
            "lib" => Self::Lib,
            "list" => Self::List,
            input => input
                .parse()
                .map(Self::Bool)
                .or_else(|_| input.parse().map(Self::Var))
                .or_else(|_| input.parse().map(Self::Int))
                .unwrap_or_else(|_| Self::Ident(input.into())),
        };
        Ok(op)
    }
}

impl Teachable for ListOp {
    fn from_binding_expr<T>(binding_expr: BindingExpr<T>) -> AstNode<Self, T> {
        match binding_expr {
            BindingExpr::Lambda(body) => AstNode::new(Self::Lambda, [body]),
            BindingExpr::Apply(fun, arg) => AstNode::new(Self::Apply, [fun, arg]),
            BindingExpr::Index(index) => AstNode::leaf(Self::Var(DeBruijnIndex(index))),
            BindingExpr::Ident(ident) => AstNode::leaf(Self::Ident(ident)),
            BindingExpr::Lib { ident, value, body } => {
                AstNode::new(Self::Lib, [ident, value, body])
            }
        }
    }

    fn as_binding_expr<T>(node: &AstNode<Self, T>) -> Option<BindingExpr<&T>> {
        let binding_expr = match node.as_parts() {
            (Self::Lambda, [body]) => BindingExpr::Lambda(body),
            (Self::Apply, [fun, arg]) => BindingExpr::Apply(fun, arg),
            (&Self::Var(DeBruijnIndex(index)), []) => BindingExpr::Index(index),
            (&Self::Ident(ident), []) => BindingExpr::Ident(ident),
            (Self::Lib, [ident, value, body]) => BindingExpr::Lib { ident, value, body },
            _ => return None,
        };
        Some(binding_expr)
    }
}

impl FreeVars for ListOp {
    fn get_ident(&self) -> Option<Symbol> {
        match *self {
            Self::Ident(ident) => Some(ident),
            _ => None,
        }
    }

    fn free_vars(&self, children: &[&HashSet<Symbol>]) -> HashSet<Symbol> {
        match (self, children) {
            (&Self::Ident(ident), []) => {
                let mut result = HashSet::new();
                result.insert(ident);
                result
            }
            (Self::Let | Self::Lib, &[ident, val, body]) => &(body - ident) | val,
            (_, children) => children
                .iter()
                .flat_map(|child| child.iter())
                .copied()
                .collect(),
        }
    }
}

lazy_static! {
    static ref LIFT_LIB_REWRITES: &'static [Rewrite<AstNode<ListOp>, FreeVarAnalysis<ListOp>>] = {
        let mut rules = rewrite_rules! {
            // TODO: Check for captures of de Bruijn variables and re-index if necessary.
            lift_lambda: "(lambda (lib ?x ?v ?e))" => "(lib ?x ?v (lambda ?e))";

            // Binding expressions
            lift_let_both: "(let ?x1 (lib ?x2 ?v2 ?v1) (lib ?x2 ?v2 ?e))" => "(lib ?x2 ?v2 (let ?x1 ?v1 ?e))" if not_free_in("?v2", "?x1");
            lift_let_body: "(let ?x1 ?v1 (lib ?x2 ?v2 ?e))" => "(lib ?x2 ?v2 (let ?x1 ?v1 ?e))" if and(not_free_in("?v1", "?x2"), not_free_in("?v2", "?x1"));
            lift_let_binding: "(let ?x1 (lib ?x2 ?v2 ?v1) ?e)" => "(lib ?x2 ?v2 (let ?x1 ?v1 ?e))" if not_free_in("?e", "?x2");

            lift_lib_both: "(lib ?x1 (lib ?x2 ?v2 ?v1) (lib ?x2 ?v2 ?e))" => "(lib ?x2 ?v2 (lib ?x1 ?v1 ?e))" if not_free_in("?v2", "?x1");
            lift_lib_body: "(lib ?x1 ?v1 (lib ?x2 ?v2 ?e))" => "(lib ?x2 ?v2 (lib ?x1 ?v1 ?e))" if and(not_free_in("?v1", "?x2"), not_free_in("?v2", "?x1"));
            lift_lib_binding: "(lib ?x1 (lib ?x2 ?v2 ?v1) ?e)" => "(lib ?x2 ?v2 (lib ?x1 ?v1 ?e))" if not_free_in("?e", "?x2");
        };

        rules.extend([
            LiftLib::rewrite("lift_list", ListOp::List),
            LiftLib::rewrite("lift_if", ListOp::If),
            LiftLib::rewrite("lift_cons", ListOp::Cons),
            LiftLib::rewrite("lift_apply", ListOp::Apply),
        ]);

        rules.leak()
    };
}
/// Execute `EGraph` building and program extraction on a single expression
/// containing all of the programs to extract common fragments out of.
pub fn run_single(runner: Runner<AstNode<ListOp>, FreeVarAnalysis<ListOp>>) {
    let learned_lib = LearnedLibrary::from(&runner.egraph);
    let lib_rewrites: Vec<_> = learned_lib.rewrites().collect();

    let mut runner = runner.with_iter_limit(1).run(lib_rewrites.iter());
    runner.stop_reason = None;

    let runner = runner.with_iter_limit(30).run(*LIFT_LIB_REWRITES);
    // After running, `runner.stop_reason` is guaranteed to not be `None`.
    let stop_reason = runner.stop_reason.unwrap_or_else(|| unreachable!());
    eprintln!("Stop reason: {:?}", stop_reason);

    let num_iterations = runner.iterations.len() - 1;
    eprintln!("Number of iterations: {}", num_iterations);
    eprintln!("Number of nodes: {}", runner.egraph.total_size());

    let extractor = Extractor::new(&runner.egraph, AstSize);
    let (cost, expr) = extractor.find_best(runner.roots[0]);
    eprintln!("Cost: {}\n", cost);
    eprintln!("{}", expr.pretty(100));
}
