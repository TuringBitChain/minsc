use std::borrow::Borrow;
use std::convert::{TryFrom, TryInto};
use std::fmt;

use bitcoin::blockdata::script::Builder as ScriptBuilder;
use bitcoin::hashes::{self, hex::ToHex, Hash};
use bitcoin::{Address, Network, Script};
use miniscript::bitcoin;
use miniscript::descriptor::{self, DescriptorPublicKey};

use crate::ast::{self, Expr, Stmt};
use crate::function::{Call, Function};
use crate::stdlib::miniscript::fns as miniscript_fns;
use crate::time;
use crate::util::{DescriptorExt, EC};
use crate::{Descriptor, Error, Miniscript, Policy, Result, Scope};

/// A runtime value. This is what gets passed around as function arguments, returned from functions,
/// and assigned to variables.
#[derive(Debug, Clone)]
pub enum Value {
    PubKey(DescriptorPublicKey),
    Bytes(Vec<u8>),
    Number(i64),
    Bool(bool),
    DateTime(String),
    Duration(ast::Duration),

    Policy(Policy),
    WithProb(usize, Policy),

    Miniscript(Miniscript),
    Descriptor(Descriptor),
    Script(Script),
    Address(Address),

    Function(Function),
    Array(Array),

    // Exists in the runtime builtins but cannot be constructed
    Network(Network),
}

impl_from_variant!(Policy, Value);
impl_from_variant!(Miniscript, Value);
impl_from_variant!(Descriptor, Value);
impl_from_variant!(DescriptorPublicKey, Value, PubKey);
impl_from_variant!(Script, Value);
impl_from_variant!(Address, Value);
impl_from_variant!(Array, Value);
impl_from_variant!(Vec<u8>, Value, Bytes);
impl_from_variant!(Network, Value);
impl_from_variant!(i64, Value, Number);
impl_from_variant!(bool, Value, Bool);
impl From<usize> for Value {
    fn from(num: usize) -> Self {
        (num as i64).into()
    }
}
impl From<Vec<Value>> for Value {
    fn from(elements: Vec<Value>) -> Self {
        Array(elements).into()
    }
}
impl<T: Into<Function>> From<T> for Value {
    fn from(f: T) -> Self {
        Value::Function(f.into())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Array(pub Vec<Value>);

/// Evaluate an expression. Expressions have no side-effects and return a value.
pub trait Evaluate {
    fn eval(&self, scope: &Scope) -> Result<Value>;
}

/// Execute a statement. Statements have side-effects and don't have a return value.
pub trait Execute {
    fn exec(&self, scope: &mut Scope) -> Result<()>;
}

impl Execute for ast::Assign {
    fn exec(&self, scope: &mut Scope) -> Result<()> {
        for assignment in &self.0 {
            let value = assignment.rhs.eval(scope)?;
            scope.set(assignment.lhs.clone(), value)?;
        }
        Ok(())
    }
}

impl Execute for ast::FnDef {
    fn exec(&self, scope: &mut Scope) -> Result<()> {
        let func = Function::from(self.clone());
        scope.set(self.ident.clone(), func)
    }
}

impl Execute for Stmt {
    fn exec(&self, scope: &mut Scope) -> Result<()> {
        match self {
            Stmt::FnDef(x) => x.exec(scope),
            Stmt::Assign(x) => x.exec(scope),
        }
    }
}

impl Evaluate for ast::Call {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        call_exprs(scope, &self.ident, &self.args)
    }
}

impl Evaluate for ast::Or {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        eval_andor(&self.0, scope, true, "or", 1)
    }
}

impl Evaluate for ast::And {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        eval_andor(&self.0, scope, false, "and", self.0.len())
    }
}

fn eval_andor(
    operands: &[Expr],
    scope: &Scope,
    bool_stop_on: bool,
    desc_op: &str,
    desc_thresh_n: usize,
) -> Result<Value> {
    // Peek at the first operand to determine if its an operation between booleans or between descriptors.
    // All the other operands are expected to have the same type.
    let first_operand = operands[0].eval(scope)?;
    match &first_operand {
        Value::Bool(_) => eval_bool_andor(first_operand, &operands[1..], scope, bool_stop_on),
        Value::Policy(_) | Value::WithProb(_, _) => {
            eval_policy_andor(desc_op, desc_thresh_n, first_operand, &operands[1..], scope)
        }
        _ => Err(Error::InvalidArguments),
    }
}

