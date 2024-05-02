use std::fmt;

use crate::ast::{self, Expr, Ident};
use crate::runtime::{Error, Evaluate, Result, Scope, Value};

#[derive(Debug, Clone)]
pub enum Function {
    User(UserFunction),
    Native(NativeFunction),
}

/// A user-defined function implemented in Minsc
#[derive(Debug, Clone)]
pub struct UserFunction {
    pub ident: Ident,
    pub signature: Vec<Ident>,
    pub body: Expr,
}
impl_from_variant!(UserFunction, Function, User);

/// A native function implemented in Rust
#[derive(Clone)]
pub struct NativeFunction(pub NativeFunctionPt);

pub type NativeFunctionPt = fn(Vec<Value>, &Scope) -> Result<Value>;

impl_from_variant!(NativeFunction, Function, Native);

impl fmt::Debug for NativeFunction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("NativeFunction")
    }
}

pub trait Call {
    fn call(&self, args: Vec<Value>, scope: &Scope) -> Result<Value>;
}

impl Call for Function {
    fn call(&self, args: Vec<Value>, scope: &Scope) -> Result<Value> {
        match self {
            Function::User(x) => x.call(args, scope),
            Function::Native(x) => x.call(args, scope),
        }
    }
}

impl Call for UserFunction {
    fn call(&self, args: Vec<Value>, scope: &Scope) -> Result<Value> {
        if self.signature.len() != args.len() {
            return Err(Error::CallError(
                self.ident.clone(),
                Error::ArgumentMismatch(args.len(), self.signature.len()).into(),
            ));
        }
        let mut scope = scope.child();
        for (index, value) in args.into_iter().enumerate() {
            let ident = self.signature.get(index).unwrap();
            scope.set(ident.clone(), value)?;
        }
        self.body.eval(&scope)
    }
}

impl Call for NativeFunction {
    fn call(&self, args: Vec<Value>, scope: &Scope) -> Result<Value> {
        (self.0)(args, scope)
    }
}

impl Call for Value {
    fn call(&self, args: Vec<Value>, scope: &Scope) -> Result<Value> {
        match self {
            Value::Function(func) => func.call(args, scope),
            v => Err(Error::NotFn(v.clone())),
        }
    }
}

impl From<NativeFunctionPt> for Function {
    fn from(f: NativeFunctionPt) -> Self {
        NativeFunction(f).into()
    }
}

impl From<ast::FnDef> for Function {
    fn from(fn_def: ast::FnDef) -> Self {
        UserFunction {
            ident: fn_def.ident,
            signature: fn_def.signature,
            body: fn_def.body,
        }
        .into()
    }
}

impl From<ast::FnExpr> for Function {
    fn from(fn_expr: ast::FnExpr) -> Self {
        UserFunction {
            ident: "_anonymous".into(),
            signature: fn_expr.signature,
            body: *fn_expr.body,
        }
        .into()
    }
}

impl PartialEq for Function {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Function::Native(a), Function::Native(b)) => a.0 as usize == b.0 as usize,
            (Function::Native(_), Function::User(_)) | (Function::User(_), Function::Native(_)) => {
                false
            }
            (Function::User(_), Function::User(_)) => {
                // TODO this requires implementing PartialEq for ast::Expr
                unimplemented!("user defined functions cannot be compared")
            }
        }
    }
}
