#[macro_use]
extern crate lalrpop_util;
#[cfg(feature = "lazy_static")]
#[macro_use]
extern crate lazy_static;

lalrpop_mod!(
    #[allow(clippy::all)]
    grammar
);

#[macro_use]
mod macros;
pub mod ast;
pub mod error;
pub mod function;
pub mod runtime;
pub mod scope;
pub mod stdlib;
pub mod time;
pub mod util;

#[cfg(feature = "playground")]
pub mod playground;
#[cfg(feature = "wasm")]
pub mod wasm;

pub use ast::{Expr, Ident};
pub use error::{Error, Result};
pub use runtime::{Evaluate, Value};
pub use scope::Scope;

use miniscript::{descriptor, policy};

pub type PolicyDpk = policy::concrete::Policy<descriptor::DescriptorPublicKey>;
pub type DescriptorDpk = descriptor::Descriptor<descriptor::DescriptorPublicKey>;
pub type MiniscriptDpk<Ctx> = miniscript::Miniscript<descriptor::DescriptorPublicKey, Ctx>;

pub fn parse(s: &str) -> Result<Expr> {
    let parser = grammar::ProgramParser::new();
    Ok(parser.parse(s)?)
}

pub fn eval(expr: Expr) -> Result<Value> {
    expr.eval(&Scope::root())
}

pub fn run(s: &str) -> Result<Value> {
    eval(parse(s)?)
}

pub fn parse_lib(s: &str) -> Result<ast::Library> {
    let parser = grammar::LibraryParser::new();
    Ok(parser.parse(s)?)
}