// Evaluate && / || for booleans (lazily evaluated)
fn eval_bool_andor(
    first_operand: Value,
    other_exprs: &[Expr],
    scope: &Scope,
    stop_on: bool,
) -> Result<Value> {
    if first_operand.into_bool()? == stop_on {
        return Ok(stop_on.into());
    }
    for expr in other_exprs {
        let operand = expr.eval(scope)?;
        if operand.into_bool()? == stop_on {
            return Ok(stop_on.into());
        }
    }
    Ok((!stop_on).into())
}

// Evaluate && / || for combining policies, using the or()/and()/thres() policy functions
fn eval_policy_andor(
    op_name: &str,
    thresh_n: usize,
    first_policy: Value,
    other_policies: &[Expr],
    scope: &Scope,
) -> Result<Value> {
    let policies = [&[first_policy], &eval_exprs(scope, other_policies)?[..]].concat();
    if policies.len() == 2 {
        // delegate to or()/and() when there are exactly 2 subpolicies
        call_args(scope, &op_name.into(), policies)
    } else {
        // delegate to thresh() when there are more
        let mut args = vec![thresh_n.into()];
        args.extend(policies);
        call_args(scope, &"thresh".into(), args)
    }
}

impl Evaluate for ast::Thresh {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        call_exprs(scope, &"thresh".into(), &[&*self.thresh, &*self.policies])
    }
}

impl Evaluate for ast::Ident {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        Ok(match scope.get(&self) {
            Some(binding) => binding.clone(),
            None => bail!(Error::VarNotFound(self.clone())),
        })
    }
}
impl Evaluate for ast::Array {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        let elements = eval_exprs(scope, &self.0)?;
        Ok(Array(elements).into())
    }
}

impl Evaluate for ast::ArrayAccess {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        let value = self.array.eval(scope)?;
        let index = self.index.eval(scope)?.into_usize()?;
        Ok(match value {
            Value::Array(Array(mut elements)) => {
                ensure!(index < elements.len(), Error::ArrayIndexOutOfRange);
                elements.remove(index)
            }
            Value::Bytes(mut bytes) => {
                ensure!(index < bytes.len(), Error::ArrayIndexOutOfRange);
                (bytes.remove(index) as i64).into()
            }
            other => bail!(Error::NotArray(other))
        })
    }
}

impl Evaluate for ast::WithProb {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        call_exprs(scope, &"prob".into(), &[&*self.prob, &*self.expr])
    }
}

impl Evaluate for ast::ChildDerive {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        let parent = self.parent.eval(scope)?;

        // Derive xpubs children
        if let Value::PubKey(key) = parent {
            let mut xpub = match key {
                DescriptorPublicKey::XPub(xpub) => xpub,
                DescriptorPublicKey::SinglePub(_) => bail!(Error::InvalidSingleDerivation),
            };
            for child in &self.path {
                let child = child.eval(scope)?.into_usize()? as u32;
                xpub.derivation_path = xpub.derivation_path.into_child(child.into());
            }
            xpub.wildcard = iif!(
                self.is_wildcard,
                descriptor::Wildcard::Unhardened,
                descriptor::Wildcard::None
            );
            Ok(DescriptorPublicKey::XPub(xpub).into())
        }
        // Derive descriptor children
        // Policies and Miniscripts are implicitly coerced into descriptors
        else {
            ensure!(
                !self.is_wildcard && self.path.len() == 1,
                Error::InvalidDescriptorDerivation
            );
            let desc = parent.into_desc()?;
            let child = self.path[0].eval(scope)?.into_usize()? as u32;
            let desc = desc.derive(child.into());
            Ok(desc.into())
        }
        // TODO support hardened child codes
    }
}

impl Evaluate for ast::FnExpr {
    fn eval(&self, _scope: &Scope) -> Result<Value> {
        Ok(Function::from(self.clone()).into())
    }
}

impl Evaluate for ast::ScriptFrag {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        let scripts = self
            .fragments
            .iter()
            .map(|frag| frag.eval(scope)?.into_script())
            .collect::<Result<Vec<Script>>>()?;
        let bytes = scripts
            .into_iter()
            .map(|script| script.into_bytes())
            .flatten()
            .collect::<Vec<u8>>();
        Ok(Script::from(bytes).into())
    }
}

impl Evaluate for ast::Not {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        Ok((!self.0.eval(scope)?.into_bool()?).into())
    }
}

impl Evaluate for ast::Infix {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        self.op
            .apply(self.lhs.eval(scope)?, self.rhs.eval(scope)?)
            .map_err(|e| Error::OpError(self.op, e.into()))
    }
}

impl ast::InfixOp {
    fn apply(&self, lhs: Value, rhs: Value) -> Result<Value> {
        use ast::InfixOp::*;
        use Value::*;

        Ok(match (self, lhs, rhs) {
            // == != for all types
            (Eq, a, b) => (a == b).into(),
            (NotEq, a, b) => (a != b).into(),
            // < > <= >= for numbers only
            (Gt, Number(a), Number(b)) => (a > b).into(),
            (Lt, Number(a), Number(b)) => (a < b).into(),
            (Gte, Number(a), Number(b)) => (a >= b).into(),
            (Lte, Number(a), Number(b)) => (a <= b).into(),
            // + - for numbers
            (Add, Number(a), Number(b)) => a.checked_add(b).ok_or(Error::Overflow)?.into(),
            (Subtract, Number(a), Number(b)) => a.checked_sub(b).ok_or(Error::Overflow)?.into(),
            // + for arrays
            (Add, Array(a), Array(b)) => [a.0, b.0].concat().into(),
            // + for bytes
            (Add, Bytes(a), Bytes(b)) => [a, b].concat().into(),
            _ => bail!(Error::InvalidArguments),
        })
    }
}

impl Evaluate for ast::Block {
    // Execute the block in a new child scope, with no visible side-effects.
    fn eval(&self, scope: &Scope) -> Result<Value> {
        let mut scope = scope.child();
        for stmt in &self.stmts {
            stmt.exec(&mut scope)?;
        }
        if let Some(return_value) = &self.return_value {
            // The return value is the final expression within the function body,
            // optionally prefixed with the `return` keyword
            return_value.eval(&scope)
        } else if let Some(Value::Function(func)) = scope.get(&"main".into()) {
            // The return value is the evaluation of main()
            func.call(vec![], &scope)
        } else {
            Err(Error::NoReturnValue)
        }
    }
}

impl Execute for ast::Library {
    // Execute the library in the given scope, producing visible side-effects
    fn exec(&self, scope: &mut Scope) -> Result<()> {
        for stmt in &self.stmts {
            stmt.exec(scope)?;
        }
        Ok(())
    }
}

impl Evaluate for Expr {
    fn eval(&self, scope: &Scope) -> Result<Value> {
        Ok(match self {
            Expr::Ident(x) => x.eval(scope)?,
            Expr::Call(x) => x.eval(scope)?,
            Expr::Or(x) => x.eval(scope)?,
            Expr::And(x) => x.eval(scope)?,
            Expr::Thresh(x) => x.eval(scope)?,
            Expr::Block(x) => x.eval(scope)?,
            Expr::WithProb(x) => x.eval(scope)?,
            Expr::Array(x) => x.eval(scope)?,
            Expr::ArrayAccess(x) => x.eval(scope)?,
            Expr::ChildDerive(x) => x.eval(scope)?,
            Expr::ScriptFrag(x) => x.eval(scope)?,
            Expr::FnExpr(x) => x.eval(scope)?,
            Expr::Infix(x) => x.eval(scope)?,
            Expr::Not(x) => x.eval(scope)?,

            // Atoms
            Expr::PubKey(x) => Value::PubKey(x.parse()?),
            Expr::Bytes(x) => Value::Bytes(x.clone()),
            Expr::Number(x) => Value::Number(*x),
            Expr::Duration(x) => Value::Duration(x.clone()),
            Expr::DateTime(x) => Value::DateTime(x.clone()),
        })
    }
}

/// Call the function with the given expressions evaluated into values
fn call_exprs<T: Borrow<Expr>>(scope: &Scope, ident: &ast::Ident, exprs: &[T]) -> Result<Value> {
    call_args(scope, ident, eval_exprs(scope, exprs)?)
}

/// Call the function with the given argument values (already evaluated)
fn call_args(scope: &Scope, ident: &ast::Ident, args: Vec<Value>) -> Result<Value> {
    let func = scope
        .get(ident)
        .ok_or_else(|| Error::FnNotFound(ident.clone()))?;

    func.call(args, scope)
        .map_err(|e| Error::CallError(ident.clone(), e.into()))
}

/// Evaluate a list of expressions to produce a list of values
fn eval_exprs<T: Borrow<Expr>>(scope: &Scope, exprs: &[T]) -> Result<Vec<Value>> {
    exprs.iter().map(|arg| arg.borrow().eval(scope)).collect()
}

impl TryFrom<Value> for Policy {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::Policy(policy) => Ok(policy),
            arr @ Value::Array(Array(_)) => miniscript_fns::all_(arr)?.try_into(),
            v => Err(Error::NotPolicyLike(v)),
        }
    }
}

impl TryFrom<Value> for i64 {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::Number(n) => Ok(n),
            v => Err(Error::NotNumber(v)),
        }
    }
}

impl TryFrom<Value> for usize {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        Ok(value.into_i64()?.try_into()?)
    }
}

impl TryFrom<Value> for bool {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::Bool(b) => Ok(b),
            v => Err(Error::NotBool(v)),
        }
    }
}

impl TryFrom<Value> for DescriptorPublicKey {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::PubKey(x) => Ok(x),
            Value::Bytes(x) => {
                let pubkey = bitcoin::PublicKey::from_slice(&x)?;
                Ok(DescriptorPublicKey::SinglePub(
                    descriptor::DescriptorSinglePub {
                        key: descriptor::SinglePubKey::FullKey(pubkey),
                        origin: None,
                    },
                ))
            }
            v => Err(Error::NotPubKey(v)),
        }
    }
}
impl TryFrom<Value> for Descriptor {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::Descriptor(x) => Ok(x),
            Value::Miniscript(x) => Ok(Descriptor::new_wsh(x)?),
            Value::Policy(x) => Ok(Descriptor::new_wsh(x.compile()?)?),
            Value::PubKey(x) => Ok(Descriptor::new_wpkh(x)?),
            v => Err(Error::NotDescriptorLike(v)),
        }
    }
}
impl TryFrom<Value> for Miniscript {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::Miniscript(x) => Ok(x),
            Value::Policy(x) => Ok(x.compile()?),
            v => Err(Error::NotMiniscriptLike(v)),
        }
    }
}

impl TryFrom<Value> for Vec<u8> {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::Bytes(bytes) => Ok(bytes),
            v => Err(Error::NotBytes(v)),
        }
    }
}

impl TryFrom<Value> for Array {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::Array(array) => Ok(array),
            v => Err(Error::NotArray(v)),
        }
    }
}

impl TryFrom<Value> for Function {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::Function(f) => Ok(f),
            v => Err(Error::NotFn(v)),
        }
    }
}

impl TryFrom<Value> for Network {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::Network(array) => Ok(array),
            v => Err(Error::NotNetwork(v)),
        }
    }
}

impl TryFrom<Value> for Script {
    type Error = Error;
    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::Script(x) => Ok(x),
            v @ Value::Descriptor(_) | v @ Value::Miniscript(_) | v @ Value::Policy(_) => {
                Ok(v.into_desc()?.to_explicit_script()?)
            }
            Value::Number(n) => Ok(ScriptBuilder::new().push_int(n).into_script()),
            Value::Bool(val) => Ok(ScriptBuilder::new().push_int(val as i64).into_script()),
            Value::Bytes(bytes) => Ok(ScriptBuilder::new().push_slice(&bytes).into_script()),
            Value::PubKey(desc_pubkey) => {
                let pubkey = desc_pubkey.derive_public_key(&EC)?;
                Ok(ScriptBuilder::new().push_key(&pubkey).into_script())
            }
            Value::Duration(dur) => {
                let seq_num = time::duration_to_seq(&dur)?;
                Ok(ScriptBuilder::new().push_int(seq_num as i64).into_script())
            }
            Value::DateTime(datetime) => {
                let unix_timestamp = time::parse_datetime(&datetime)?;
                Ok(ScriptBuilder::new()
                    .push_int(unix_timestamp as i64)
                    .into_script())
            }
            Value::Array(Array(elements)) => {
                let scriptbytes = elements
                    .into_iter()
                    .map(|val| Ok(val.into_script()?.into_bytes()))
                    .collect::<Result<Vec<Vec<u8>>>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<u8>>();
                Ok(scriptbytes.into())
            }
            v => Err(Error::NotScriptLike(v)),
        }
    }
}

macro_rules! impl_hash_conv {
    ($name:path) => {
        impl TryFrom<Value> for $name {
            type Error = Error;
            fn try_from(value: Value) -> Result<Self> {
                match value {
                    Value::Bytes(b) => Ok(Self::from_slice(&b)?),
                    v => Err(Error::NotHashLike(v)),
                }
            }
        }
    };
}

impl_hash_conv!(hashes::sha256::Hash);
impl_hash_conv!(hashes::sha256d::Hash);
impl_hash_conv!(hashes::ripemd160::Hash);
impl_hash_conv!(hashes::hash160::Hash);

impl Value {
    pub fn array(elements: Vec<Value>) -> Value {
        Array(elements).into()
    }
    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }
    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Bool(_))
    }
    pub fn into_policy(self) -> Result<Policy> {
        self.try_into()
    }
    pub fn into_i64(self) -> Result<i64> {
        self.try_into()
    }
    pub fn into_usize(self) -> Result<usize> {
        self.try_into()
    }
    pub fn into_bool(self) -> Result<bool> {
        self.try_into()
    }
    pub fn into_key(self) -> Result<DescriptorPublicKey> {
        self.try_into()
    }
    pub fn into_bytes(self) -> Result<Vec<u8>> {
        self.try_into()
    }
    pub fn into_miniscript(self) -> Result<Miniscript> {
        self.try_into()
    }
    pub fn into_desc(self) -> Result<Descriptor> {
        self.try_into()
    }
    pub fn into_script(self) -> Result<Script> {
        self.try_into()
    }
    pub fn into_fn(self) -> Result<Function> {
        self.try_into()
    }
    pub fn into_array_elements(self) -> Result<Vec<Value>> {
        Ok(Array::try_from(self)?.0)
    }
    pub fn into_script_pubkey(self) -> Result<Script> {
        Ok(self.into_desc()?.to_script_pubkey()?)
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::Network(a), Value::Network(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::Script(a), Value::Script(b)) => a == b,
            (Value::DateTime(a), Value::DateTime(b)) => a == b,
            (Value::Duration(a), Value::Duration(b)) => a == b,
            (Value::Address(a), Value::Address(b)) => a == b,
            (Value::Policy(a), Value::Policy(b)) => a == b,
            (Value::Miniscript(a), Value::Miniscript(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            (Value::Descriptor(a), Value::Descriptor(b)) => a == b,
            (Value::PubKey(a), Value::PubKey(b)) => a == b,
            (Value::WithProb(a_p, a_d), Value::WithProb(b_p, b_d)) => a_p == b_p && a_d == b_d,
            (Value::Function(_), Value::Function(_)) => {
                unimplemented!("functions cannot be compared")
            }
            // comparsion with a different type always returns false (no coercion)
            (_, _) => false,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Value::PubKey(x) => write!(f, "{}", x),
            Value::Number(x) => write!(f, "{}", x),
            Value::Bool(x) => write!(f, "{}", x),
            Value::DateTime(x) => write!(f, "{}", x),
            Value::Duration(x) => write!(f, "{:?}", x),
            Value::Bytes(x) => write!(f, "0x{}", x.to_hex()),
            Value::Policy(x) => write!(f, "{}", x),
            Value::WithProb(p, x) => write!(f, "{}@{}", p, x),
            Value::Miniscript(x) => write!(f, "{}", x),
            Value::Descriptor(x) => write!(f, "{}", x),
            Value::Address(x) => write!(f, "{}", x),
            Value::Script(x) => write!(f, "{}", x.to_hex()),
            Value::Function(x) => write!(f, "{:?}", x),
            Value::Network(x) => write!(f, "{}", x),
            Value::Array(Array(elements)) => {
                write!(f, "[")?;
                for (i, element) in elements.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "\n  {}", element)?;
                }
                write!(f, "\n]")
            }
        }
    }
}
