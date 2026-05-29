//! Tree-walking interpreter for Bock AIR expressions.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use async_recursion::async_recursion;
use futures::future::BoxFuture;

use bock_air::{
    AIRNode, AirArg, AirInterpolationPart, AirRecordField, EnumVariantPayload, NodeKind,
    ResultVariant,
};
use bock_ast::{AssignOp, BinOp, Literal, TypePath, UnaryOp};

use crate::builtins::{BuiltinRegistry, CallbackInvoker, TypeTag};
use crate::env::{EffectStack, Environment};
use crate::error::RuntimeError;
use crate::value::{BockString, EnumValue, FnValue, IteratorNext, OrdF64, RecordValue, Value};

// ─── Closure ──────────────────────────────────────────────────────────────────

/// A reference-counted native constructor function.
type NativeFn = std::sync::Arc<dyn Fn(&[Value]) -> Value + Send + Sync>;

/// The body of a closure — either an AIR node, a composition, or a native Rust function.
#[derive(Clone)]
enum ClosureBody {
    /// A regular lambda / function body.
    Air(Box<AIRNode>),
    /// A composed function: apply `inner` first, pass result to `outer`.
    Composed { inner: u64, outer: u64 },
    /// A native constructor function implemented in Rust.
    Native(NativeFn),
}

impl std::fmt::Debug for ClosureBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClosureBody::Air(node) => f.debug_tuple("Air").field(node).finish(),
            ClosureBody::Composed { inner, outer } => f
                .debug_struct("Composed")
                .field("inner", inner)
                .field("outer", outer)
                .finish(),
            ClosureBody::Native(_) => f.debug_tuple("Native").field(&"<fn>").finish(),
        }
    }
}

/// A closure: parameter names, body, and the environment captured at definition.
#[derive(Debug, Clone)]
struct Closure {
    params: Vec<String>,
    body: ClosureBody,
    captured: Environment,
    /// True for named top-level functions. When called, these use the current
    /// interpreter environment (which contains all registered globals) instead
    /// of the captured snapshot. This enables recursion, mutual recursion, and
    /// forward references between top-level functions.
    is_toplevel: bool,
    /// True for `async fn` declarations. When true, calling the closure
    /// spawns the body as a tokio task and returns `Value::Future` immediately.
    is_async: bool,
}

// ─── Interpreter ──────────────────────────────────────────────────────────────

/// Tree-walking interpreter for Bock AIR.
///
/// Evaluates expressions against a typed AIR tree. The `Environment` manages
/// lexical variable bindings; the `EffectStack` will be used by P5.5 for
/// algebraic effect dispatch.
#[derive(Clone)]
pub struct Interpreter {
    /// Current variable bindings (nested scopes).
    pub env: Environment,
    /// Algebraic effect handler stack (populated by P5.5).
    pub effect_handlers: EffectStack,
    /// Maps `FnValue::id` to the corresponding closure implementation.
    fn_registry: HashMap<u64, Closure>,
    /// Built-in method and global function dispatch table.
    pub builtins: BuiltinRegistry,
    /// User-defined impl methods: type_name → method_name → (param_names, body).
    method_table: HashMap<String, HashMap<String, (Vec<String>, AIRNode)>>,
    /// Maps effect operation names to their parent effect name.
    /// Used to dispatch `log(msg)` → look up handler for `Logger` → call it.
    effect_operations: HashMap<String, String>,
}

impl CallbackInvoker for Interpreter {
    fn invoke<'a>(
        &'a mut self,
        callable: &'a Value,
        args: &'a [Value],
    ) -> BoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async move { self.invoke_callback(callable, args).await })
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    /// Create a new interpreter with an empty global environment
    /// and default built-in functions registered.
    #[must_use]
    pub fn new() -> Self {
        let mut builtins = BuiltinRegistry::new();
        builtins.register_defaults();
        let mut interp = Self {
            env: Environment::new(),
            effect_handlers: EffectStack::new(),
            fn_registry: HashMap::new(),
            builtins,
            method_table: HashMap::new(),
            effect_operations: HashMap::new(),
        };
        interp.register_prelude_constructors();
        interp
    }

    /// Register the built-in `Ok`, `Err`, `Some`, and `None` constructors
    /// in the global environment so they are available at runtime.
    fn register_prelude_constructors(&mut self) {
        // None is a plain value, not a function.
        self.env.define("None", Value::Optional(None));

        // Some(x) → Value::Optional(Some(x))
        self.register_native_constructor("Some", 1, |args| {
            Value::Optional(Some(Box::new(args[0].clone())))
        });

        // Ok(x) → Value::Result(Ok(x))
        self.register_native_constructor("Ok", 1, |args| {
            Value::Result(Ok(Box::new(args[0].clone())))
        });

        // Err(x) → Value::Result(Err(x))
        self.register_native_constructor("Err", 1, |args| {
            Value::Result(Err(Box::new(args[0].clone())))
        });
    }

    /// Register a native constructor function (not backed by an AIR body).
    ///
    /// The `build` closure receives the validated arguments and returns the
    /// constructed `Value`.
    fn register_native_constructor(
        &mut self,
        name: &str,
        arity: usize,
        build: impl Fn(&[Value]) -> Value + Send + Sync + 'static,
    ) {
        let fn_val = FnValue::new_named(name);
        let id = fn_val.id;
        let params: Vec<String> = (0..arity).map(|i| format!("__arg{i}")).collect();
        self.fn_registry.insert(
            id,
            Closure {
                params,
                body: ClosureBody::Native(std::sync::Arc::new(build)),
                captured: Environment::new(),
                is_toplevel: true,
                is_async: false,
            },
        );
        self.env.define(name, Value::Function(fn_val));
    }

    /// Register all variants of an enum declaration in the environment.
    ///
    /// - Unit variants become `Value::Enum` values.
    /// - Tuple variants become constructor functions wrapping args in `Value::Enum`.
    /// - Record (struct) variants become constructor functions producing `Value::Record`.
    pub fn register_enum(&mut self, enum_name: &str, variants: &[AIRNode]) {
        let type_name = enum_name.to_string();
        for variant in variants {
            if let NodeKind::EnumVariant { name, payload } = &variant.kind {
                let variant_name = name.name.clone();
                match payload {
                    EnumVariantPayload::Unit => {
                        self.env.define(
                            variant_name.clone(),
                            Value::Enum(EnumValue {
                                type_name: type_name.clone(),
                                variant: variant_name,
                                payload: None,
                            }),
                        );
                    }
                    EnumVariantPayload::Tuple(fields) => {
                        let arity = fields.len();
                        let tn = type_name.clone();
                        let vn = variant_name.clone();
                        if arity == 1 {
                            self.register_native_constructor(&variant_name, arity, move |args| {
                                Value::Enum(EnumValue {
                                    type_name: tn.clone(),
                                    variant: vn.clone(),
                                    payload: Some(Box::new(args[0].clone())),
                                })
                            });
                        } else {
                            self.register_native_constructor(&variant_name, arity, move |args| {
                                Value::Enum(EnumValue {
                                    type_name: tn.clone(),
                                    variant: vn.clone(),
                                    payload: Some(Box::new(Value::Tuple(args.to_vec()))),
                                })
                            });
                        }
                    }
                    EnumVariantPayload::Struct(fields) => {
                        let arity = fields.len();
                        let vn = variant_name.clone();
                        let field_names: Vec<String> =
                            fields.iter().map(|f| f.name.name.clone()).collect();
                        self.register_native_constructor(&variant_name, arity, move |args| {
                            let mut field_map = std::collections::BTreeMap::new();
                            for (fname, val) in field_names.iter().zip(args.iter()) {
                                field_map.insert(fname.clone(), val.clone());
                            }
                            Value::Record(RecordValue {
                                type_name: vn.clone(),
                                fields: field_map,
                            })
                        });
                    }
                }
            }
        }
    }

    /// Register a named function in the global environment.
    ///
    /// The function is stored in the registry and bound by name so that
    /// `Identifier` nodes referencing it will resolve to `Value::Function`.
    pub fn register_fn(&mut self, name: &str, params: Vec<String>, body: AIRNode) {
        self.register_fn_with_async(name, params, body, false);
    }

    /// Register a named function whose body may be `async`.
    ///
    /// When `is_async` is true, calling the function spawns a tokio task and
    /// returns a `Value::Future`; when false, it runs inline like a regular
    /// function call.
    pub fn register_fn_with_async(
        &mut self,
        name: &str,
        params: Vec<String>,
        body: AIRNode,
        is_async: bool,
    ) {
        let fn_val = FnValue::new_named(name);
        let id = fn_val.id;
        self.env.define(name, Value::Function(fn_val));
        self.fn_registry.insert(
            id,
            Closure {
                params,
                body: ClosureBody::Air(Box::new(body)),
                captured: Environment::new(),
                is_toplevel: true,
                is_async,
            },
        );
    }

    /// Register methods from an `impl` block in the method table.
    ///
    /// Extracts the target type name and each method's parameter names + body,
    /// storing them in `method_table[type_name][method_name]`.
    pub fn register_impl(&mut self, target: &AIRNode, methods: &[AIRNode]) {
        // Extract the type name from the target node (TypeNamed path).
        let type_name = match &target.kind {
            NodeKind::TypeNamed { path, .. } => path
                .segments
                .last()
                .map(|s| s.name.clone())
                .unwrap_or_default(),
            _ => return,
        };

        let type_methods = self.method_table.entry(type_name).or_default();
        for method in methods {
            if let NodeKind::FnDecl {
                name, params, body, ..
            } = &method.kind
            {
                let param_names: Vec<String> = params
                    .iter()
                    .filter_map(|p| {
                        if let NodeKind::Param { pattern, .. } = &p.kind {
                            if let NodeKind::BindPat { name, .. } = &pattern.kind {
                                return Some(name.name.clone());
                            }
                        }
                        None
                    })
                    .collect();
                type_methods.insert(name.name.clone(), (param_names, *body.clone()));
            }
        }
    }

    /// Register an effect declaration's operations so they can be dispatched
    /// at runtime through the effect handler stack.
    ///
    /// For each operation in the effect, records a mapping from the operation
    /// name to the effect name. When a call like `log(msg)` is evaluated,
    /// the interpreter checks this map, finds the effect (`Logger`), resolves
    /// the handler, and dispatches the call.
    pub fn register_effect(&mut self, effect_name: &str, operations: &[AIRNode]) {
        // Also define the effect name in the environment (type-level marker).
        self.env.define(effect_name, Value::Void);

        for op in operations {
            if let NodeKind::FnDecl { name, .. } = &op.kind {
                self.effect_operations
                    .insert(name.name.clone(), effect_name.to_string());
            }
        }
    }

    /// Invoke a callable (function value) using the interpreter's closure machinery.
    ///
    /// This is the public entry point for callback invocation from builtins.
    #[async_recursion]
    pub async fn invoke_callback(
        &mut self,
        callable: &Value,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        let fn_id = match callable {
            Value::Function(fv) => fv.id,
            other => {
                return Err(RuntimeError::NotCallable {
                    value: other.to_string(),
                })
            }
        };
        let closure =
            self.fn_registry
                .get(&fn_id)
                .cloned()
                .ok_or_else(|| RuntimeError::NotCallable {
                    value: format!("unregistered fn #{fn_id}"),
                })?;
        self.call_closure(&closure, args.to_vec()).await
    }

    // ── Main evaluation entry point ────────────────────────────────────────

    /// Evaluate a single AIR expression node and return its runtime value.
    #[async_recursion]
    pub async fn eval_expr(&mut self, node: &AIRNode) -> Result<Value, RuntimeError> {
        match &node.kind {
            NodeKind::Literal { lit } => self.eval_literal(lit),

            NodeKind::Identifier { name } => {
                self.env
                    .get(&name.name)
                    .cloned()
                    .ok_or_else(|| RuntimeError::UndefinedVariable {
                        name: name.name.clone(),
                    })
            }

            NodeKind::BinaryOp { op, left, right } => self.eval_binary_op(*op, left, right).await,

            NodeKind::UnaryOp { op, operand } => self.eval_unary_op(*op, operand).await,

            NodeKind::Assign { op, target, value } => self.eval_assign(*op, target, value).await,

            NodeKind::Call { callee, args, .. } => self.eval_call(callee, args).await,

            NodeKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                self.eval_method_call(receiver, &method.name.clone(), args)
                    .await
            }

            NodeKind::FieldAccess { object, field } => {
                self.eval_field_access(object, &field.name.clone()).await
            }

            NodeKind::Index { object, index } => self.eval_index(object, index).await,

            NodeKind::Propagate { expr } => self.eval_propagate(expr).await,

            NodeKind::Lambda { params, body } => self.eval_lambda(params, body),

            NodeKind::Pipe { left, right } => self.eval_pipe(left, right).await,

            NodeKind::Compose { left, right } => self.eval_compose(left, right).await,

            // ── Collection literals ────────────────────────────────────────
            NodeKind::ListLiteral { elems } => {
                let mut values = Vec::with_capacity(elems.len());
                for elem in elems {
                    values.push(self.eval_expr(elem).await?);
                }
                Ok(Value::List(values))
            }

            NodeKind::MapLiteral { entries } => {
                let mut map = BTreeMap::new();
                for entry in entries {
                    let k = self.eval_expr(&entry.key).await?;
                    let v = self.eval_expr(&entry.value).await?;
                    map.insert(k, v);
                }
                Ok(Value::Map(map))
            }

            NodeKind::SetLiteral { elems } => {
                let mut set = BTreeSet::new();
                for elem in elems {
                    set.insert(self.eval_expr(elem).await?);
                }
                Ok(Value::Set(set))
            }

            NodeKind::TupleLiteral { elems } => {
                let mut values = Vec::with_capacity(elems.len());
                for elem in elems {
                    values.push(self.eval_expr(elem).await?);
                }
                Ok(Value::Tuple(values))
            }

            NodeKind::RecordConstruct {
                path,
                fields,
                spread,
            } => {
                self.eval_record_construct(path, fields, spread.as_deref())
                    .await
            }

            NodeKind::Interpolation { parts } => self.eval_interpolation(parts).await,

            NodeKind::Range { lo, hi, inclusive } => self.eval_range(lo, hi, *inclusive).await,

            NodeKind::ResultConstruct { variant, value } => {
                let inner = match value {
                    Some(v) => self.eval_expr(v).await?,
                    None => Value::Void,
                };
                match variant {
                    ResultVariant::Ok => Ok(Value::Result(Ok(Box::new(inner)))),
                    ResultVariant::Err => Ok(Value::Result(Err(Box::new(inner)))),
                }
            }

            // ── Control flow ───────────────────────────────────────────────
            NodeKind::Block { stmts, tail } => self.eval_block(stmts, tail.as_deref()).await,

            NodeKind::If {
                let_pattern,
                condition,
                then_block,
                else_block,
            } => {
                self.eval_if(
                    let_pattern.as_deref(),
                    condition,
                    then_block,
                    else_block.as_deref(),
                )
                .await
            }

            NodeKind::Match { scrutinee, arms } => self.eval_match(scrutinee, arms).await,

            NodeKind::Return { value } => {
                let v = match value {
                    Some(e) => self.eval_expr(e).await?,
                    None => Value::Void,
                };
                Err(RuntimeError::Return(Box::new(v)))
            }

            NodeKind::Break { value } => {
                let v = match value {
                    Some(e) => Some(Box::new(self.eval_expr(e).await?)),
                    None => None,
                };
                Err(RuntimeError::Break(v))
            }

            NodeKind::Continue => Err(RuntimeError::Continue),

            NodeKind::For {
                pattern,
                iterable,
                body,
            } => self.eval_for(pattern, iterable, body).await,

            NodeKind::While { condition, body } => self.eval_while(condition, body).await,

            NodeKind::Loop { body } => self.eval_loop(body).await,

            NodeKind::Guard {
                let_pattern,
                condition,
                else_block,
            } => {
                self.eval_guard(let_pattern.as_deref(), condition, else_block)
                    .await
            }

            NodeKind::Unreachable => Err(RuntimeError::Unreachable),

            // ── Ownership annotations (pass-through) ───────────────────────
            NodeKind::Move { expr }
            | NodeKind::Borrow { expr }
            | NodeKind::MutableBorrow { expr } => self.eval_expr(expr).await,

            // ── Async — `await` resolves a `Value::Future` to its inner value.
            NodeKind::Await { expr } => {
                let val = self.eval_expr(expr).await?;
                match val {
                    Value::Future(handle) => {
                        let h = handle.lock().unwrap().take();
                        match h {
                            Some(jh) => match jh.await {
                                Ok(inner) => inner,
                                Err(e) => Err(RuntimeError::TypeError(format!(
                                    "async task panicked: {e}"
                                ))),
                            },
                            None => Err(RuntimeError::TypeError(
                                "future already awaited".to_string(),
                            )),
                        }
                    }
                    other => Ok(other),
                }
            }

            // ── Let binding (also appears as a statement inside blocks) ─────
            NodeKind::LetBinding { pattern, value, .. } => {
                let v = self.eval_expr(value).await?;
                self.bind_pattern(pattern, v).await?;
                Ok(Value::Void)
            }

            // ── Placeholder `_` ────────────────────────────────────────────
            NodeKind::Placeholder => Ok(Value::Void),

            // ── Effects ──────────────────────────────────────────────────────

            // Effect declaration: register effect name, evaluate to Void.
            NodeKind::EffectDecl { name, .. } => {
                // Effect declarations are type-level; at runtime we just
                // record the name so it can be referenced.
                self.env.define(&name.name, Value::Void);
                Ok(Value::Void)
            }

            // Module-level `handle Effect with handler`
            NodeKind::ModuleHandle { effect, handler } => {
                let handler_val = self.eval_expr(handler).await?;
                let effect_name = self.type_path_to_name(effect);
                self.effect_handlers
                    .set_module_handler(effect_name, handler_val);
                Ok(Value::Void)
            }

            // Effect operation invocation: resolve handler, call it.
            NodeKind::EffectOp {
                effect,
                operation,
                args,
            } => {
                let effect_name = self.type_path_to_name(effect);
                let handler = self.effect_handlers.resolve(&effect_name).cloned().ok_or(
                    RuntimeError::NoEffectHandler {
                        effect: effect_name,
                    },
                )?;

                // Evaluate arguments
                let mut arg_values = Vec::with_capacity(args.len());
                for arg in args {
                    arg_values.push(self.eval_expr(&arg.value).await?);
                }

                // Call handler.operation(args...)
                // The handler is a record value whose fields are the operation
                // implementations, or a function if the effect has a single op.
                self.dispatch_effect_op(&handler, &operation.name, arg_values)
                    .await
            }

            // Handling block: push handlers, execute body, pop handlers.
            NodeKind::HandlingBlock { handlers, body } => {
                let mut frame = std::collections::HashMap::new();
                for pair in handlers {
                    let handler_val = self.eval_expr(&pair.handler).await?;
                    let effect_name = self.type_path_to_name(&pair.effect);
                    frame.insert(effect_name, handler_val);
                }
                self.effect_handlers.push_handlers(frame);
                let result = self.eval_expr(body).await;
                self.effect_handlers.pop_handlers();
                result
            }

            // Effect reference in type position — no runtime behavior.
            NodeKind::EffectRef { .. } => Ok(Value::Void),

            other => Err(RuntimeError::NotImplemented(
                format!("{other:?}").chars().take(60).collect(),
            )),
        }
    }

    // ── Effect helpers ─────────────────────────────────────────────────────

    /// Convert a `TypePath` to a dot-separated effect name string.
    fn type_path_to_name(&self, tp: &TypePath) -> String {
        tp.segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }

    /// Dispatch an effect operation call to a handler value.
    ///
    /// Resolution order for a record handler:
    ///   1. A field on the record whose value is a function — call it
    ///      with the operation arguments.
    ///   2. An `impl EffectTrait for TypeName` method in the interpreter's
    ///      method table — dispatched as a regular instance method.
    ///
    /// If the handler is a plain function, it's called directly (for
    /// single-operation effects).
    #[async_recursion]
    async fn dispatch_effect_op(
        &mut self,
        handler: &Value,
        operation: &str,
        args: Vec<Value>,
    ) -> Result<Value, RuntimeError> {
        match handler {
            // Record handler: look up operation as a field first, then
            // fall back to impl-block methods.
            Value::Record(rec) => {
                if let Some(op_fn) = rec.fields.get(operation).cloned() {
                    return self.call_fn_value(&op_fn, args).await;
                }
                if let Some(result) = self.try_call_impl_method(handler, operation, args).await? {
                    return Ok(result);
                }
                Err(RuntimeError::FieldNotFound {
                    field: operation.to_string(),
                    type_name: rec.type_name.clone(),
                })
            }
            // Function handler: single-operation effect, call directly
            Value::Function(_) => {
                let handler = handler.clone();
                self.call_fn_value(&handler, args).await
            }
            other => Err(RuntimeError::TypeError(format!(
                "effect handler must be a record or function, got {other}"
            ))),
        }
    }

    /// Call a `Value::Function` by its identity, looking up the closure in
    /// the function registry.
    /// Call a function value with the given arguments.
    #[async_recursion]
    pub async fn call_fn_value(
        &mut self,
        val: &Value,
        args: Vec<Value>,
    ) -> Result<Value, RuntimeError> {
        let fn_id = match val {
            Value::Function(fv) => fv.id,
            other => {
                return Err(RuntimeError::NotCallable {
                    value: other.to_string(),
                })
            }
        };
        let closure =
            self.fn_registry
                .get(&fn_id)
                .cloned()
                .ok_or_else(|| RuntimeError::NotCallable {
                    value: format!("unregistered fn #{fn_id}"),
                })?;
        self.call_closure(&closure, args).await
    }

    // ── Literal evaluation ─────────────────────────────────────────────────

    fn eval_literal(&self, lit: &Literal) -> Result<Value, RuntimeError> {
        match lit {
            Literal::Int(s) => {
                // Strip type suffix (e.g., _u8) before parsing.
                let (numeric, _) = bock_ast::strip_type_suffix(s);
                // Support 0x, 0o, 0b prefixes and optional _ separators.
                let clean = numeric.replace('_', "");
                let n = if clean.starts_with("0x") || clean.starts_with("0X") {
                    i64::from_str_radix(&clean[2..], 16)
                } else if clean.starts_with("0o") || clean.starts_with("0O") {
                    i64::from_str_radix(&clean[2..], 8)
                } else if clean.starts_with("0b") || clean.starts_with("0B") {
                    i64::from_str_radix(&clean[2..], 2)
                } else {
                    clean.parse::<i64>()
                };
                n.map(Value::Int)
                    .map_err(|_| RuntimeError::IntParseFailed(s.clone()))
            }
            Literal::Float(s) => {
                // Strip type suffix (e.g., _f32) before parsing.
                let (numeric, _) = bock_ast::strip_type_suffix(s);
                numeric
                    .replace('_', "")
                    .parse::<f64>()
                    .map(|f| Value::Float(OrdF64(f)))
                    .map_err(|_| RuntimeError::FloatParseFailed(s.clone()))
            }
            Literal::Bool(b) => Ok(Value::Bool(*b)),
            Literal::Char(s) => Ok(Value::Char(s.chars().next().unwrap_or('\0'))),
            Literal::String(s) => Ok(Value::String(BockString::new(s.clone()))),
            Literal::Unit => Ok(Value::Void),
        }
    }

    // ── Binary operator evaluation ─────────────────────────────────────────

    #[async_recursion]
    async fn eval_binary_op(
        &mut self,
        op: BinOp,
        left: &AIRNode,
        right: &AIRNode,
    ) -> Result<Value, RuntimeError> {
        // Short-circuit logical operators.
        match op {
            BinOp::And => {
                let l = self.eval_expr(left).await?;
                return match l {
                    Value::Bool(false) => Ok(Value::Bool(false)),
                    Value::Bool(true) => self.eval_expr(right).await,
                    other => Err(RuntimeError::TypeError(format!(
                        "expected Bool in &&, got {other}"
                    ))),
                };
            }
            BinOp::Or => {
                let l = self.eval_expr(left).await?;
                return match l {
                    Value::Bool(true) => Ok(Value::Bool(true)),
                    Value::Bool(false) => self.eval_expr(right).await,
                    other => Err(RuntimeError::TypeError(format!(
                        "expected Bool in ||, got {other}"
                    ))),
                };
            }
            _ => {}
        }

        let l = self.eval_expr(left).await?;
        let r = self.eval_expr(right).await?;

        match (op, l, r) {
            // ── Arithmetic ────────────────────────────────────────────────
            (BinOp::Add, Value::Int(a), Value::Int(b)) => a
                .checked_add(b)
                .map(Value::Int)
                .ok_or(RuntimeError::IntOverflow),
            (BinOp::Add, Value::Float(a), Value::Float(b)) => Ok(Value::Float(OrdF64(a.0 + b.0))),
            (BinOp::Add, Value::String(a), Value::String(b)) => {
                Ok(Value::String(BockString::new(format!("{a}{b}"))))
            }

            (BinOp::Sub, Value::Int(a), Value::Int(b)) => a
                .checked_sub(b)
                .map(Value::Int)
                .ok_or(RuntimeError::IntOverflow),
            (BinOp::Sub, Value::Float(a), Value::Float(b)) => Ok(Value::Float(OrdF64(a.0 - b.0))),

            (BinOp::Mul, Value::Int(a), Value::Int(b)) => a
                .checked_mul(b)
                .map(Value::Int)
                .ok_or(RuntimeError::IntOverflow),
            (BinOp::Mul, Value::Float(a), Value::Float(b)) => Ok(Value::Float(OrdF64(a.0 * b.0))),

            (BinOp::Div, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    Err(RuntimeError::DivisionByZero)
                } else {
                    Ok(Value::Int(a / b))
                }
            }
            (BinOp::Div, Value::Float(a), Value::Float(b)) => Ok(Value::Float(OrdF64(a.0 / b.0))),

            (BinOp::Rem, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    Err(RuntimeError::DivisionByZero)
                } else {
                    Ok(Value::Int(a % b))
                }
            }
            (BinOp::Rem, Value::Float(a), Value::Float(b)) => Ok(Value::Float(OrdF64(a.0 % b.0))),

            (BinOp::Pow, Value::Int(a), Value::Int(b)) => {
                if b < 0 {
                    Err(RuntimeError::TypeError(
                        "negative integer exponent".to_string(),
                    ))
                } else if b > u32::MAX as i64 {
                    Err(RuntimeError::IntOverflow)
                } else {
                    a.checked_pow(b as u32)
                        .map(Value::Int)
                        .ok_or(RuntimeError::IntOverflow)
                }
            }
            (BinOp::Pow, Value::Float(a), Value::Float(b)) => {
                Ok(Value::Float(OrdF64(a.0.powf(b.0))))
            }

            // ── Comparison ────────────────────────────────────────────────
            (BinOp::Eq, l, r) => Ok(Value::Bool(l == r)),
            (BinOp::Ne, l, r) => Ok(Value::Bool(l != r)),
            (BinOp::Lt, l, r) => Ok(Value::Bool(l < r)),
            (BinOp::Le, l, r) => Ok(Value::Bool(l <= r)),
            (BinOp::Gt, l, r) => Ok(Value::Bool(l > r)),
            (BinOp::Ge, l, r) => Ok(Value::Bool(l >= r)),

            // ── Bitwise ───────────────────────────────────────────────────
            (BinOp::BitAnd, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
            (BinOp::BitOr, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
            (BinOp::BitXor, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
            // ── Function composition (`>>`) ────────────────────────────────
            // Note: Compose as a BinOp is separate from NodeKind::Compose.
            (BinOp::Compose, Value::Function(f), Value::Function(g)) => {
                let fn_val = FnValue::new_anonymous();
                let id = fn_val.id;
                self.fn_registry.insert(
                    id,
                    Closure {
                        params: vec!["__x".to_string()],
                        body: ClosureBody::Composed {
                            inner: f.id,
                            outer: g.id,
                        },
                        captured: self.env.clone(),
                        is_toplevel: false,
                        is_async: false,
                    },
                );
                Ok(Value::Function(fn_val))
            }

            // ── Is (type check) ───────────────────────────────────────────
            (BinOp::Is, value, Value::String(type_name)) => {
                let tag = TypeTag::of(&value);
                Ok(Value::Bool(tag.name() == type_name.as_str()))
            }

            // ── Trait dispatch fallback ────────────────────────────────────
            // If the inline handler didn't match, try dispatching via
            // registered trait methods (e.g., Add.add, Comparable.compare).
            (op, l, r) => {
                let tag = TypeTag::of(&l);
                let method = match op {
                    BinOp::Add => Some("add"),
                    BinOp::Sub => Some("sub"),
                    BinOp::Mul => Some("mul"),
                    BinOp::Div => Some("div"),
                    BinOp::Rem => Some("rem"),
                    BinOp::Pow => Some("pow"),
                    _ => None,
                };
                if let Some(name) = method {
                    if let Some(result) = self.builtins.call(tag, name, &[l.clone(), r.clone()]) {
                        return result;
                    }
                }
                Err(RuntimeError::TypeError(format!(
                    "operator {op:?} not supported for {l} and {r}"
                )))
            }
        }
    }

    // ── Unary operator evaluation ──────────────────────────────────────────

    #[async_recursion]
    async fn eval_unary_op(
        &mut self,
        op: UnaryOp,
        operand: &AIRNode,
    ) -> Result<Value, RuntimeError> {
        let val = self.eval_expr(operand).await?;
        match (op, val) {
            (UnaryOp::Neg, Value::Int(n)) => n
                .checked_neg()
                .map(Value::Int)
                .ok_or(RuntimeError::IntOverflow),
            (UnaryOp::Neg, Value::Float(f)) => Ok(Value::Float(OrdF64(-f.0))),
            (UnaryOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            (UnaryOp::BitNot, Value::Int(n)) => Ok(Value::Int(!n)),
            (op, val) => Err(RuntimeError::TypeError(format!(
                "unary operator {op:?} not supported for {val}"
            ))),
        }
    }

    // ── Assignment ─────────────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_assign(
        &mut self,
        op: AssignOp,
        target: &AIRNode,
        value: &AIRNode,
    ) -> Result<Value, RuntimeError> {
        let rhs = self.eval_expr(value).await?;
        match &target.kind {
            NodeKind::Identifier { name } => {
                let name = name.name.clone();
                let new_val = match op {
                    AssignOp::Assign => rhs,
                    compound => {
                        let current = self.env.get(&name).cloned().ok_or_else(|| {
                            RuntimeError::UndefinedVariable { name: name.clone() }
                        })?;
                        self.apply_assign_op(compound, current, rhs)?
                    }
                };
                if !self.env.assign(&name, new_val) {
                    return Err(RuntimeError::UndefinedVariable { name });
                }
                Ok(Value::Void)
            }
            NodeKind::FieldAccess { object, field } => {
                let field_name = field.name.clone();
                let obj = self.eval_expr(object).await?;
                match obj {
                    Value::Record(mut rv) => {
                        let new_val = match op {
                            AssignOp::Assign => rhs,
                            compound => {
                                let current =
                                    rv.fields.get(&field_name).cloned().ok_or_else(|| {
                                        RuntimeError::FieldNotFound {
                                            field: field_name.clone(),
                                            type_name: rv.type_name.clone(),
                                        }
                                    })?;
                                self.apply_assign_op(compound, current, rhs)?
                            }
                        };
                        rv.fields.insert(field_name, new_val);
                        // Re-bind the object in the environment
                        if let NodeKind::Identifier { name: obj_name } = &object.kind {
                            let updated = Value::Record(rv);
                            if !self.env.assign(&obj_name.name, updated) {
                                return Err(RuntimeError::UndefinedVariable {
                                    name: obj_name.name.clone(),
                                });
                            }
                        }
                        Ok(Value::Void)
                    }
                    other => Err(RuntimeError::TypeError(format!(
                        "cannot assign to field '{field_name}' on {other}"
                    ))),
                }
            }
            NodeKind::Index { object, index } => {
                let idx = self.eval_expr(index).await?;
                let obj = self.eval_expr(object).await?;
                match (obj, idx) {
                    (Value::List(mut items), Value::Int(i)) => {
                        if i < 0 || i as usize >= items.len() {
                            return Err(RuntimeError::IndexOutOfBounds {
                                index: i,
                                len: items.len(),
                            });
                        }
                        let new_val = match op {
                            AssignOp::Assign => rhs,
                            compound => {
                                let current = items[i as usize].clone();
                                self.apply_assign_op(compound, current, rhs)?
                            }
                        };
                        items[i as usize] = new_val;
                        if let NodeKind::Identifier { name: obj_name } = &object.kind {
                            let updated = Value::List(items);
                            if !self.env.assign(&obj_name.name, updated) {
                                return Err(RuntimeError::UndefinedVariable {
                                    name: obj_name.name.clone(),
                                });
                            }
                        }
                        Ok(Value::Void)
                    }
                    (Value::Map(mut map), key) => {
                        let new_val = match op {
                            AssignOp::Assign => rhs,
                            compound => {
                                let current = map.get(&key).cloned().ok_or_else(|| {
                                    RuntimeError::TypeError(format!("key not found: {key}"))
                                })?;
                                self.apply_assign_op(compound, current, rhs)?
                            }
                        };
                        map.insert(key, new_val);
                        if let NodeKind::Identifier { name: obj_name } = &object.kind {
                            let updated = Value::Map(map);
                            if !self.env.assign(&obj_name.name, updated) {
                                return Err(RuntimeError::UndefinedVariable {
                                    name: obj_name.name.clone(),
                                });
                            }
                        }
                        Ok(Value::Void)
                    }
                    (obj, idx) => Err(RuntimeError::TypeError(format!(
                        "cannot index-assign {obj} with {idx}"
                    ))),
                }
            }
            _ => Err(RuntimeError::NotImplemented(
                "unsupported assignment target".to_string(),
            )),
        }
    }

    fn apply_assign_op(&self, op: AssignOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
        match (op, lhs, rhs) {
            (AssignOp::AddAssign, Value::Int(a), Value::Int(b)) => a
                .checked_add(b)
                .map(Value::Int)
                .ok_or(RuntimeError::IntOverflow),
            (AssignOp::SubAssign, Value::Int(a), Value::Int(b)) => a
                .checked_sub(b)
                .map(Value::Int)
                .ok_or(RuntimeError::IntOverflow),
            (AssignOp::MulAssign, Value::Int(a), Value::Int(b)) => a
                .checked_mul(b)
                .map(Value::Int)
                .ok_or(RuntimeError::IntOverflow),
            (AssignOp::DivAssign, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    Err(RuntimeError::DivisionByZero)
                } else {
                    Ok(Value::Int(a / b))
                }
            }
            (AssignOp::RemAssign, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    Err(RuntimeError::DivisionByZero)
                } else {
                    Ok(Value::Int(a % b))
                }
            }
            (AssignOp::AddAssign, Value::Float(a), Value::Float(b)) => {
                Ok(Value::Float(OrdF64(a.0 + b.0)))
            }
            (AssignOp::SubAssign, Value::Float(a), Value::Float(b)) => {
                Ok(Value::Float(OrdF64(a.0 - b.0)))
            }
            (AssignOp::MulAssign, Value::Float(a), Value::Float(b)) => {
                Ok(Value::Float(OrdF64(a.0 * b.0)))
            }
            (AssignOp::DivAssign, Value::Float(a), Value::Float(b)) => {
                Ok(Value::Float(OrdF64(a.0 / b.0)))
            }
            (AssignOp::AddAssign, Value::String(a), Value::String(b)) => {
                Ok(Value::String(BockString::new(format!("{a}{b}"))))
            }
            (op, l, r) => Err(RuntimeError::TypeError(format!(
                "compound assignment {op:?} not supported for {l} and {r}"
            ))),
        }
    }

    // ── Function calls ─────────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_call(
        &mut self,
        callee: &AIRNode,
        args: &[AirArg],
    ) -> Result<Value, RuntimeError> {
        // Check for global built-in functions first (e.g., print, println, debug).
        if let NodeKind::Identifier { name } = &callee.kind {
            if self.builtins.has_global(&name.name) {
                let mut arg_values: Vec<Value> = Vec::with_capacity(args.len());
                for a in args {
                    arg_values.push(self.eval_expr(&a.value).await?);
                }
                return self
                    .builtins
                    .call_global(&name.name, &arg_values)
                    .expect("has_global check confirmed builtin exists");
            }

            // Check for effect operation dispatch: log(msg) → Logger handler
            if let Some(effect_name) = self.effect_operations.get(&name.name).cloned() {
                let handler = self.effect_handlers.resolve(&effect_name).cloned().ok_or(
                    RuntimeError::NoEffectHandler {
                        effect: effect_name,
                    },
                )?;
                let mut arg_values = Vec::with_capacity(args.len());
                for arg in args {
                    arg_values.push(self.eval_expr(&arg.value).await?);
                }
                return self
                    .dispatch_effect_op(&handler, &name.name, arg_values)
                    .await;
            }
        }

        // Check for associated function calls: `Type.method(args)` is lowered
        // to `Call(FieldAccess(Type, method), args)` with NO self prepended.
        // Associated functions are registered as qualified globals like
        // `Duration.seconds`; detect this before the desugared-method path.
        if let NodeKind::FieldAccess { object, field } = &callee.kind {
            if let NodeKind::Identifier { name: type_name } = &object.kind {
                let qualified = format!("{}.{}", type_name.name, field.name);
                if self.builtins.has_global(&qualified) {
                    let mut arg_values = Vec::with_capacity(args.len());
                    for a in args {
                        arg_values.push(self.eval_expr(&a.value).await?);
                    }
                    return self
                        .builtins
                        .call_global(&qualified, &arg_values)
                        .expect("has_global check confirmed builtin exists");
                }
            }
        }

        // Check for desugared method calls: the lowerer converts `obj.method(args)`
        // into `Call(FieldAccess(obj, method), [obj, ...args])`. Try builtin dispatch
        // before falling through to field-access evaluation.
        if let NodeKind::FieldAccess { object, field } = &callee.kind {
            let recv = self.eval_expr(object).await?;
            let type_tag = TypeTag::of(&recv);
            if self.builtins.has_method(type_tag, &field.name) {
                let mut builtin_args = Vec::with_capacity(args.len());
                builtin_args.push(recv);
                // Skip the first arg (self/receiver inserted by lowerer)
                for a in args.iter().skip(1) {
                    builtin_args.push(self.eval_expr(&a.value).await?);
                }
                // Try higher-order builtin first (needs callback invoker)
                if let Some(ho_func) = self.builtins.get_ho_method(type_tag, &field.name) {
                    return ho_func(&builtin_args, self).await;
                }
                return self
                    .builtins
                    .call(type_tag, &field.name, &builtin_args)
                    .expect("has_method check confirmed builtin exists");
            }
            // Try user-defined impl methods (skip first arg which is receiver duplicate).
            let mut method_args = Vec::with_capacity(args.len().saturating_sub(1));
            for a in args.iter().skip(1) {
                method_args.push(self.eval_expr(&a.value).await?);
            }
            if let Some(result) = self
                .try_call_impl_method(&recv, &field.name, method_args)
                .await?
            {
                return Ok(result);
            }
            // Fallback: try inline collection method dispatch (contains, map,
            // filter, keys, first, etc.) which lives in eval_method_call.
            return self.eval_method_call(object, &field.name, &args[1..]).await;
        }

        let fn_val = self.eval_expr(callee).await?;
        let fn_id = match &fn_val {
            Value::Function(fv) => fv.id,
            other => {
                return Err(RuntimeError::NotCallable {
                    value: other.to_string(),
                })
            }
        };
        let mut arg_values: Vec<Value> = Vec::with_capacity(args.len());
        for a in args {
            arg_values.push(self.eval_expr(&a.value).await?);
        }
        let closure =
            self.fn_registry
                .get(&fn_id)
                .cloned()
                .ok_or_else(|| RuntimeError::NotCallable {
                    value: format!("unregistered fn #{fn_id}"),
                })?;
        self.call_closure(&closure, arg_values).await
    }

    #[async_recursion]
    async fn call_closure(
        &mut self,
        closure: &Closure,
        args: Vec<Value>,
    ) -> Result<Value, RuntimeError> {
        match &closure.body {
            ClosureBody::Air(body) => {
                if closure.params.len() != args.len() {
                    return Err(RuntimeError::ArityMismatch {
                        expected: closure.params.len(),
                        got: args.len(),
                    });
                }
                let body = *body.clone();

                if closure.is_async {
                    // Spawn an async task with a CLONE of the interpreter so it
                    // owns its env, fn_registry, etc. for the duration of the
                    // call. The current interpreter resumes immediately with a
                    // `Value::Future` that resolves when the task completes.
                    let mut sub = self.clone();
                    if !closure.is_toplevel {
                        sub.env = closure.captured.clone();
                    }
                    sub.env.push_scope();
                    for (name, val) in closure.params.iter().zip(args) {
                        sub.env.define(name.clone(), val);
                    }
                    let handle: tokio::task::JoinHandle<Result<Value, RuntimeError>> =
                        tokio::spawn(async move {
                            match sub.eval_expr(&body).await {
                                Err(RuntimeError::Return(v)) => Ok(*v),
                                other => other,
                            }
                        });
                    return Ok(Value::Future(Arc::new(Mutex::new(Some(handle)))));
                }

                // Top-level functions use the current env (all globals visible);
                // closures/lambdas use their captured env (lexical scoping).
                let saved_env = if closure.is_toplevel {
                    self.env.clone()
                } else {
                    std::mem::replace(&mut self.env, closure.captured.clone())
                };
                self.env.push_scope();
                for (name, val) in closure.params.iter().zip(args) {
                    self.env.define(name.clone(), val);
                }
                let result = self.eval_expr(&body).await;
                self.env = saved_env;
                // Convert a `return` signal into the return value.
                match result {
                    Err(RuntimeError::Return(v)) => Ok(*v),
                    other => other,
                }
            }
            ClosureBody::Composed { inner, outer } => {
                let inner_id = *inner;
                let outer_id = *outer;
                let inner_closure = self.fn_registry.get(&inner_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: format!("composed inner fn #{inner_id}"),
                    }
                })?;
                let intermediate = self.call_closure(&inner_closure, args).await?;
                let outer_closure = self.fn_registry.get(&outer_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: format!("composed outer fn #{outer_id}"),
                    }
                })?;
                self.call_closure(&outer_closure, vec![intermediate]).await
            }
            ClosureBody::Native(build) => {
                if closure.params.len() != args.len() {
                    return Err(RuntimeError::ArityMismatch {
                        expected: closure.params.len(),
                        got: args.len(),
                    });
                }
                Ok(build(&args))
            }
        }
    }

    // ── Lambda / closure creation ──────────────────────────────────────────

    fn eval_lambda(&mut self, params: &[AIRNode], body: &AIRNode) -> Result<Value, RuntimeError> {
        let param_names: Vec<String> = params
            .iter()
            .map(|p| match &p.kind {
                NodeKind::Param { pattern, .. } => match &pattern.kind {
                    NodeKind::BindPat { name, .. } => name.name.clone(),
                    NodeKind::WildcardPat => "_".to_string(),
                    _ => "_".to_string(),
                },
                _ => "_".to_string(),
            })
            .collect();

        let fn_val = FnValue::new_anonymous();
        let id = fn_val.id;
        let captured = self.env.clone();
        self.fn_registry.insert(
            id,
            Closure {
                params: param_names,
                body: ClosureBody::Air(Box::new(body.clone())),
                captured,
                is_toplevel: false,
                is_async: false,
            },
        );
        Ok(Value::Function(fn_val))
    }

    // ── Pipe operator ──────────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_pipe(&mut self, left: &AIRNode, right: &AIRNode) -> Result<Value, RuntimeError> {
        let lhs = self.eval_expr(left).await?;
        // If the right-hand side is itself a call, prepend lhs to its args.
        match &right.kind.clone() {
            NodeKind::Call { callee, args, .. } => {
                let callee = callee.clone();
                let args: Vec<AirArg> = args.clone();
                let fn_val = self.eval_expr(&callee).await?;
                let fn_id = match &fn_val {
                    Value::Function(fv) => fv.id,
                    other => {
                        return Err(RuntimeError::NotCallable {
                            value: other.to_string(),
                        })
                    }
                };
                let mut arg_values = vec![lhs];
                for a in &args {
                    arg_values.push(self.eval_expr(&a.value).await?);
                }
                let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: format!("unregistered fn #{fn_id}"),
                    }
                })?;
                self.call_closure(&closure, arg_values).await
            }
            _ => {
                // Evaluate right as a function, pass lhs as single argument.
                let fn_val = self.eval_expr(right).await?;
                let fn_id = match &fn_val {
                    Value::Function(fv) => fv.id,
                    other => {
                        return Err(RuntimeError::NotCallable {
                            value: other.to_string(),
                        })
                    }
                };
                let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: format!("unregistered fn #{fn_id}"),
                    }
                })?;
                self.call_closure(&closure, vec![lhs]).await
            }
        }
    }

    // ── Function composition ───────────────────────────────────────────────

    #[async_recursion]
    async fn eval_compose(
        &mut self,
        left: &AIRNode,
        right: &AIRNode,
    ) -> Result<Value, RuntimeError> {
        let f = self.eval_expr(left).await?;
        let g = self.eval_expr(right).await?;
        let f_id = match &f {
            Value::Function(fv) => fv.id,
            other => {
                return Err(RuntimeError::TypeError(format!(
                    ">> requires functions, got {other}"
                )))
            }
        };
        let g_id = match &g {
            Value::Function(fv) => fv.id,
            other => {
                return Err(RuntimeError::TypeError(format!(
                    ">> requires functions, got {other}"
                )))
            }
        };
        let fn_val = FnValue::new_anonymous();
        let id = fn_val.id;
        self.fn_registry.insert(
            id,
            Closure {
                params: vec!["__x".to_string()],
                body: ClosureBody::Composed {
                    inner: f_id,
                    outer: g_id,
                },
                captured: self.env.clone(),
                is_toplevel: false,
                is_async: false,
            },
        );
        Ok(Value::Function(fn_val))
    }

    // ── Method calls ───────────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_method_call(
        &mut self,
        receiver: &AIRNode,
        method: &str,
        args: &[AirArg],
    ) -> Result<Value, RuntimeError> {
        let recv = self.eval_expr(receiver).await?;
        let method = method.to_string();
        let mut arg_values: Vec<Value> = Vec::with_capacity(args.len());
        for a in args {
            arg_values.push(self.eval_expr(&a.value).await?);
        }

        // Check the builtin registry first.
        let type_tag = TypeTag::of(&recv);
        {
            let mut builtin_args = Vec::with_capacity(1 + arg_values.len());
            builtin_args.push(recv.clone());
            builtin_args.extend(arg_values.iter().cloned());
            // Try higher-order builtin first (needs callback invoker)
            if let Some(ho_func) = self.builtins.get_ho_method(type_tag, &method) {
                return ho_func(&builtin_args, self).await;
            }
            if let Some(result) = self.builtins.call(type_tag, &method, &builtin_args) {
                return result;
            }
        }

        match (&recv, method.as_str()) {
            // ── Universal ─────────────────────────────────────────────────
            (_, "to_string") => Ok(Value::String(BockString::new(recv.to_string()))),

            // ── List ──────────────────────────────────────────────────────
            (Value::List(items), "len") => Ok(Value::Int(items.len() as i64)),
            (Value::List(items), "is_empty") => Ok(Value::Bool(items.is_empty())),
            (Value::List(items), "first") => {
                Ok(Value::Optional(items.first().cloned().map(Box::new)))
            }
            (Value::List(items), "last") => {
                Ok(Value::Optional(items.last().cloned().map(Box::new)))
            }
            (Value::List(items), "get") => {
                let idx = arg_values.first().ok_or(RuntimeError::ArityMismatch {
                    expected: 1,
                    got: 0,
                })?;
                if let Value::Int(i) = idx {
                    let i = *i;
                    if i < 0 || i as usize >= items.len() {
                        Ok(Value::Optional(None))
                    } else {
                        Ok(Value::Optional(Some(Box::new(items[i as usize].clone()))))
                    }
                } else {
                    Err(RuntimeError::TypeError(
                        "List.get expects an Int index".to_string(),
                    ))
                }
            }
            (Value::List(items), "push") => {
                let v = arg_values
                    .into_iter()
                    .next()
                    .ok_or(RuntimeError::ArityMismatch {
                        expected: 1,
                        got: 0,
                    })?;
                let mut new_list = items.clone();
                new_list.push(v);
                Ok(Value::List(new_list))
            }
            (Value::List(items), "contains") => {
                let v = arg_values.first().ok_or(RuntimeError::ArityMismatch {
                    expected: 1,
                    got: 0,
                })?;
                Ok(Value::Bool(items.contains(v)))
            }
            (Value::List(items), "reverse") => {
                let mut v = items.clone();
                v.reverse();
                Ok(Value::List(v))
            }
            (Value::List(items), "map") => {
                let fn_val = arg_values
                    .into_iter()
                    .next()
                    .ok_or(RuntimeError::ArityMismatch {
                        expected: 1,
                        got: 0,
                    })?;
                let fn_id = match fn_val {
                    Value::Function(ref fv) => fv.id,
                    other => {
                        return Err(RuntimeError::TypeError(format!(
                            "List.map expects a function, got {other}"
                        )))
                    }
                };
                let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: "unregistered fn".to_string(),
                    }
                })?;
                let items = items.clone();
                let mut result = Vec::with_capacity(items.len());
                for item in items {
                    result.push(self.call_closure(&closure, vec![item]).await?);
                }
                Ok(Value::List(result))
            }
            (Value::List(items), "filter") => {
                let fn_val = arg_values
                    .into_iter()
                    .next()
                    .ok_or(RuntimeError::ArityMismatch {
                        expected: 1,
                        got: 0,
                    })?;
                let fn_id = match fn_val {
                    Value::Function(ref fv) => fv.id,
                    other => {
                        return Err(RuntimeError::TypeError(format!(
                            "List.filter expects a function, got {other}"
                        )))
                    }
                };
                let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: "unregistered fn".to_string(),
                    }
                })?;
                let items = items.clone();
                let mut result = Vec::new();
                for item in items {
                    if let Value::Bool(true) =
                        self.call_closure(&closure, vec![item.clone()]).await?
                    {
                        result.push(item);
                    }
                }
                Ok(Value::List(result))
            }
            (Value::List(items), "fold") | (Value::List(items), "reduce") => {
                if arg_values.len() != 2 {
                    return Err(RuntimeError::ArityMismatch {
                        expected: 2,
                        got: arg_values.len(),
                    });
                }
                let mut acc = arg_values.remove(0);
                let fn_val = arg_values.remove(0);
                let fn_id = match fn_val {
                    Value::Function(ref fv) => fv.id,
                    other => {
                        return Err(RuntimeError::TypeError(format!(
                            "List.fold expects a function, got {other}"
                        )))
                    }
                };
                let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: "unregistered fn".to_string(),
                    }
                })?;
                let items = items.clone();
                for item in items {
                    acc = self.call_closure(&closure, vec![acc, item]).await?;
                }
                Ok(acc)
            }
            (Value::List(items), "any") => {
                let fn_val = arg_values
                    .into_iter()
                    .next()
                    .ok_or(RuntimeError::ArityMismatch {
                        expected: 1,
                        got: 0,
                    })?;
                let fn_id = match fn_val {
                    Value::Function(ref fv) => fv.id,
                    other => {
                        return Err(RuntimeError::TypeError(format!(
                            "List.any expects a function, got {other}"
                        )))
                    }
                };
                let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: "unregistered fn".to_string(),
                    }
                })?;
                let items = items.clone();
                for item in items {
                    if let Value::Bool(true) = self.call_closure(&closure, vec![item]).await? {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            (Value::List(items), "all") => {
                let fn_val = arg_values
                    .into_iter()
                    .next()
                    .ok_or(RuntimeError::ArityMismatch {
                        expected: 1,
                        got: 0,
                    })?;
                let fn_id = match fn_val {
                    Value::Function(ref fv) => fv.id,
                    other => {
                        return Err(RuntimeError::TypeError(format!(
                            "List.all expects a function, got {other}"
                        )))
                    }
                };
                let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: "unregistered fn".to_string(),
                    }
                })?;
                let items = items.clone();
                for item in items {
                    if let Value::Bool(false) = self.call_closure(&closure, vec![item]).await? {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            (Value::List(items), "find") => {
                let fn_val = arg_values
                    .into_iter()
                    .next()
                    .ok_or(RuntimeError::ArityMismatch {
                        expected: 1,
                        got: 0,
                    })?;
                let fn_id = match fn_val {
                    Value::Function(ref fv) => fv.id,
                    other => {
                        return Err(RuntimeError::TypeError(format!(
                            "List.find expects a function, got {other}"
                        )))
                    }
                };
                let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: "unregistered fn".to_string(),
                    }
                })?;
                let items = items.clone();
                for item in items {
                    if let Value::Bool(true) =
                        self.call_closure(&closure, vec![item.clone()]).await?
                    {
                        return Ok(Value::Optional(Some(Box::new(item))));
                    }
                }
                Ok(Value::Optional(None))
            }
            (Value::List(items), "for_each") => {
                let fn_val = arg_values
                    .into_iter()
                    .next()
                    .ok_or(RuntimeError::ArityMismatch {
                        expected: 1,
                        got: 0,
                    })?;
                let fn_id = match fn_val {
                    Value::Function(ref fv) => fv.id,
                    other => {
                        return Err(RuntimeError::TypeError(format!(
                            "List.for_each expects a function, got {other}"
                        )))
                    }
                };
                let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: "unregistered fn".to_string(),
                    }
                })?;
                let items = items.clone();
                for item in items {
                    self.call_closure(&closure, vec![item]).await?;
                }
                Ok(Value::Void)
            }
            (Value::List(items), "flat_map") => {
                let fn_val = arg_values
                    .into_iter()
                    .next()
                    .ok_or(RuntimeError::ArityMismatch {
                        expected: 1,
                        got: 0,
                    })?;
                let fn_id = match fn_val {
                    Value::Function(ref fv) => fv.id,
                    other => {
                        return Err(RuntimeError::TypeError(format!(
                            "List.flat_map expects a function, got {other}"
                        )))
                    }
                };
                let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                    RuntimeError::NotCallable {
                        value: "unregistered fn".to_string(),
                    }
                })?;
                let items = items.clone();
                let mut result = Vec::new();
                for item in items {
                    match self.call_closure(&closure, vec![item]).await? {
                        Value::List(inner) => result.extend(inner),
                        other => result.push(other),
                    }
                }
                Ok(Value::List(result))
            }
            (Value::List(items), "sort") => {
                let mut v = items.clone();
                v.sort();
                Ok(Value::List(v))
            }
            (Value::List(items), "dedup") => {
                let mut v = items.clone();
                v.dedup();
                Ok(Value::List(v))
            }
            (Value::List(items), "flatten") => {
                let mut result = Vec::new();
                for item in items {
                    match item {
                        Value::List(inner) => result.extend(inner.iter().cloned()),
                        other => result.push(other.clone()),
                    }
                }
                Ok(Value::List(result))
            }
            (Value::List(items), "zip") => {
                let other = match arg_values.first() {
                    Some(Value::List(l)) => l,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "List.zip expects a List argument".to_string(),
                        ))
                    }
                };
                let pairs: Vec<Value> = items
                    .iter()
                    .zip(other.iter())
                    .map(|(a, b)| Value::Tuple(vec![a.clone(), b.clone()]))
                    .collect();
                Ok(Value::List(pairs))
            }
            (Value::List(items), "concat") => {
                let other = match arg_values.first() {
                    Some(Value::List(l)) => l,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "List.concat expects a List argument".to_string(),
                        ))
                    }
                };
                let mut new_list = items.clone();
                new_list.extend_from_slice(other);
                Ok(Value::List(new_list))
            }
            (Value::List(items), "slice") => {
                if arg_values.len() < 2 {
                    return Err(RuntimeError::ArityMismatch {
                        expected: 2,
                        got: arg_values.len(),
                    });
                }
                let start = match &arg_values[0] {
                    Value::Int(i) => *i,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "List.slice expects Int arguments".to_string(),
                        ))
                    }
                };
                let end = match &arg_values[1] {
                    Value::Int(i) => *i,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "List.slice expects Int arguments".to_string(),
                        ))
                    }
                };
                let len = items.len() as i64;
                let start = start.max(0).min(len) as usize;
                let end = end.max(0).min(len) as usize;
                if start >= end {
                    Ok(Value::List(vec![]))
                } else {
                    Ok(Value::List(items[start..end].to_vec()))
                }
            }
            (Value::List(items), "take") => {
                let n = match arg_values.first() {
                    Some(Value::Int(i)) => *i,
                    _ => return Err(RuntimeError::TypeError("List.take expects Int".to_string())),
                };
                let n = (n.max(0) as usize).min(items.len());
                Ok(Value::List(items[..n].to_vec()))
            }
            (Value::List(items), "skip") => {
                let n = match arg_values.first() {
                    Some(Value::Int(i)) => *i,
                    _ => return Err(RuntimeError::TypeError("List.skip expects Int".to_string())),
                };
                let n = (n.max(0) as usize).min(items.len());
                Ok(Value::List(items[n..].to_vec()))
            }
            (Value::List(items), "enumerate") => {
                let result: Vec<Value> = items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| Value::Tuple(vec![Value::Int(i as i64), v.clone()]))
                    .collect();
                Ok(Value::List(result))
            }
            (Value::List(items), "count") => Ok(Value::Int(items.len() as i64)),
            (Value::List(items), "pop") => {
                if items.is_empty() {
                    Ok(Value::List(vec![]))
                } else {
                    Ok(Value::List(items[..items.len() - 1].to_vec()))
                }
            }
            (Value::List(items), "insert") => {
                if arg_values.len() < 2 {
                    return Err(RuntimeError::ArityMismatch {
                        expected: 2,
                        got: arg_values.len(),
                    });
                }
                let idx = match &arg_values[0] {
                    Value::Int(i) => *i as usize,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "List.insert expects Int index".to_string(),
                        ))
                    }
                };
                if idx > items.len() {
                    return Err(RuntimeError::IndexOutOfBounds {
                        index: idx as i64,
                        len: items.len(),
                    });
                }
                let mut new_list = items.clone();
                new_list.insert(idx, arg_values[1].clone());
                Ok(Value::List(new_list))
            }
            (Value::List(items), "remove") => {
                let idx = match arg_values.first() {
                    Some(Value::Int(i)) => *i,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "List.remove expects Int index".to_string(),
                        ))
                    }
                };
                if idx < 0 || idx as usize >= items.len() {
                    return Err(RuntimeError::IndexOutOfBounds {
                        index: idx,
                        len: items.len(),
                    });
                }
                let mut new_list = items.clone();
                new_list.remove(idx as usize);
                Ok(Value::List(new_list))
            }
            (Value::List(items), "index_of") => {
                let needle = arg_values.first().ok_or(RuntimeError::ArityMismatch {
                    expected: 1,
                    got: 0,
                })?;
                match items.iter().position(|v| v == needle) {
                    Some(pos) => Ok(Value::Optional(Some(Box::new(Value::Int(pos as i64))))),
                    None => Ok(Value::Optional(None)),
                }
            }
            (Value::List(items), "join") => {
                let sep = match arg_values.first() {
                    Some(Value::String(s)) => s.as_str().to_owned(),
                    _ => String::new(),
                };
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
                Ok(Value::String(BockString::new(parts.join(&sep))))
            }
            (Value::List(items), "to_set") => {
                let set: std::collections::BTreeSet<Value> = items.iter().cloned().collect();
                Ok(Value::Set(set))
            }

            // ── String ────────────────────────────────────────────────────
            (Value::String(s), "len") => Ok(Value::Int(s.as_str().chars().count() as i64)),
            (Value::String(s), "is_empty") => Ok(Value::Bool(s.as_str().is_empty())),
            (Value::String(s), "to_upper") => {
                Ok(Value::String(BockString::new(s.as_str().to_uppercase())))
            }
            (Value::String(s), "to_lower") => {
                Ok(Value::String(BockString::new(s.as_str().to_lowercase())))
            }
            (Value::String(s), "trim") => Ok(Value::String(BockString::new(s.as_str().trim()))),
            (Value::String(s), "contains") => {
                if let Some(Value::String(sub)) = arg_values.first() {
                    Ok(Value::Bool(s.as_str().contains(sub.as_str())))
                } else {
                    Err(RuntimeError::TypeError(
                        "String.contains expects a String".to_string(),
                    ))
                }
            }
            (Value::String(s), "starts_with") => {
                if let Some(Value::String(prefix)) = arg_values.first() {
                    Ok(Value::Bool(s.as_str().starts_with(prefix.as_str())))
                } else {
                    Err(RuntimeError::TypeError(
                        "String.starts_with expects a String".to_string(),
                    ))
                }
            }
            (Value::String(s), "ends_with") => {
                if let Some(Value::String(suffix)) = arg_values.first() {
                    Ok(Value::Bool(s.as_str().ends_with(suffix.as_str())))
                } else {
                    Err(RuntimeError::TypeError(
                        "String.ends_with expects a String".to_string(),
                    ))
                }
            }
            (Value::String(s), "split") => {
                let sep = match arg_values.first() {
                    Some(Value::String(sep)) => sep.as_str().to_owned(),
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "String.split expects a String separator".to_string(),
                        ))
                    }
                };
                let parts: Vec<Value> = s
                    .as_str()
                    .split(&sep)
                    .map(|p| Value::String(BockString::new(p)))
                    .collect();
                Ok(Value::List(parts))
            }
            (Value::String(s), "replace") => {
                if arg_values.len() < 2 {
                    return Err(RuntimeError::ArityMismatch {
                        expected: 2,
                        got: arg_values.len(),
                    });
                }
                let old = match &arg_values[0] {
                    Value::String(s) => s.as_str().to_owned(),
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "String.replace expects String arguments".to_string(),
                        ))
                    }
                };
                let new_s = match &arg_values[1] {
                    Value::String(s) => s.as_str().to_owned(),
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "String.replace expects String arguments".to_string(),
                        ))
                    }
                };
                Ok(Value::String(BockString::new(
                    s.as_str().replace(&old, &new_s),
                )))
            }
            (Value::String(s), "chars") => {
                let chars: Vec<Value> = s.as_str().chars().map(Value::Char).collect();
                Ok(Value::List(chars))
            }
            (Value::String(s), "substring") => {
                if arg_values.len() < 2 {
                    return Err(RuntimeError::ArityMismatch {
                        expected: 2,
                        got: arg_values.len(),
                    });
                }
                let start = match &arg_values[0] {
                    Value::Int(i) => *i,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "String.substring expects Int arguments".to_string(),
                        ))
                    }
                };
                let end = match &arg_values[1] {
                    Value::Int(i) => *i,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "String.substring expects Int arguments".to_string(),
                        ))
                    }
                };
                let chars: Vec<char> = s.as_str().chars().collect();
                let len = chars.len() as i64;
                let start = start.max(0).min(len) as usize;
                let end = end.max(0).min(len) as usize;
                if start >= end {
                    Ok(Value::String(BockString::new("")))
                } else {
                    Ok(Value::String(BockString::new(
                        chars[start..end].iter().collect::<String>(),
                    )))
                }
            }

            // ── Map ───────────────────────────────────────────────────────
            (Value::Map(map), "len") => Ok(Value::Int(map.len() as i64)),
            (Value::Map(map), "is_empty") => Ok(Value::Bool(map.is_empty())),
            (Value::Map(map), "contains_key") => {
                let k = arg_values.first().ok_or(RuntimeError::ArityMismatch {
                    expected: 1,
                    got: 0,
                })?;
                Ok(Value::Bool(map.contains_key(k)))
            }
            (Value::Map(map), "get") => {
                let k = arg_values.first().ok_or(RuntimeError::ArityMismatch {
                    expected: 1,
                    got: 0,
                })?;
                Ok(Value::Optional(map.get(k).cloned().map(Box::new)))
            }
            (Value::Map(map), "set") => {
                if arg_values.len() < 2 {
                    return Err(RuntimeError::ArityMismatch {
                        expected: 2,
                        got: arg_values.len(),
                    });
                }
                let mut new_map = map.clone();
                new_map.insert(arg_values.remove(0), arg_values.remove(0));
                Ok(Value::Map(new_map))
            }
            (Value::Map(map), "delete") => {
                let k = arg_values.first().ok_or(RuntimeError::ArityMismatch {
                    expected: 1,
                    got: 0,
                })?;
                let mut new_map = map.clone();
                new_map.remove(k);
                Ok(Value::Map(new_map))
            }
            (Value::Map(map), "merge") => {
                let other = match arg_values.first() {
                    Some(Value::Map(m)) => m,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "Map.merge expects a Map argument".to_string(),
                        ))
                    }
                };
                let mut new_map = map.clone();
                for (k, v) in other {
                    new_map.insert(k.clone(), v.clone());
                }
                Ok(Value::Map(new_map))
            }
            (Value::Map(map), "keys") => Ok(Value::List(map.keys().cloned().collect())),
            (Value::Map(map), "values") => Ok(Value::List(map.values().cloned().collect())),
            (Value::Map(map), "entries") => {
                let entries: Vec<Value> = map
                    .iter()
                    .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                    .collect();
                Ok(Value::List(entries))
            }

            // ── Set ───────────────────────────────────────────────────────
            (Value::Set(set), "len") => Ok(Value::Int(set.len() as i64)),
            (Value::Set(set), "is_empty") => Ok(Value::Bool(set.is_empty())),
            (Value::Set(set), "contains") => {
                let v = arg_values.first().ok_or(RuntimeError::ArityMismatch {
                    expected: 1,
                    got: 0,
                })?;
                Ok(Value::Bool(set.contains(v)))
            }

            // ── Range ─────────────────────────────────────────────────────
            (
                Value::Range {
                    start,
                    end,
                    inclusive,
                    ..
                },
                "step",
            ) => {
                let s = start;
                let e = end;
                let i = inclusive;
                if let Some(Value::Int(step)) = arg_values.first() {
                    Ok(Value::Range {
                        start: *s,
                        end: *e,
                        inclusive: *i,
                        step: *step,
                    })
                } else {
                    Err(RuntimeError::TypeError(
                        "Range.step expects an Int".to_string(),
                    ))
                }
            }
            (
                Value::Range {
                    start,
                    end,
                    inclusive,
                    step,
                },
                "to_list",
            ) => Ok(Value::List(range_to_vec(*start, *end, *inclusive, *step))),

            // ── Optional / Result ─────────────────────────────────────────
            (Value::Optional(Some(inner)), "unwrap") => Ok(*inner.clone()),
            (Value::Optional(None), "unwrap") => {
                Err(RuntimeError::TypeError("unwrapped None".to_string()))
            }
            (Value::Optional(opt), "unwrap_or") => {
                let default = arg_values
                    .into_iter()
                    .next()
                    .ok_or(RuntimeError::ArityMismatch {
                        expected: 1,
                        got: 0,
                    })?;
                Ok(opt.as_deref().cloned().unwrap_or(default))
            }
            (Value::Result(Ok(inner)), "unwrap") => Ok(*inner.clone()),
            (Value::Result(Err(e)), "unwrap") => {
                Err(RuntimeError::TypeError(format!("unwrapped Err({e})")))
            }

            // ── Fallback: check user-defined impl methods, then free functions
            (recv_val, _) => {
                // Try user-defined impl methods from method_table.
                if let Some(result) = self
                    .try_call_impl_method(recv_val, &method, arg_values.clone())
                    .await?
                {
                    return Ok(result);
                }
                if let Some(fn_val) = self.env.get(&method).cloned() {
                    let fn_id = match &fn_val {
                        Value::Function(fv) => fv.id,
                        _ => {
                            return Err(RuntimeError::TypeError(format!(
                                "method '{method}' not found on {type_tag}"
                            )))
                        }
                    };
                    let closure = self.fn_registry.get(&fn_id).cloned().ok_or_else(|| {
                        RuntimeError::NotCallable {
                            value: format!("unregistered method '{method}'"),
                        }
                    })?;
                    let mut all_args = vec![recv_val.clone()];
                    all_args.extend(arg_values);
                    self.call_closure(&closure, all_args).await
                } else {
                    Err(RuntimeError::TypeError(format!(
                        "method '{method}' not found on {type_tag}"
                    )))
                }
            }
        }
    }

    // ── User-defined impl method dispatch ──────────────────────────────────

    /// Try to dispatch a method call via the user-defined method table.
    ///
    /// Returns `Ok(Some(value))` if the method was found and called,
    /// `Ok(None)` if no matching method exists, or `Err` on runtime error.
    #[async_recursion]
    async fn try_call_impl_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Option<Value>, RuntimeError> {
        let type_name = match receiver {
            Value::Record(rv) => &rv.type_name,
            _ => return Ok(None),
        };

        let entry = self
            .method_table
            .get(type_name)
            .and_then(|methods| methods.get(method))
            .cloned();

        let (param_names, body) = match entry {
            Some(e) => e,
            None => return Ok(None),
        };

        // Check if first param is `self` — instance method.
        if param_names.first().map(|s| s.as_str()) == Some("self") {
            let saved_env = std::mem::replace(&mut self.env, Environment::new());
            self.env.push_scope();
            self.env.define("self", receiver.clone());
            for (name, val) in param_names.iter().skip(1).zip(args) {
                self.env.define(name.clone(), val);
            }
            let result = self.eval_expr(&body).await;
            self.env = saved_env;
            match result {
                Err(RuntimeError::Return(v)) => Ok(Some(*v)),
                other => other.map(Some),
            }
        } else {
            // Static method — no self parameter.
            let saved_env = std::mem::replace(&mut self.env, Environment::new());
            self.env.push_scope();
            for (name, val) in param_names.iter().zip(args) {
                self.env.define(name.clone(), val);
            }
            let result = self.eval_expr(&body).await;
            self.env = saved_env;
            match result {
                Err(RuntimeError::Return(v)) => Ok(Some(*v)),
                other => other.map(Some),
            }
        }
    }

    // ── Field access ───────────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_field_access(
        &mut self,
        object: &AIRNode,
        field: &str,
    ) -> Result<Value, RuntimeError> {
        let obj = self.eval_expr(object).await?;
        match obj {
            Value::Record(rv) => {
                rv.fields
                    .get(field)
                    .cloned()
                    .ok_or_else(|| RuntimeError::FieldNotFound {
                        field: field.to_string(),
                        type_name: rv.type_name.clone(),
                    })
            }
            Value::Enum(ev) => {
                if field == "variant" {
                    Ok(Value::String(BockString::new(ev.variant.clone())))
                } else {
                    Err(RuntimeError::FieldNotFound {
                        field: field.to_string(),
                        type_name: ev.type_name.clone(),
                    })
                }
            }
            other => Err(RuntimeError::TypeError(format!(
                "cannot access field '{field}' on {other}"
            ))),
        }
    }

    // ── Index access ───────────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_index(
        &mut self,
        object: &AIRNode,
        index: &AIRNode,
    ) -> Result<Value, RuntimeError> {
        let obj = self.eval_expr(object).await?;
        let idx = self.eval_expr(index).await?;
        match (obj, idx) {
            (Value::List(items), Value::Int(i)) => {
                if i < 0 || i as usize >= items.len() {
                    Err(RuntimeError::IndexOutOfBounds {
                        index: i,
                        len: items.len(),
                    })
                } else {
                    Ok(items[i as usize].clone())
                }
            }
            (Value::Tuple(items), Value::Int(i)) => {
                if i < 0 || i as usize >= items.len() {
                    Err(RuntimeError::IndexOutOfBounds {
                        index: i,
                        len: items.len(),
                    })
                } else {
                    Ok(items[i as usize].clone())
                }
            }
            (Value::Map(map), key) => map
                .get(&key)
                .cloned()
                .ok_or_else(|| RuntimeError::TypeError(format!("key not found: {key}"))),
            (Value::String(s), Value::Int(i)) => {
                let chars: Vec<char> = s.as_str().chars().collect();
                if i < 0 || i as usize >= chars.len() {
                    Err(RuntimeError::IndexOutOfBounds {
                        index: i,
                        len: chars.len(),
                    })
                } else {
                    Ok(Value::Char(chars[i as usize]))
                }
            }
            (obj, idx) => Err(RuntimeError::TypeError(format!(
                "cannot index {obj} with {idx}"
            ))),
        }
    }

    // ── Error propagation (`?`) ────────────────────────────────────────────

    #[async_recursion]
    async fn eval_propagate(&mut self, expr: &AIRNode) -> Result<Value, RuntimeError> {
        let val = self.eval_expr(expr).await?;
        match val {
            Value::Optional(Some(inner)) => Ok(*inner),
            Value::Optional(None) => Err(RuntimeError::Propagated(Box::new(Value::Optional(None)))),
            Value::Result(Ok(inner)) => Ok(*inner),
            Value::Result(Err(e)) => Err(RuntimeError::Propagated(e)),
            other => Err(RuntimeError::TypeError(format!(
                "? applied to non-Optional/Result: {other}"
            ))),
        }
    }

    // ── Record construction ────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_record_construct(
        &mut self,
        path: &TypePath,
        fields: &[AirRecordField],
        spread: Option<&AIRNode>,
    ) -> Result<Value, RuntimeError> {
        let type_name = path
            .segments
            .last()
            .map(|s| s.name.as_str())
            .unwrap_or("")
            .to_string();
        let mut record_fields: BTreeMap<String, Value> = BTreeMap::new();

        // Apply spread first (base record).
        if let Some(spread_expr) = spread {
            let spread_val = self.eval_expr(spread_expr).await?;
            if let Value::Record(rv) = spread_val {
                record_fields = rv.fields;
            }
        }

        // Apply explicit fields (override spread values).
        for field in fields {
            let val = match &field.value {
                Some(v) => self.eval_expr(v).await?,
                None => {
                    // Shorthand: field name resolves as a variable.
                    self.env.get(&field.name.name).cloned().ok_or_else(|| {
                        RuntimeError::UndefinedVariable {
                            name: field.name.name.clone(),
                        }
                    })?
                }
            };
            record_fields.insert(field.name.name.clone(), val);
        }

        Ok(Value::Record(RecordValue {
            type_name,
            fields: record_fields,
        }))
    }

    // ── String interpolation ───────────────────────────────────────────────

    #[async_recursion]
    async fn eval_interpolation(
        &mut self,
        parts: &[AirInterpolationPart],
    ) -> Result<Value, RuntimeError> {
        let mut result = String::new();
        for part in parts {
            match part {
                AirInterpolationPart::Literal(s) => result.push_str(s),
                AirInterpolationPart::Expr(expr) => {
                    let val = self.eval_expr(expr).await?;
                    // Use Displayable.display trait method if registered,
                    // otherwise fall back to Rust Display impl.
                    let tag = TypeTag::of(&val);
                    let displayed =
                        match self
                            .builtins
                            .call(tag, "display", std::slice::from_ref(&val))
                        {
                            Some(Ok(Value::String(s))) => s.to_string(),
                            _ => val.to_string(),
                        };
                    result.push_str(&displayed);
                }
            }
        }
        Ok(Value::String(BockString::new(result)))
    }

    // ── Range construction ─────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_range(
        &mut self,
        lo: &AIRNode,
        hi: &AIRNode,
        inclusive: bool,
    ) -> Result<Value, RuntimeError> {
        let lo_val = self.eval_expr(lo).await?;
        let hi_val = self.eval_expr(hi).await?;
        match (lo_val, hi_val) {
            (Value::Int(start), Value::Int(end)) => Ok(Value::Range {
                start,
                end,
                inclusive,
                step: 1,
            }),
            (lo, hi) => Err(RuntimeError::TypeError(format!(
                "range bounds must be Int, got {lo} and {hi}"
            ))),
        }
    }

    // ── Block evaluation ───────────────────────────────────────────────────

    /// Evaluate a block: push scope, execute statements, eval tail expression.
    #[async_recursion]
    pub async fn eval_block(
        &mut self,
        stmts: &[AIRNode],
        tail: Option<&AIRNode>,
    ) -> Result<Value, RuntimeError> {
        self.env.push_scope();

        for stmt in stmts {
            match self.eval_expr(stmt).await {
                Ok(_) => {} // discard intermediate statement result
                Err(e) => {
                    self.env.pop_scope();
                    return Err(e);
                }
            }
        }

        let result = match tail {
            Some(expr) => self.eval_expr(expr).await,
            None => Ok(Value::Void),
        };

        self.env.pop_scope();
        result
    }

    // ── If expression ──────────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_if(
        &mut self,
        let_pattern: Option<&AIRNode>,
        condition: &AIRNode,
        then_block: &AIRNode,
        else_block: Option<&AIRNode>,
    ) -> Result<Value, RuntimeError> {
        if let Some(pat) = let_pattern {
            // `if (let pat = expr) { ... }` — condition is the expression to match.
            let val = self.eval_expr(condition).await?;
            self.env.push_scope();
            let matched = self.try_match_pattern(pat, &val).await;
            if matched {
                let result = self.eval_expr(then_block).await;
                self.env.pop_scope();
                result
            } else {
                self.env.pop_scope();
                match else_block {
                    Some(eb) => self.eval_expr(eb).await,
                    None => Ok(Value::Void),
                }
            }
        } else {
            let cond = self.eval_expr(condition).await?;
            match cond {
                Value::Bool(true) => self.eval_expr(then_block).await,
                Value::Bool(false) => match else_block {
                    Some(eb) => self.eval_expr(eb).await,
                    None => Ok(Value::Void),
                },
                other => Err(RuntimeError::TypeError(format!(
                    "if condition must be Bool, got {other}"
                ))),
            }
        }
    }

    // ── Match expression ───────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_match(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<Value, RuntimeError> {
        let val = self.eval_expr(scrutinee).await?;

        // M-075: Check for non-exhaustive match patterns.
        // If the scrutinee is an enum and no arm uses a wildcard or catch-all
        // bind pattern, emit a warning.
        if matches!(&val, Value::Enum(_)) {
            let has_catch_all = arms.iter().any(|arm| {
                if let NodeKind::MatchArm { pattern, guard, .. } = &arm.kind {
                    guard.is_none()
                        && matches!(
                            pattern.kind,
                            NodeKind::WildcardPat | NodeKind::BindPat { .. }
                        )
                } else {
                    false
                }
            });
            if !has_catch_all {
                eprintln!(
                    "warning: match on enum value may not be exhaustive (no wildcard `_` arm)"
                );
            }
        }

        for arm in arms {
            if let NodeKind::MatchArm {
                pattern,
                guard,
                body,
            } = &arm.kind
            {
                self.env.push_scope();
                let matched = self.try_match_pattern(pattern, &val).await;

                let should_exec = if matched {
                    if let Some(guard_expr) = guard {
                        match self.eval_expr(guard_expr).await {
                            Ok(Value::Bool(b)) => b,
                            Ok(_) => false,
                            Err(_) => false,
                        }
                    } else {
                        true
                    }
                } else {
                    false
                };

                if should_exec {
                    let result = self.eval_expr(body).await;
                    self.env.pop_scope();
                    return result;
                }
                self.env.pop_scope();
            }
        }

        Err(RuntimeError::MatchFailed)
    }

    // ── Pattern matching ───────────────────────────────────────────────────

    /// Attempt to match `value` against `pattern`, binding names into the
    /// current scope. Returns `true` on a successful match.
    #[async_recursion]
    pub async fn try_match_pattern(&mut self, pattern: &AIRNode, value: &Value) -> bool {
        match &pattern.kind.clone() {
            NodeKind::WildcardPat | NodeKind::RestPat => true,

            NodeKind::BindPat { name, .. } => {
                self.env.define(name.name.clone(), value.clone());
                true
            }

            NodeKind::LiteralPat { lit } => {
                matches!(self.eval_literal(lit), Ok(lit_val) if lit_val == *value)
            }

            NodeKind::ConstructorPat { path, fields } => {
                let variant_name = path.segments.last().map(|s| s.name.as_str()).unwrap_or("");
                match (variant_name, value) {
                    ("Some", Value::Optional(Some(inner))) => {
                        fields.len() == 1 && self.try_match_pattern(&fields[0], inner).await
                    }
                    ("None", Value::Optional(None)) => true,
                    ("Ok", Value::Result(Ok(inner))) => {
                        fields.is_empty()
                            || (fields.len() == 1
                                && self.try_match_pattern(&fields[0], inner).await)
                    }
                    ("Err", Value::Result(Err(inner))) => {
                        fields.is_empty()
                            || (fields.len() == 1
                                && self.try_match_pattern(&fields[0], inner).await)
                    }
                    (name, Value::Enum(ev)) if ev.variant == name => {
                        match (&ev.payload, fields.len()) {
                            (None, 0) => true,
                            (Some(inner), 1) => self.try_match_pattern(&fields[0], inner).await,
                            _ => false,
                        }
                    }
                    _ => false,
                }
            }

            NodeKind::RecordPat { path, fields, rest } => {
                if let Value::Record(rv) = value {
                    let type_name = path.segments.last().map(|s| s.name.as_str()).unwrap_or("");
                    if rv.type_name != type_name {
                        return false;
                    }
                    if !rest && fields.len() != rv.fields.len() {
                        return false;
                    }
                    for field in fields {
                        let field_val = match rv.fields.get(&field.name.name) {
                            Some(v) => v.clone(),
                            None => return false,
                        };
                        if let Some(pat) = &field.pattern {
                            if !self.try_match_pattern(pat, &field_val).await {
                                return false;
                            }
                        } else {
                            self.env.define(field.name.name.clone(), field_val);
                        }
                    }
                    true
                } else {
                    false
                }
            }

            NodeKind::TuplePat { elems } => {
                if let Value::Tuple(vals) = value {
                    if elems.len() != vals.len() {
                        return false;
                    }
                    let pairs: Vec<_> = elems
                        .iter()
                        .zip(vals.iter())
                        .map(|(p, v)| (p.clone(), v.clone()))
                        .collect();
                    for (pat, val) in pairs {
                        if !self.try_match_pattern(&pat, &val).await {
                            return false;
                        }
                    }
                    true
                } else {
                    false
                }
            }

            NodeKind::ListPat { elems, rest } => {
                if let Value::List(vals) = value {
                    if elems.len() > vals.len() {
                        return false;
                    }
                    if rest.is_none() && elems.len() != vals.len() {
                        return false;
                    }
                    let pairs: Vec<_> = elems
                        .iter()
                        .zip(vals.iter())
                        .map(|(p, v)| (p.clone(), v.clone()))
                        .collect();
                    for (pat, val) in pairs {
                        if !self.try_match_pattern(&pat, &val).await {
                            return false;
                        }
                    }
                    if let Some(rest_pat) = rest {
                        let rest_vals = Value::List(vals[elems.len()..].to_vec());
                        let rest_pat = rest_pat.clone();
                        self.try_match_pattern(&rest_pat, &rest_vals).await;
                    }
                    true
                } else {
                    false
                }
            }

            NodeKind::OrPat { alternatives } => {
                let alts: Vec<_> = alternatives.to_vec();
                for alt in alts {
                    if self.try_match_pattern(&alt, value).await {
                        return true;
                    }
                }
                false
            }

            NodeKind::RangePat { lo, hi, inclusive } => {
                let lo = lo.clone();
                let hi = hi.clone();
                let inclusive = *inclusive;
                let lo_val = match self.eval_expr(&lo).await {
                    Ok(v) => v,
                    Err(_) => return false,
                };
                let hi_val = match self.eval_expr(&hi).await {
                    Ok(v) => v,
                    Err(_) => return false,
                };
                if inclusive {
                    *value >= lo_val && *value <= hi_val
                } else {
                    *value >= lo_val && *value < hi_val
                }
            }

            _ => false,
        }
    }

    /// Bind `value` to `pattern` in the current scope, for use in `let`.
    #[async_recursion]
    pub async fn bind_pattern(
        &mut self,
        pattern: &AIRNode,
        value: Value,
    ) -> Result<(), RuntimeError> {
        match &pattern.kind.clone() {
            NodeKind::BindPat { name, .. } => {
                self.env.define(name.name.clone(), value);
                Ok(())
            }
            NodeKind::WildcardPat => Ok(()),
            NodeKind::TuplePat { elems } => {
                if let Value::Tuple(vals) = value {
                    if elems.len() != vals.len() {
                        return Err(RuntimeError::MatchFailed);
                    }
                    let pairs: Vec<_> = elems
                        .iter()
                        .zip(vals)
                        .map(|(p, v)| (p.clone(), v))
                        .collect();
                    for (pat, val) in pairs {
                        self.bind_pattern(&pat, val).await?;
                    }
                    Ok(())
                } else {
                    Err(RuntimeError::MatchFailed)
                }
            }
            _ => {
                if self.try_match_pattern(pattern, &value).await {
                    Ok(())
                } else {
                    Err(RuntimeError::MatchFailed)
                }
            }
        }
    }

    // ── Loop evaluation ────────────────────────────────────────────────────

    #[async_recursion]
    async fn eval_for(
        &mut self,
        pattern: &AIRNode,
        iterable: &AIRNode,
        body: &AIRNode,
    ) -> Result<Value, RuntimeError> {
        let iter_val = self.eval_expr(iterable).await?;
        match iter_val {
            Value::List(items) => self.for_loop_items(pattern, body, items).await,
            Value::Range {
                start,
                end,
                inclusive,
                step,
            } => {
                let items = range_to_vec(start, end, inclusive, step);
                self.for_loop_items(pattern, body, items).await
            }
            Value::Set(set) => {
                let items: Vec<Value> = set.into_iter().collect();
                self.for_loop_items(pattern, body, items).await
            }
            Value::Map(map) => {
                let items: Vec<Value> = map
                    .into_iter()
                    .map(|(k, v)| Value::Tuple(vec![k, v]))
                    .collect();
                self.for_loop_items(pattern, body, items).await
            }
            Value::Iterator(it) => self.for_loop_iterator(pattern, body, &it).await,
            other => Err(RuntimeError::TypeError(format!(
                "cannot iterate over {other}"
            ))),
        }
    }

    #[async_recursion]
    async fn for_loop_items(
        &mut self,
        pattern: &AIRNode,
        body: &AIRNode,
        items: Vec<Value>,
    ) -> Result<Value, RuntimeError> {
        for item in items {
            self.env.push_scope();
            let bind_result = self.bind_pattern(pattern, item).await;
            if let Err(e) = bind_result {
                self.env.pop_scope();
                return Err(e);
            }
            let result = self.eval_expr(body).await;
            self.env.pop_scope();
            match result {
                Ok(_) | Err(RuntimeError::Continue) => {}
                Err(RuntimeError::Break(None)) => return Ok(Value::Void),
                Err(RuntimeError::Break(Some(v))) => return Ok(*v),
                Err(e) => return Err(e),
            }
        }
        Ok(Value::Void)
    }

    #[async_recursion]
    async fn for_loop_iterator(
        &mut self,
        pattern: &AIRNode,
        body: &AIRNode,
        it: &crate::value::IteratorValue,
    ) -> Result<Value, RuntimeError> {
        loop {
            let next = {
                let mut kind = it.kind.lock().unwrap();
                kind.next()
            };
            match next {
                IteratorNext::Some(val) => {
                    self.env.push_scope();
                    let bind_result = self.bind_pattern(pattern, val).await;
                    if let Err(e) = bind_result {
                        self.env.pop_scope();
                        return Err(e);
                    }
                    let result = self.eval_expr(body).await;
                    self.env.pop_scope();
                    match result {
                        Ok(_) | Err(RuntimeError::Continue) => {}
                        Err(RuntimeError::Break(None)) => return Ok(Value::Void),
                        Err(RuntimeError::Break(Some(v))) => return Ok(*v),
                        Err(e) => return Err(e),
                    }
                }
                IteratorNext::Done => break,
                IteratorNext::NeedsMapCallback { value, func } => {
                    let fn_val = Value::Function(func);
                    let mapped = self.invoke_callback(&fn_val, &[value]).await?;
                    self.env.push_scope();
                    let bind_result = self.bind_pattern(pattern, mapped).await;
                    if let Err(e) = bind_result {
                        self.env.pop_scope();
                        return Err(e);
                    }
                    let result = self.eval_expr(body).await;
                    self.env.pop_scope();
                    match result {
                        Ok(_) | Err(RuntimeError::Continue) => {}
                        Err(RuntimeError::Break(None)) => return Ok(Value::Void),
                        Err(RuntimeError::Break(Some(v))) => return Ok(*v),
                        Err(e) => return Err(e),
                    }
                }
                IteratorNext::NeedsFilterCallback { value, func } => {
                    let fn_val = Value::Function(func);
                    let keep = self
                        .invoke_callback(&fn_val, std::slice::from_ref(&value))
                        .await?;
                    if keep == Value::Bool(true) {
                        self.env.push_scope();
                        let bind_result = self.bind_pattern(pattern, value).await;
                        if let Err(e) = bind_result {
                            self.env.pop_scope();
                            return Err(e);
                        }
                        let result = self.eval_expr(body).await;
                        self.env.pop_scope();
                        match result {
                            Ok(_) | Err(RuntimeError::Continue) => {}
                            Err(RuntimeError::Break(None)) => return Ok(Value::Void),
                            Err(RuntimeError::Break(Some(v))) => return Ok(*v),
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
        }
        Ok(Value::Void)
    }

    #[async_recursion]
    async fn eval_while(
        &mut self,
        condition: &AIRNode,
        body: &AIRNode,
    ) -> Result<Value, RuntimeError> {
        loop {
            let cond = self.eval_expr(condition).await?;
            match cond {
                Value::Bool(false) => break,
                Value::Bool(true) => {}
                other => {
                    return Err(RuntimeError::TypeError(format!(
                        "while condition must be Bool, got {other}"
                    )))
                }
            }
            match self.eval_expr(body).await {
                Ok(_) | Err(RuntimeError::Continue) => {}
                Err(RuntimeError::Break(None)) => return Ok(Value::Void),
                Err(RuntimeError::Break(Some(v))) => return Ok(*v),
                Err(e) => return Err(e),
            }
        }
        Ok(Value::Void)
    }

    #[async_recursion]
    async fn eval_loop(&mut self, body: &AIRNode) -> Result<Value, RuntimeError> {
        loop {
            match self.eval_expr(body).await {
                Ok(_) | Err(RuntimeError::Continue) => {}
                Err(RuntimeError::Break(None)) => return Ok(Value::Void),
                Err(RuntimeError::Break(Some(v))) => return Ok(*v),
                Err(e) => return Err(e),
            }
        }
    }

    #[async_recursion]
    async fn eval_guard(
        &mut self,
        let_pattern: Option<&AIRNode>,
        condition: &AIRNode,
        else_block: &AIRNode,
    ) -> Result<Value, RuntimeError> {
        if let Some(pat) = let_pattern {
            // guard (let pat = expr) — try pattern match, bind into current scope
            let val = self.eval_expr(condition).await?;
            let matched = self.try_match_pattern(pat, &val).await;
            if matched {
                Ok(Value::Void)
            } else {
                self.eval_expr(else_block).await?;
                Ok(Value::Void)
            }
        } else {
            let cond = self.eval_expr(condition).await?;
            match cond {
                Value::Bool(true) => Ok(Value::Void),
                Value::Bool(false) => {
                    // Execute the divergent else block (return/break/continue/never).
                    // We propagate whatever control-flow signal it raises.
                    self.eval_expr(else_block).await?;
                    Ok(Value::Void)
                }
                other => Err(RuntimeError::TypeError(format!(
                    "guard condition must be Bool, got {other}"
                ))),
            }
        }
    }

    // ── Statement-level execution ──────────────────────────────────────────

    /// Execute a statement node.
    ///
    /// Returns `None` for pure statements (let bindings, for/while loops,
    /// guard) and `Some(value)` for expression-statements (including `loop`
    /// which can yield a break value, and blocks).
    #[async_recursion]
    pub async fn exec_stmt(&mut self, node: &AIRNode) -> Result<Option<Value>, RuntimeError> {
        match &node.kind {
            NodeKind::LetBinding { .. }
            | NodeKind::For { .. }
            | NodeKind::While { .. }
            | NodeKind::Guard { .. } => {
                self.eval_expr(node).await?;
                Ok(None)
            }
            _ => Ok(Some(self.eval_expr(node).await?)),
        }
    }

    /// Execute a block node, returning the value of its tail expression.
    ///
    /// Handles `NodeKind::Block` directly; falls back to `eval_expr` for any
    /// other node kind so callers can pass bare expression nodes too.
    #[async_recursion]
    pub async fn exec_block(&mut self, block: &AIRNode) -> Result<Value, RuntimeError> {
        match &block.kind {
            NodeKind::Block { stmts, tail } => self.eval_block(stmts, tail.as_deref()).await,
            _ => self.eval_expr(block).await,
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Materialise a Range into a `Vec<Value::Int>`.
fn range_to_vec(start: i64, end: i64, inclusive: bool, step: i64) -> Vec<Value> {
    // Determine effective step: if step is the default (1) and the range is
    // descending, automatically use -1 so `5..1` produces `[5, 4, 3, 2]`.
    let effective_step = if step == 1 && start > end {
        -1
    } else if step == 0 {
        return Vec::new();
    } else {
        step
    };

    let mut result = Vec::new();
    let mut i = start;
    if effective_step > 0 {
        while if inclusive { i <= end } else { i < end } {
            result.push(Value::Int(i));
            i = match i.checked_add(effective_step) {
                Some(next) => next,
                None => break,
            };
        }
    } else {
        // Descending range
        while if inclusive { i >= end } else { i > end } {
            result.push(Value::Int(i));
            i = match i.checked_add(effective_step) {
                Some(next) => next,
                None => break,
            };
        }
    }
    result
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{AirHandlerPair, AirMapEntry, NodeId, NodeIdGen};
    use bock_ast::{AssignOp, BinOp, Ident, Literal, TypePath, UnaryOp};
    use bock_errors::{FileId, Span};

    fn span() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }

    fn ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: span(),
        }
    }

    fn type_path(name: &str) -> TypePath {
        TypePath {
            segments: vec![ident(name)],
            span: span(),
        }
    }

    fn gen() -> NodeIdGen {
        NodeIdGen::new()
    }

    fn node(id: NodeId, kind: NodeKind) -> AIRNode {
        AIRNode::new(id, span(), kind)
    }

    fn int_lit(g: &NodeIdGen, n: i64) -> AIRNode {
        node(
            g.next(),
            NodeKind::Literal {
                lit: Literal::Int(n.to_string()),
            },
        )
    }

    fn float_lit(g: &NodeIdGen, f: f64) -> AIRNode {
        node(
            g.next(),
            NodeKind::Literal {
                lit: Literal::Float(f.to_string()),
            },
        )
    }

    fn bool_lit(g: &NodeIdGen, b: bool) -> AIRNode {
        node(
            g.next(),
            NodeKind::Literal {
                lit: Literal::Bool(b),
            },
        )
    }

    fn str_lit(g: &NodeIdGen, s: &str) -> AIRNode {
        node(
            g.next(),
            NodeKind::Literal {
                lit: Literal::String(s.to_string()),
            },
        )
    }

    fn var(g: &NodeIdGen, name: &str) -> AIRNode {
        node(g.next(), NodeKind::Identifier { name: ident(name) })
    }

    // ── Literals ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_int_literal() {
        let mut interp = Interpreter::new();
        let g = gen();
        assert_eq!(interp.eval_expr(&int_lit(&g, 42)).await, Ok(Value::Int(42)));
    }

    #[tokio::test]
    async fn eval_float_literal() {
        let mut interp = Interpreter::new();
        let g = gen();
        let result = interp.eval_expr(&float_lit(&g, 3.5)).await;
        assert!(matches!(result, Ok(Value::Float(_))));
    }

    #[tokio::test]
    async fn eval_bool_literal() {
        let mut interp = Interpreter::new();
        let g = gen();
        assert_eq!(
            interp.eval_expr(&bool_lit(&g, true)).await,
            Ok(Value::Bool(true))
        );
        assert_eq!(
            interp.eval_expr(&bool_lit(&g, false)).await,
            Ok(Value::Bool(false))
        );
    }

    #[tokio::test]
    async fn eval_string_literal() {
        let mut interp = Interpreter::new();
        let g = gen();
        assert_eq!(
            interp.eval_expr(&str_lit(&g, "hello")).await,
            Ok(Value::String(BockString::new("hello")))
        );
    }

    #[tokio::test]
    async fn eval_hex_literal() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::Literal {
                lit: Literal::Int("0xFF".to_string()),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(255)));
    }

    #[tokio::test]
    async fn eval_unit_literal() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(g.next(), NodeKind::Literal { lit: Literal::Unit });
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Void));
    }

    // ── Arithmetic ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_add_ints() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(int_lit(&g, 3)),
                right: Box::new(int_lit(&g, 4)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(7)));
    }

    #[tokio::test]
    async fn eval_add_strings() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(str_lit(&g, "foo")),
                right: Box::new(str_lit(&g, "bar")),
            },
        );
        assert_eq!(
            interp.eval_expr(&n).await,
            Ok(Value::String(BockString::new("foobar")))
        );
    }

    #[tokio::test]
    async fn eval_sub_ints() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Sub,
                left: Box::new(int_lit(&g, 10)),
                right: Box::new(int_lit(&g, 3)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(7)));
    }

    #[tokio::test]
    async fn eval_mul_ints() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Mul,
                left: Box::new(int_lit(&g, 6)),
                right: Box::new(int_lit(&g, 7)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(42)));
    }

    #[tokio::test]
    async fn eval_div_ints() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Div,
                left: Box::new(int_lit(&g, 10)),
                right: Box::new(int_lit(&g, 2)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(5)));
    }

    #[tokio::test]
    async fn eval_div_by_zero() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Div,
                left: Box::new(int_lit(&g, 1)),
                right: Box::new(int_lit(&g, 0)),
            },
        );
        assert!(matches!(
            interp.eval_expr(&n).await,
            Err(RuntimeError::DivisionByZero)
        ));
    }

    #[tokio::test]
    async fn eval_pow() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Pow,
                left: Box::new(int_lit(&g, 2)),
                right: Box::new(int_lit(&g, 10)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(1024)));
    }

    // ── Comparison ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_eq() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Eq,
                left: Box::new(int_lit(&g, 5)),
                right: Box::new(int_lit(&g, 5)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Bool(true)));
    }

    #[tokio::test]
    async fn eval_ne() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Ne,
                left: Box::new(int_lit(&g, 3)),
                right: Box::new(int_lit(&g, 5)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Bool(true)));
    }

    #[tokio::test]
    async fn eval_lt_gt() {
        let mut interp = Interpreter::new();
        let g = gen();
        let lt = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Lt,
                left: Box::new(int_lit(&g, 1)),
                right: Box::new(int_lit(&g, 2)),
            },
        );
        let gt = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Gt,
                left: Box::new(int_lit(&g, 3)),
                right: Box::new(int_lit(&g, 2)),
            },
        );
        assert_eq!(interp.eval_expr(&lt).await, Ok(Value::Bool(true)));
        assert_eq!(interp.eval_expr(&gt).await, Ok(Value::Bool(true)));
    }

    // ── Logical ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_and_short_circuit() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::And,
                left: Box::new(bool_lit(&g, false)),
                right: Box::new(bool_lit(&g, true)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Bool(false)));
    }

    #[tokio::test]
    async fn eval_or_short_circuit() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Or,
                left: Box::new(bool_lit(&g, true)),
                right: Box::new(bool_lit(&g, false)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Bool(true)));
    }

    // ── Unary ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_neg() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::UnaryOp {
                op: UnaryOp::Neg,
                operand: Box::new(int_lit(&g, 7)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(-7)));
    }

    #[tokio::test]
    async fn eval_not() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::UnaryOp {
                op: UnaryOp::Not,
                operand: Box::new(bool_lit(&g, false)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Bool(true)));
    }

    // ── Variable lookup ───────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_identifier() {
        let mut interp = Interpreter::new();
        let g = gen();
        interp.env.define("x", Value::Int(99));
        assert_eq!(interp.eval_expr(&var(&g, "x")).await, Ok(Value::Int(99)));
    }

    #[tokio::test]
    async fn eval_undefined_variable() {
        let mut interp = Interpreter::new();
        let g = gen();
        assert!(matches!(
            interp.eval_expr(&var(&g, "y")).await,
            Err(RuntimeError::UndefinedVariable { .. })
        ));
    }

    // ── Collection literals ───────────────────────────────────────────────

    #[tokio::test]
    async fn eval_list_literal() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::ListLiteral {
                elems: vec![int_lit(&g, 1), int_lit(&g, 2), int_lit(&g, 3)],
            },
        );
        assert_eq!(
            interp.eval_expr(&n).await,
            Ok(Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3)
            ]))
        );
    }

    #[tokio::test]
    async fn eval_tuple_literal() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::TupleLiteral {
                elems: vec![int_lit(&g, 1), bool_lit(&g, true)],
            },
        );
        assert_eq!(
            interp.eval_expr(&n).await,
            Ok(Value::Tuple(vec![Value::Int(1), Value::Bool(true)]))
        );
    }

    #[tokio::test]
    async fn eval_map_literal() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::MapLiteral {
                entries: vec![AirMapEntry {
                    key: str_lit(&g, "a"),
                    value: int_lit(&g, 1),
                }],
            },
        );
        let result = interp.eval_expr(&n).await.unwrap();
        if let Value::Map(map) = result {
            assert_eq!(
                map.get(&Value::String(BockString::new("a"))),
                Some(&Value::Int(1))
            );
        } else {
            panic!("expected Map");
        }
    }

    #[tokio::test]
    async fn eval_set_literal() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::SetLiteral {
                elems: vec![int_lit(&g, 1), int_lit(&g, 2), int_lit(&g, 1)],
            },
        );
        if let Ok(Value::Set(set)) = interp.eval_expr(&n).await {
            assert_eq!(set.len(), 2); // duplicates removed
        } else {
            panic!("expected Set");
        }
    }

    // ── Record construction & field access ────────────────────────────────

    #[tokio::test]
    async fn eval_record_construct_and_field_access() {
        let mut interp = Interpreter::new();
        let g = gen();

        let record = node(
            g.next(),
            NodeKind::RecordConstruct {
                path: type_path("Point"),
                fields: vec![
                    AirRecordField {
                        name: ident("x"),
                        value: Some(Box::new(int_lit(&g, 3))),
                    },
                    AirRecordField {
                        name: ident("y"),
                        value: Some(Box::new(int_lit(&g, 4))),
                    },
                ],
                spread: None,
            },
        );

        let rec_val = interp.eval_expr(&record).await.unwrap();
        interp.env.define("p", rec_val);

        let field_node = node(
            g.next(),
            NodeKind::FieldAccess {
                object: Box::new(var(&g, "p")),
                field: ident("x"),
            },
        );
        assert_eq!(interp.eval_expr(&field_node).await, Ok(Value::Int(3)));
    }

    #[tokio::test]
    async fn eval_record_spread() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Build base record {x: 1, y: 2}
        let base = node(
            g.next(),
            NodeKind::RecordConstruct {
                path: type_path("Point"),
                fields: vec![
                    AirRecordField {
                        name: ident("x"),
                        value: Some(Box::new(int_lit(&g, 1))),
                    },
                    AirRecordField {
                        name: ident("y"),
                        value: Some(Box::new(int_lit(&g, 2))),
                    },
                ],
                spread: None,
            },
        );
        let base_val = interp.eval_expr(&base).await.unwrap();
        interp.env.define("base", base_val);

        // Spread + override y: Point { ..base, y: 99 }
        let spread_record = node(
            g.next(),
            NodeKind::RecordConstruct {
                path: type_path("Point"),
                fields: vec![AirRecordField {
                    name: ident("y"),
                    value: Some(Box::new(int_lit(&g, 99))),
                }],
                spread: Some(Box::new(var(&g, "base"))),
            },
        );
        if let Ok(Value::Record(rv)) = interp.eval_expr(&spread_record).await {
            assert_eq!(rv.fields["x"], Value::Int(1));
            assert_eq!(rv.fields["y"], Value::Int(99));
        } else {
            panic!("expected Record");
        }
    }

    // ── Lambda & function call ─────────────────────────────────────────────

    #[tokio::test]
    async fn eval_lambda_and_call() {
        let mut interp = Interpreter::new();
        let g = gen();

        // (x) => x * 2
        let param = node(
            g.next(),
            NodeKind::Param {
                pattern: Box::new(node(
                    g.next(),
                    NodeKind::BindPat {
                        name: ident("x"),
                        is_mut: false,
                    },
                )),
                ty: None,
                default: None,
            },
        );
        let body = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Mul,
                left: Box::new(var(&g, "x")),
                right: Box::new(int_lit(&g, 2)),
            },
        );
        let lambda = node(
            g.next(),
            NodeKind::Lambda {
                params: vec![param],
                body: Box::new(body),
            },
        );

        let fn_val = interp.eval_expr(&lambda).await.unwrap();
        interp.env.define("double", fn_val);

        let call = node(
            g.next(),
            NodeKind::Call {
                callee: Box::new(var(&g, "double")),
                args: vec![AirArg {
                    label: None,
                    value: int_lit(&g, 5),
                }],
                type_args: vec![],
            },
        );
        assert_eq!(interp.eval_expr(&call).await, Ok(Value::Int(10)));
    }

    #[tokio::test]
    async fn eval_closure_captures_env() {
        let mut interp = Interpreter::new();
        let g = gen();

        interp.env.define("factor", Value::Int(3));

        // (x) => x * factor
        let param = node(
            g.next(),
            NodeKind::Param {
                pattern: Box::new(node(
                    g.next(),
                    NodeKind::BindPat {
                        name: ident("x"),
                        is_mut: false,
                    },
                )),
                ty: None,
                default: None,
            },
        );
        let body = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Mul,
                left: Box::new(var(&g, "x")),
                right: Box::new(var(&g, "factor")),
            },
        );
        let lambda = node(
            g.next(),
            NodeKind::Lambda {
                params: vec![param],
                body: Box::new(body),
            },
        );
        let fn_val = interp.eval_expr(&lambda).await.unwrap();
        interp.env.define("triple", fn_val);

        let call = node(
            g.next(),
            NodeKind::Call {
                callee: Box::new(var(&g, "triple")),
                args: vec![AirArg {
                    label: None,
                    value: int_lit(&g, 4),
                }],
                type_args: vec![],
            },
        );
        assert_eq!(interp.eval_expr(&call).await, Ok(Value::Int(12)));
    }

    // ── Pipe operator ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_pipe_to_function() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Register (x) => x + 1 as "inc"
        let param = node(
            g.next(),
            NodeKind::Param {
                pattern: Box::new(node(
                    g.next(),
                    NodeKind::BindPat {
                        name: ident("x"),
                        is_mut: false,
                    },
                )),
                ty: None,
                default: None,
            },
        );
        let body = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(var(&g, "x")),
                right: Box::new(int_lit(&g, 1)),
            },
        );
        let lambda = node(
            g.next(),
            NodeKind::Lambda {
                params: vec![param],
                body: Box::new(body),
            },
        );
        let fn_val = interp.eval_expr(&lambda).await.unwrap();
        interp.env.define("inc", fn_val);

        // 5 |> inc
        let pipe = node(
            g.next(),
            NodeKind::Pipe {
                left: Box::new(int_lit(&g, 5)),
                right: Box::new(var(&g, "inc")),
            },
        );
        assert_eq!(interp.eval_expr(&pipe).await, Ok(Value::Int(6)));
    }

    // ── If expression ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_if_true_branch() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(bool_lit(&g, true)),
                then_block: Box::new(int_lit(&g, 1)),
                else_block: Some(Box::new(int_lit(&g, 2))),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(1)));
    }

    #[tokio::test]
    async fn eval_if_false_branch() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(bool_lit(&g, false)),
                then_block: Box::new(int_lit(&g, 1)),
                else_block: Some(Box::new(int_lit(&g, 2))),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(2)));
    }

    // ── Match expression ──────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_match_literal_pattern() {
        let mut interp = Interpreter::new();
        let g = gen();

        let arm1 = node(
            g.next(),
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    g.next(),
                    NodeKind::LiteralPat {
                        lit: Literal::Int("1".to_string()),
                    },
                )),
                guard: None,
                body: Box::new(str_lit(&g, "one")),
            },
        );
        let arm2 = node(
            g.next(),
            NodeKind::MatchArm {
                pattern: Box::new(node(g.next(), NodeKind::WildcardPat)),
                guard: None,
                body: Box::new(str_lit(&g, "other")),
            },
        );
        let m = node(
            g.next(),
            NodeKind::Match {
                scrutinee: Box::new(int_lit(&g, 1)),
                arms: vec![arm1, arm2],
            },
        );
        assert_eq!(
            interp.eval_expr(&m).await,
            Ok(Value::String(BockString::new("one")))
        );
    }

    #[tokio::test]
    async fn eval_match_bind_pattern() {
        let mut interp = Interpreter::new();
        let g = gen();

        let arm = node(
            g.next(),
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    g.next(),
                    NodeKind::BindPat {
                        name: ident("n"),
                        is_mut: false,
                    },
                )),
                guard: None,
                body: Box::new(node(
                    g.next(),
                    NodeKind::BinaryOp {
                        op: BinOp::Mul,
                        left: Box::new(var(&g, "n")),
                        right: Box::new(int_lit(&g, 2)),
                    },
                )),
            },
        );
        let m = node(
            g.next(),
            NodeKind::Match {
                scrutinee: Box::new(int_lit(&g, 5)),
                arms: vec![arm],
            },
        );
        assert_eq!(interp.eval_expr(&m).await, Ok(Value::Int(10)));
    }

    // ── Error propagation ─────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_propagate_ok() {
        let mut interp = Interpreter::new();
        let g = gen();
        // Ok(42)?  →  42
        let ok_node = node(
            g.next(),
            NodeKind::ResultConstruct {
                variant: ResultVariant::Ok,
                value: Some(Box::new(int_lit(&g, 42))),
            },
        );
        let prop = node(
            g.next(),
            NodeKind::Propagate {
                expr: Box::new(ok_node),
            },
        );
        assert_eq!(interp.eval_expr(&prop).await, Ok(Value::Int(42)));
    }

    #[tokio::test]
    async fn eval_propagate_err() {
        let mut interp = Interpreter::new();
        let g = gen();
        // Err("boom")?  →  Propagated
        let err_node = node(
            g.next(),
            NodeKind::ResultConstruct {
                variant: ResultVariant::Err,
                value: Some(Box::new(str_lit(&g, "boom"))),
            },
        );
        let prop = node(
            g.next(),
            NodeKind::Propagate {
                expr: Box::new(err_node),
            },
        );
        assert!(matches!(
            interp.eval_expr(&prop).await,
            Err(RuntimeError::Propagated(_))
        ));
    }

    #[tokio::test]
    async fn eval_propagate_some() {
        let mut interp = Interpreter::new();
        let g = gen();
        // Some(7)? → 7
        interp
            .env
            .define("opt", Value::Optional(Some(Box::new(Value::Int(7)))));
        let prop = node(
            g.next(),
            NodeKind::Propagate {
                expr: Box::new(var(&g, "opt")),
            },
        );
        assert_eq!(interp.eval_expr(&prop).await, Ok(Value::Int(7)));
    }

    #[tokio::test]
    async fn eval_propagate_none() {
        let mut interp = Interpreter::new();
        let g = gen();
        interp.env.define("opt", Value::Optional(None));
        let prop = node(
            g.next(),
            NodeKind::Propagate {
                expr: Box::new(var(&g, "opt")),
            },
        );
        assert!(matches!(
            interp.eval_expr(&prop).await,
            Err(RuntimeError::Propagated(_))
        ));
    }

    // ── String interpolation ──────────────────────────────────────────────

    #[tokio::test]
    async fn eval_interpolation() {
        let mut interp = Interpreter::new();
        let g = gen();
        interp
            .env
            .define("name", Value::String(BockString::new("world")));
        let n = node(
            g.next(),
            NodeKind::Interpolation {
                parts: vec![
                    AirInterpolationPart::Literal("Hello, ".to_string()),
                    AirInterpolationPart::Expr(Box::new(var(&g, "name"))),
                    AirInterpolationPart::Literal("!".to_string()),
                ],
            },
        );
        assert_eq!(
            interp.eval_expr(&n).await,
            Ok(Value::String(BockString::new("Hello, world!")))
        );
    }

    // ── Range ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_range_exclusive() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::Range {
                lo: Box::new(int_lit(&g, 1)),
                hi: Box::new(int_lit(&g, 4)),
                inclusive: false,
            },
        );
        assert_eq!(
            interp.eval_expr(&n).await,
            Ok(Value::Range {
                start: 1,
                end: 4,
                inclusive: false,
                step: 1
            })
        );
    }

    #[tokio::test]
    async fn eval_range_inclusive() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::Range {
                lo: Box::new(int_lit(&g, 1)),
                hi: Box::new(int_lit(&g, 3)),
                inclusive: true,
            },
        );
        assert_eq!(
            interp.eval_expr(&n).await,
            Ok(Value::Range {
                start: 1,
                end: 3,
                inclusive: true,
                step: 1
            })
        );
    }

    // ── Block with let binding ─────────────────────────────────────────────

    #[tokio::test]
    async fn eval_block_with_let_binding() {
        let mut interp = Interpreter::new();
        let g = gen();

        // { let x = 10; x + 5 }
        let let_stmt = node(
            g.next(),
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(node(
                    g.next(),
                    NodeKind::BindPat {
                        name: ident("x"),
                        is_mut: false,
                    },
                )),
                ty: None,
                value: Box::new(int_lit(&g, 10)),
            },
        );
        let tail = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(var(&g, "x")),
                right: Box::new(int_lit(&g, 5)),
            },
        );
        let block = node(
            g.next(),
            NodeKind::Block {
                stmts: vec![let_stmt],
                tail: Some(Box::new(tail)),
            },
        );
        assert_eq!(interp.eval_expr(&block).await, Ok(Value::Int(15)));
    }

    // ── Index access ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_index_list() {
        let mut interp = Interpreter::new();
        let g = gen();
        interp
            .env
            .define("lst", Value::List(vec![Value::Int(10), Value::Int(20)]));
        let n = node(
            g.next(),
            NodeKind::Index {
                object: Box::new(var(&g, "lst")),
                index: Box::new(int_lit(&g, 1)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(20)));
    }

    // ── Result construction ───────────────────────────────────────────────

    #[tokio::test]
    async fn eval_result_ok() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::ResultConstruct {
                variant: ResultVariant::Ok,
                value: Some(Box::new(int_lit(&g, 42))),
            },
        );
        assert_eq!(
            interp.eval_expr(&n).await,
            Ok(Value::Result(Ok(Box::new(Value::Int(42)))))
        );
    }

    #[tokio::test]
    async fn eval_result_err() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::ResultConstruct {
                variant: ResultVariant::Err,
                value: Some(Box::new(str_lit(&g, "oops"))),
            },
        );
        assert_eq!(
            interp.eval_expr(&n).await,
            Ok(Value::Result(Err(Box::new(Value::String(
                BockString::new("oops")
            )))))
        );
    }

    // ── Function composition ──────────────────────────────────────────────

    #[tokio::test]
    async fn eval_compose_functions() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Register inc = (x) => x + 1
        let double_body = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Mul,
                left: Box::new(var(&g, "x")),
                right: Box::new(int_lit(&g, 2)),
            },
        );
        interp.register_fn("double", vec!["x".to_string()], double_body);

        let inc_body = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(var(&g, "x")),
                right: Box::new(int_lit(&g, 1)),
            },
        );
        interp.register_fn("inc", vec!["x".to_string()], inc_body);

        // double >> inc  — apply double first, then inc
        let compose = node(
            g.next(),
            NodeKind::Compose {
                left: Box::new(var(&g, "double")),
                right: Box::new(var(&g, "inc")),
            },
        );
        let fn_val = interp.eval_expr(&compose).await.unwrap();
        interp.env.define("double_then_inc", fn_val);

        let call = node(
            g.next(),
            NodeKind::Call {
                callee: Box::new(var(&g, "double_then_inc")),
                args: vec![AirArg {
                    label: None,
                    value: int_lit(&g, 5),
                }],
                type_args: vec![],
            },
        );
        // double(5) = 10, inc(10) = 11
        assert_eq!(interp.eval_expr(&call).await, Ok(Value::Int(11)));
    }

    // ── Method calls ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_list_len_method() {
        let mut interp = Interpreter::new();
        let g = gen();
        interp.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );
        let n = node(
            g.next(),
            NodeKind::MethodCall {
                receiver: Box::new(var(&g, "lst")),
                method: ident("len"),
                type_args: vec![],
                args: vec![],
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(3)));
    }

    #[tokio::test]
    async fn eval_list_map_method() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Register double = (x) => x * 2
        let body = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Mul,
                left: Box::new(var(&g, "x")),
                right: Box::new(int_lit(&g, 2)),
            },
        );
        interp.register_fn("double", vec!["x".to_string()], body);

        interp.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );

        let n = node(
            g.next(),
            NodeKind::MethodCall {
                receiver: Box::new(var(&g, "lst")),
                method: ident("map"),
                type_args: vec![],
                args: vec![AirArg {
                    label: None,
                    value: var(&g, "double"),
                }],
            },
        );
        assert_eq!(
            interp.eval_expr(&n).await,
            Ok(Value::List(vec![
                Value::Int(2),
                Value::Int(4),
                Value::Int(6)
            ]))
        );
    }

    // ── Bitwise ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn eval_bitwise_and() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::BitAnd,
                left: Box::new(int_lit(&g, 0b1100)),
                right: Box::new(int_lit(&g, 0b1010)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(0b1000)));
    }

    #[tokio::test]
    async fn eval_bitwise_or() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::BitOr,
                left: Box::new(int_lit(&g, 0b1100)),
                right: Box::new(int_lit(&g, 0b1010)),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Int(0b1110)));
    }

    // ── Statement execution helpers ────────────────────────────────────────

    fn bind_pat(g: &NodeIdGen, name: &str) -> AIRNode {
        node(
            g.next(),
            NodeKind::BindPat {
                name: ident(name),
                is_mut: false,
            },
        )
    }

    fn let_stmt(g: &NodeIdGen, pat: AIRNode, val: AIRNode) -> AIRNode {
        node(
            g.next(),
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(pat),
                ty: None,
                value: Box::new(val),
            },
        )
    }

    fn assign_node(g: &NodeIdGen, name: &str, val: AIRNode) -> AIRNode {
        node(
            g.next(),
            NodeKind::Assign {
                op: AssignOp::Assign,
                target: Box::new(var(g, name)),
                value: Box::new(val),
            },
        )
    }

    fn add(g: &NodeIdGen, left: AIRNode, right: AIRNode) -> AIRNode {
        node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(left),
                right: Box::new(right),
            },
        )
    }

    fn lt(g: &NodeIdGen, left: AIRNode, right: AIRNode) -> AIRNode {
        node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Lt,
                left: Box::new(left),
                right: Box::new(right),
            },
        )
    }

    fn block(g: &NodeIdGen, stmts: Vec<AIRNode>, tail: Option<AIRNode>) -> AIRNode {
        node(
            g.next(),
            NodeKind::Block {
                stmts,
                tail: tail.map(Box::new),
            },
        )
    }

    fn list_lit(g: &NodeIdGen, elems: Vec<AIRNode>) -> AIRNode {
        node(g.next(), NodeKind::ListLiteral { elems })
    }

    // ── exec_stmt / exec_block tests ───────────────────────────────────────

    #[tokio::test]
    async fn exec_stmt_let_binding_returns_none() {
        let mut interp = Interpreter::new();
        let g = gen();
        let stmt = let_stmt(&g, bind_pat(&g, "x"), int_lit(&g, 99));
        let result = interp.exec_stmt(&stmt).await.unwrap();
        assert_eq!(result, None);
        assert_eq!(interp.env.get("x"), Some(&Value::Int(99)));
    }

    #[tokio::test]
    async fn exec_block_returns_tail_expression() {
        // { let a = 3; a + 4 }  =>  7
        let mut interp = Interpreter::new();
        let g = gen();
        let blk = block(
            &g,
            vec![let_stmt(&g, bind_pat(&g, "a"), int_lit(&g, 3))],
            Some(add(&g, var(&g, "a"), int_lit(&g, 4))),
        );
        assert_eq!(interp.exec_block(&blk).await, Ok(Value::Int(7)));
    }

    #[tokio::test]
    async fn block_scope_variables_do_not_leak() {
        // { let inner = 99 }  — inner should not be visible afterward
        let mut interp = Interpreter::new();
        let g = gen();
        let blk = block(
            &g,
            vec![let_stmt(&g, bind_pat(&g, "inner"), int_lit(&g, 99))],
            None,
        );
        interp.exec_block(&blk).await.unwrap();
        assert_eq!(interp.env.get("inner"), None);
    }

    #[tokio::test]
    async fn for_loop_iterates_over_list() {
        // sum = 0; for x in [1, 2, 3] { sum = sum + x }  => sum == 6
        let mut interp = Interpreter::new();
        let g = gen();
        interp.env.define("sum", Value::Int(0));
        let for_node = node(
            g.next(),
            NodeKind::For {
                pattern: Box::new(bind_pat(&g, "x")),
                iterable: Box::new(list_lit(
                    &g,
                    vec![int_lit(&g, 1), int_lit(&g, 2), int_lit(&g, 3)],
                )),
                body: Box::new(assign_node(
                    &g,
                    "sum",
                    add(&g, var(&g, "sum"), var(&g, "x")),
                )),
            },
        );
        assert_eq!(interp.eval_expr(&for_node).await, Ok(Value::Void));
        assert_eq!(interp.env.get("sum"), Some(&Value::Int(6)));
    }

    #[tokio::test]
    async fn for_loop_break_exits_early() {
        // for x in [1, 2, 3] { break }  — completes without error
        let mut interp = Interpreter::new();
        let g = gen();
        let break_node = node(g.next(), NodeKind::Break { value: None });
        let for_node = node(
            g.next(),
            NodeKind::For {
                pattern: Box::new(bind_pat(&g, "x")),
                iterable: Box::new(list_lit(
                    &g,
                    vec![int_lit(&g, 1), int_lit(&g, 2), int_lit(&g, 3)],
                )),
                body: Box::new(break_node),
            },
        );
        assert_eq!(interp.eval_expr(&for_node).await, Ok(Value::Void));
    }

    #[tokio::test]
    async fn while_loop_does_not_execute_when_false() {
        // while (false) { <never reached> }
        let mut interp = Interpreter::new();
        let g = gen();
        let cond = bool_lit(&g, false);
        let body = block(&g, vec![], None);
        let while_node = node(
            g.next(),
            NodeKind::While {
                condition: Box::new(cond),
                body: Box::new(body),
            },
        );
        assert_eq!(interp.eval_expr(&while_node).await, Ok(Value::Void));
    }

    #[tokio::test]
    async fn while_loop_counts_to_three() {
        // count = 0; while (count < 3) { count = count + 1 }  => count == 3
        let mut interp = Interpreter::new();
        let g = gen();
        interp.env.define("count", Value::Int(0));
        let cond = lt(&g, var(&g, "count"), int_lit(&g, 3));
        let body = assign_node(&g, "count", add(&g, var(&g, "count"), int_lit(&g, 1)));
        let while_node = node(
            g.next(),
            NodeKind::While {
                condition: Box::new(cond),
                body: Box::new(body),
            },
        );
        assert_eq!(interp.eval_expr(&while_node).await, Ok(Value::Void));
        assert_eq!(interp.env.get("count"), Some(&Value::Int(3)));
    }

    #[tokio::test]
    async fn loop_break_with_value() {
        // loop { break 42 }  => 42
        let mut interp = Interpreter::new();
        let g = gen();
        let break_node = node(
            g.next(),
            NodeKind::Break {
                value: Some(Box::new(int_lit(&g, 42))),
            },
        );
        let loop_node = node(
            g.next(),
            NodeKind::Loop {
                body: Box::new(break_node),
            },
        );
        assert_eq!(interp.eval_expr(&loop_node).await, Ok(Value::Int(42)));
    }

    #[tokio::test]
    async fn loop_break_without_value() {
        // loop { break }  => Void
        let mut interp = Interpreter::new();
        let g = gen();
        let break_node = node(g.next(), NodeKind::Break { value: None });
        let loop_node = node(
            g.next(),
            NodeKind::Loop {
                body: Box::new(break_node),
            },
        );
        assert_eq!(interp.eval_expr(&loop_node).await, Ok(Value::Void));
    }

    #[tokio::test]
    async fn guard_passes_when_condition_true() {
        // guard (true) else { return () }  => Void (else block not executed)
        let mut interp = Interpreter::new();
        let g = gen();
        let else_blk = node(g.next(), NodeKind::Return { value: None });
        let guard_node = node(
            g.next(),
            NodeKind::Guard {
                let_pattern: None,
                condition: Box::new(bool_lit(&g, true)),
                else_block: Box::new(else_blk),
            },
        );
        assert_eq!(interp.eval_expr(&guard_node).await, Ok(Value::Void));
    }

    #[tokio::test]
    async fn guard_else_diverges_when_condition_false() {
        // guard (false) else { return () }  => propagates Return signal
        let mut interp = Interpreter::new();
        let g = gen();
        let else_blk = node(g.next(), NodeKind::Return { value: None });
        let guard_node = node(
            g.next(),
            NodeKind::Guard {
                let_pattern: None,
                condition: Box::new(bool_lit(&g, false)),
                else_block: Box::new(else_blk),
            },
        );
        assert_eq!(
            interp.eval_expr(&guard_node).await,
            Err(RuntimeError::Return(Box::new(Value::Void)))
        );
    }

    #[tokio::test]
    async fn let_binding_with_tuple_destructuring() {
        // let (a, b) = (1, 2)
        let mut interp = Interpreter::new();
        let g = gen();
        let tuple_pat = node(
            g.next(),
            NodeKind::TuplePat {
                elems: vec![bind_pat(&g, "a"), bind_pat(&g, "b")],
            },
        );
        let tuple_val = node(
            g.next(),
            NodeKind::TupleLiteral {
                elems: vec![int_lit(&g, 1), int_lit(&g, 2)],
            },
        );
        let stmt = let_stmt(&g, tuple_pat, tuple_val);
        assert_eq!(interp.exec_stmt(&stmt).await, Ok(None));
        assert_eq!(interp.env.get("a"), Some(&Value::Int(1)));
        assert_eq!(interp.env.get("b"), Some(&Value::Int(2)));
    }

    // ── Effect handler runtime tests ─────────────────────────────────────

    #[tokio::test]
    async fn effect_op_with_no_handler_errors() {
        let mut interp = Interpreter::new();
        let g = gen();
        let effect_op = node(
            g.next(),
            NodeKind::EffectOp {
                effect: type_path("Log"),
                operation: ident("log"),
                args: vec![],
            },
        );
        let result = interp.eval_expr(&effect_op).await;
        assert!(matches!(result, Err(RuntimeError::NoEffectHandler { .. })));
        if let Err(RuntimeError::NoEffectHandler { effect }) = result {
            assert_eq!(effect, "Log");
        }
    }

    #[tokio::test]
    async fn effect_op_dispatches_to_single_fn_handler() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Register a handler function that returns Int(42)
        let handler_body = int_lit(&g, 42);
        interp.register_fn("my_log", vec!["msg".to_string()], handler_body);

        // Set module-level handler
        let handler_val = interp.env.get("my_log").cloned().unwrap();
        interp
            .effect_handlers
            .set_module_handler("Log", handler_val);

        // Call the effect operation
        let effect_op = node(
            g.next(),
            NodeKind::EffectOp {
                effect: type_path("Log"),
                operation: ident("log"),
                args: vec![AirArg {
                    label: None,
                    value: str_lit(&g, "hello"),
                }],
            },
        );
        let result = interp.eval_expr(&effect_op).await;
        assert_eq!(result, Ok(Value::Int(42)));
    }

    #[tokio::test]
    async fn effect_op_dispatches_to_record_handler() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Register the operation function
        let op_body = int_lit(&g, 99);
        interp.register_fn("_log_op", vec!["msg".to_string()], op_body);
        let op_fn = interp.env.get("_log_op").cloned().unwrap();

        // Create a record handler with the operation as a field
        let mut fields = BTreeMap::new();
        fields.insert("log".to_string(), op_fn);
        let handler_record = Value::Record(RecordValue {
            type_name: "ConsoleLog".to_string(),
            fields,
        });
        interp
            .effect_handlers
            .set_module_handler("Log", handler_record);

        let effect_op = node(
            g.next(),
            NodeKind::EffectOp {
                effect: type_path("Log"),
                operation: ident("log"),
                args: vec![AirArg {
                    label: None,
                    value: str_lit(&g, "test"),
                }],
            },
        );
        let result = interp.eval_expr(&effect_op).await;
        assert_eq!(result, Ok(Value::Int(99)));
    }

    #[tokio::test]
    async fn handling_block_pushes_and_pops_handler() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Register a handler function
        let handler_body = int_lit(&g, 7);
        interp.register_fn("test_handler", vec!["msg".to_string()], handler_body);

        // Create handling block with an effect op call in the body
        let effect_op = node(
            g.next(),
            NodeKind::EffectOp {
                effect: type_path("Log"),
                operation: ident("log"),
                args: vec![AirArg {
                    label: None,
                    value: str_lit(&g, "inside"),
                }],
            },
        );

        let handling = node(
            g.next(),
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path("Log"),
                    handler: Box::new(var(&g, "test_handler")),
                }],
                body: Box::new(effect_op),
            },
        );

        // The handling block should succeed
        let result = interp.eval_expr(&handling).await;
        assert_eq!(result, Ok(Value::Int(7)));

        // After the handling block, the handler should be popped
        assert!(interp.effect_handlers.resolve("Log").is_none());
    }

    #[tokio::test]
    async fn handling_block_pops_on_error() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Handling block whose body accesses an undefined variable
        let bad_body = var(&g, "nonexistent");
        let handler_body = int_lit(&g, 1);
        interp.register_fn("h", vec![], handler_body);

        let handling = node(
            g.next(),
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path("Log"),
                    handler: Box::new(var(&g, "h")),
                }],
                body: Box::new(bad_body),
            },
        );

        let result = interp.eval_expr(&handling).await;
        assert!(result.is_err());
        // Handler stack should still be cleaned up
        assert!(interp.effect_handlers.resolve("Log").is_none());
    }

    #[tokio::test]
    async fn nested_handling_blocks_innermost_wins() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Outer handler returns 1
        let outer_body = int_lit(&g, 1);
        interp.register_fn("outer_h", vec!["m".to_string()], outer_body);

        // Inner handler returns 2
        let inner_body = int_lit(&g, 2);
        interp.register_fn("inner_h", vec!["m".to_string()], inner_body);

        let effect_op = node(
            g.next(),
            NodeKind::EffectOp {
                effect: type_path("Log"),
                operation: ident("log"),
                args: vec![AirArg {
                    label: None,
                    value: str_lit(&g, "test"),
                }],
            },
        );

        let inner_handling = node(
            g.next(),
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path("Log"),
                    handler: Box::new(var(&g, "inner_h")),
                }],
                body: Box::new(effect_op),
            },
        );

        let outer_handling = node(
            g.next(),
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path("Log"),
                    handler: Box::new(var(&g, "outer_h")),
                }],
                body: Box::new(inner_handling),
            },
        );

        let result = interp.eval_expr(&outer_handling).await;
        assert_eq!(result, Ok(Value::Int(2)));
    }

    #[tokio::test]
    async fn three_layer_resolution_local_over_module_over_project() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Project handler returns 1
        let proj_body = int_lit(&g, 1);
        interp.register_fn("proj_h", vec!["m".to_string()], proj_body);
        let proj_val = interp.env.get("proj_h").cloned().unwrap();
        interp.effect_handlers.set_project_handler("Log", proj_val);

        // Module handler returns 2
        let mod_body = int_lit(&g, 2);
        interp.register_fn("mod_h", vec!["m".to_string()], mod_body);
        let mod_val = interp.env.get("mod_h").cloned().unwrap();
        interp.effect_handlers.set_module_handler("Log", mod_val);

        // Local handler returns 3
        let local_body = int_lit(&g, 3);
        interp.register_fn("local_h", vec!["m".to_string()], local_body);

        let effect_op = node(
            g.next(),
            NodeKind::EffectOp {
                effect: type_path("Log"),
                operation: ident("log"),
                args: vec![AirArg {
                    label: None,
                    value: str_lit(&g, "test"),
                }],
            },
        );

        let handling = node(
            g.next(),
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path("Log"),
                    handler: Box::new(var(&g, "local_h")),
                }],
                body: Box::new(effect_op),
            },
        );

        // Local wins over module and project
        let result = interp.eval_expr(&handling).await;
        assert_eq!(result, Ok(Value::Int(3)));
    }

    #[tokio::test]
    async fn module_handle_registers_handler() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Register a handler function
        let handler_body = int_lit(&g, 55);
        interp.register_fn("console_log", vec!["m".to_string()], handler_body);

        // Execute ModuleHandle node
        let module_handle = node(
            g.next(),
            NodeKind::ModuleHandle {
                effect: type_path("Log"),
                handler: Box::new(var(&g, "console_log")),
            },
        );
        let result = interp.eval_expr(&module_handle).await;
        assert_eq!(result, Ok(Value::Void));

        // Now an effect op should resolve to the module handler
        let effect_op = node(
            g.next(),
            NodeKind::EffectOp {
                effect: type_path("Log"),
                operation: ident("log"),
                args: vec![AirArg {
                    label: None,
                    value: str_lit(&g, "test"),
                }],
            },
        );
        let result = interp.eval_expr(&effect_op).await;
        assert_eq!(result, Ok(Value::Int(55)));
    }

    #[tokio::test]
    async fn effect_decl_evaluates_to_void() {
        let mut interp = Interpreter::new();
        let g = gen();
        let effect_decl = node(
            g.next(),
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: bock_ast::Visibility::Private,
                name: ident("Log"),
                generic_params: vec![],
                components: vec![],
                operations: vec![],
            },
        );
        let result = interp.eval_expr(&effect_decl).await;
        assert_eq!(result, Ok(Value::Void));
    }

    #[tokio::test]
    async fn effect_ref_evaluates_to_void() {
        let mut interp = Interpreter::new();
        let g = gen();
        let effect_ref = node(
            g.next(),
            NodeKind::EffectRef {
                path: type_path("Log"),
            },
        );
        let result = interp.eval_expr(&effect_ref).await;
        assert_eq!(result, Ok(Value::Void));
    }

    #[tokio::test]
    async fn no_handler_error_message_is_clear() {
        let err = RuntimeError::NoEffectHandler {
            effect: "Log".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Log"));
        assert!(msg.contains("handling"));
        assert!(msg.contains("handler"));
    }

    #[tokio::test]
    async fn register_effect_dispatches_through_call() {
        let mut interp = Interpreter::new();
        let g = gen();

        // Create an effect with a "log" operation
        let empty_body = node(
            g.next(),
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        let log_op = node(
            g.next(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: bock_ast::Visibility::Public,
                is_async: false,
                name: ident("log"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(empty_body),
            },
        );
        interp.register_effect("Logger", &[log_op]);

        // Register a handler function
        let handler_body = int_lit(&g, 77);
        interp.register_fn("my_handler", vec!["msg".to_string()], handler_body);
        let handler_val = interp.env.get("my_handler").cloned().unwrap();
        interp
            .effect_handlers
            .set_module_handler("Logger", handler_val);

        // Call `log("test")` as a regular Call node (how the lowerer emits it)
        let call_node = node(
            g.next(),
            NodeKind::Call {
                callee: Box::new(var(&g, "log")),
                args: vec![AirArg {
                    label: None,
                    value: str_lit(&g, "test"),
                }],
                type_args: vec![],
            },
        );
        let result = interp.eval_expr(&call_node).await;
        assert_eq!(result, Ok(Value::Int(77)));
    }

    // ── M-074: BinOp::Is (runtime type check) ──────────────────────────

    #[tokio::test]
    async fn is_operator_int() {
        let mut interp = Interpreter::new();
        let g = gen();
        // 42 is Int → true
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Is,
                left: Box::new(int_lit(&g, 42)),
                right: Box::new(str_lit(&g, "Int")),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Bool(true)));
    }

    #[tokio::test]
    async fn is_operator_wrong_type() {
        let mut interp = Interpreter::new();
        let g = gen();
        // 42 is String → false
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Is,
                left: Box::new(int_lit(&g, 42)),
                right: Box::new(str_lit(&g, "String")),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Bool(false)));
    }

    #[tokio::test]
    async fn is_operator_string() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Is,
                left: Box::new(str_lit(&g, "hello")),
                right: Box::new(str_lit(&g, "String")),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Bool(true)));
    }

    #[tokio::test]
    async fn is_operator_bool() {
        let mut interp = Interpreter::new();
        let g = gen();
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Is,
                left: Box::new(bool_lit(&g, true)),
                right: Box::new(str_lit(&g, "Bool")),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Bool(true)));
    }

    #[tokio::test]
    async fn is_operator_list() {
        let mut interp = Interpreter::new();
        let g = gen();
        let list_node = node(
            g.next(),
            NodeKind::ListLiteral {
                elems: vec![int_lit(&g, 1)],
            },
        );
        let n = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Is,
                left: Box::new(list_node),
                right: Box::new(str_lit(&g, "List")),
            },
        );
        assert_eq!(interp.eval_expr(&n).await, Ok(Value::Bool(true)));
    }

    // ── M-075: Match exhaustiveness (covered by warning output) ─────────

    #[tokio::test]
    async fn match_on_enum_with_wildcard_succeeds() {
        let mut interp = Interpreter::new();
        let g = gen();
        // Define an enum value
        interp.env.define(
            "color",
            Value::Enum(crate::value::EnumValue {
                type_name: "Color".to_string(),
                variant: "Red".to_string(),
                payload: None,
            }),
        );
        // Match with wildcard arm
        let match_expr = node(
            g.next(),
            NodeKind::Match {
                scrutinee: Box::new(var(&g, "color")),
                arms: vec![node(
                    g.next(),
                    NodeKind::MatchArm {
                        pattern: Box::new(node(g.next(), NodeKind::WildcardPat)),
                        guard: None,
                        body: Box::new(int_lit(&g, 99)),
                    },
                )],
            },
        );
        assert_eq!(interp.eval_expr(&match_expr).await, Ok(Value::Int(99)));
    }

    // ── M-077: Compound assignment on field/index targets ───────────────

    #[tokio::test]
    async fn compound_assign_field() {
        let mut interp = Interpreter::new();
        let g = gen();
        // obj = Point { x: 10, y: 20 }
        let mut fields = BTreeMap::new();
        fields.insert("x".to_string(), Value::Int(10));
        fields.insert("y".to_string(), Value::Int(20));
        interp.env.define(
            "obj",
            Value::Record(RecordValue {
                type_name: "Point".to_string(),
                fields,
            }),
        );
        // obj.x += 5
        let assign = node(
            g.next(),
            NodeKind::Assign {
                op: AssignOp::AddAssign,
                target: Box::new(node(
                    g.next(),
                    NodeKind::FieldAccess {
                        object: Box::new(var(&g, "obj")),
                        field: ident("x"),
                    },
                )),
                value: Box::new(int_lit(&g, 5)),
            },
        );
        assert_eq!(interp.eval_expr(&assign).await, Ok(Value::Void));
        // Check that obj.x is now 15
        let obj = interp.env.get("obj").unwrap().clone();
        if let Value::Record(rv) = obj {
            assert_eq!(rv.fields.get("x"), Some(&Value::Int(15)));
            assert_eq!(rv.fields.get("y"), Some(&Value::Int(20)));
        } else {
            panic!("expected Record");
        }
    }

    #[tokio::test]
    async fn compound_assign_index() {
        let mut interp = Interpreter::new();
        let g = gen();
        // list = [10, 20, 30]
        interp.env.define(
            "list",
            Value::List(vec![Value::Int(10), Value::Int(20), Value::Int(30)]),
        );
        // list[1] += 5
        let assign = node(
            g.next(),
            NodeKind::Assign {
                op: AssignOp::AddAssign,
                target: Box::new(node(
                    g.next(),
                    NodeKind::Index {
                        object: Box::new(var(&g, "list")),
                        index: Box::new(int_lit(&g, 1)),
                    },
                )),
                value: Box::new(int_lit(&g, 5)),
            },
        );
        assert_eq!(interp.eval_expr(&assign).await, Ok(Value::Void));
        let list = interp.env.get("list").unwrap().clone();
        assert_eq!(
            list,
            Value::List(vec![Value::Int(10), Value::Int(25), Value::Int(30)])
        );
    }

    #[tokio::test]
    async fn assign_field_simple() {
        let mut interp = Interpreter::new();
        let g = gen();
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), Value::String(BockString::new("old")));
        interp.env.define(
            "obj",
            Value::Record(RecordValue {
                type_name: "Item".to_string(),
                fields,
            }),
        );
        // obj.name = "new"
        let assign = node(
            g.next(),
            NodeKind::Assign {
                op: AssignOp::Assign,
                target: Box::new(node(
                    g.next(),
                    NodeKind::FieldAccess {
                        object: Box::new(var(&g, "obj")),
                        field: ident("name"),
                    },
                )),
                value: Box::new(str_lit(&g, "new")),
            },
        );
        assert_eq!(interp.eval_expr(&assign).await, Ok(Value::Void));
        let obj = interp.env.get("obj").unwrap().clone();
        if let Value::Record(rv) = obj {
            assert_eq!(
                rv.fields.get("name"),
                Some(&Value::String(BockString::new("new")))
            );
        } else {
            panic!("expected Record");
        }
    }

    // ── M-078: Descending ranges ────────────────────────────────────────

    #[tokio::test]
    async fn descending_range_exclusive() {
        // 5..1 with default step should produce [5, 4, 3, 2]
        let result = range_to_vec(5, 1, false, 1);
        assert_eq!(
            result,
            vec![Value::Int(5), Value::Int(4), Value::Int(3), Value::Int(2)]
        );
    }

    #[tokio::test]
    async fn descending_range_inclusive() {
        // 5..=1 with default step should produce [5, 4, 3, 2, 1]
        let result = range_to_vec(5, 1, true, 1);
        assert_eq!(
            result,
            vec![
                Value::Int(5),
                Value::Int(4),
                Value::Int(3),
                Value::Int(2),
                Value::Int(1),
            ]
        );
    }

    #[tokio::test]
    async fn descending_range_explicit_negative_step() {
        // 10..0 step -2 should produce [10, 8, 6, 4, 2]
        let result = range_to_vec(10, 0, false, -2);
        assert_eq!(
            result,
            vec![
                Value::Int(10),
                Value::Int(8),
                Value::Int(6),
                Value::Int(4),
                Value::Int(2),
            ]
        );
    }

    #[tokio::test]
    async fn ascending_range_still_works() {
        let result = range_to_vec(1, 5, false, 1);
        assert_eq!(
            result,
            vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)]
        );
    }

    // ── M-079: for..in over Map ─────────────────────────────────────────

    #[tokio::test]
    async fn for_in_map() {
        let mut interp = Interpreter::new();
        let g = gen();
        // Build a map {1: "a", 2: "b"}
        let mut map = BTreeMap::new();
        map.insert(Value::Int(1), Value::String(BockString::new("a")));
        map.insert(Value::Int(2), Value::String(BockString::new("b")));
        interp.env.define("m", Value::Map(map));
        interp.env.define("result", Value::List(vec![]));

        // for (k, v) in m { result = result.push(k) }
        // Simplified: just iterate and collect keys
        let for_expr = node(
            g.next(),
            NodeKind::For {
                pattern: Box::new(node(
                    g.next(),
                    NodeKind::TuplePat {
                        elems: vec![
                            node(
                                g.next(),
                                NodeKind::BindPat {
                                    name: ident("k"),
                                    is_mut: false,
                                },
                            ),
                            node(
                                g.next(),
                                NodeKind::BindPat {
                                    name: ident("v"),
                                    is_mut: false,
                                },
                            ),
                        ],
                    },
                )),
                iterable: Box::new(var(&g, "m")),
                body: Box::new(node(
                    g.next(),
                    NodeKind::Assign {
                        op: AssignOp::Assign,
                        target: Box::new(var(&g, "result")),
                        value: Box::new(node(
                            g.next(),
                            NodeKind::MethodCall {
                                receiver: Box::new(var(&g, "result")),
                                method: ident("push"),
                                args: vec![AirArg {
                                    label: None,
                                    value: var(&g, "k"),
                                }],
                                type_args: vec![],
                            },
                        )),
                    },
                )),
            },
        );
        assert_eq!(interp.eval_expr(&for_expr).await, Ok(Value::Void));
        let result = interp.env.get("result").unwrap().clone();
        // BTreeMap iterates in key order, so keys are [1, 2]
        assert_eq!(result, Value::List(vec![Value::Int(1), Value::Int(2)]));
    }

    // ── M-076: for..in over lazy iterators ──────────────────────────────

    #[tokio::test]
    async fn for_in_lazy_map_iterator() {
        use crate::value::{IteratorKind, IteratorValue};

        let mut interp = Interpreter::new();
        let g = gen();

        // Create a function that doubles its argument
        let double_body = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Mul,
                left: Box::new(var(&g, "x")),
                right: Box::new(int_lit(&g, 2)),
            },
        );
        interp.register_fn("double", vec!["x".to_string()], double_body);
        let double_fn = match interp.env.get("double").unwrap().clone() {
            Value::Function(fv) => fv,
            _ => panic!("expected function"),
        };

        // Create a lazy map iterator over [1, 2, 3] with the double function
        let source = IteratorKind::List {
            items: vec![Value::Int(1), Value::Int(2), Value::Int(3)],
            pos: 0,
        };
        let map_iter = IteratorKind::Map {
            source: std::sync::Arc::new(std::sync::Mutex::new(source)),
            func: double_fn,
        };
        let iter_val = IteratorValue::new(map_iter);
        interp.env.define("it", Value::Iterator(iter_val));
        interp.env.define("result", Value::List(vec![]));

        // for x in it { result = result.push(x) }
        let for_expr = node(
            g.next(),
            NodeKind::For {
                pattern: Box::new(node(
                    g.next(),
                    NodeKind::BindPat {
                        name: ident("item"),
                        is_mut: false,
                    },
                )),
                iterable: Box::new(var(&g, "it")),
                body: Box::new(node(
                    g.next(),
                    NodeKind::Assign {
                        op: AssignOp::Assign,
                        target: Box::new(var(&g, "result")),
                        value: Box::new(node(
                            g.next(),
                            NodeKind::MethodCall {
                                receiver: Box::new(var(&g, "result")),
                                method: ident("push"),
                                args: vec![AirArg {
                                    label: None,
                                    value: var(&g, "item"),
                                }],
                                type_args: vec![],
                            },
                        )),
                    },
                )),
            },
        );
        assert_eq!(interp.eval_expr(&for_expr).await, Ok(Value::Void));
        let result = interp.env.get("result").unwrap().clone();
        assert_eq!(
            result,
            Value::List(vec![Value::Int(2), Value::Int(4), Value::Int(6)])
        );
    }

    #[tokio::test]
    async fn for_in_lazy_filter_iterator() {
        use crate::value::{IteratorKind, IteratorValue};

        let mut interp = Interpreter::new();
        let g = gen();

        // Create a predicate function: x > 2
        let pred_body = node(
            g.next(),
            NodeKind::BinaryOp {
                op: BinOp::Gt,
                left: Box::new(var(&g, "x")),
                right: Box::new(int_lit(&g, 2)),
            },
        );
        interp.register_fn("gt2", vec!["x".to_string()], pred_body);
        let gt2_fn = match interp.env.get("gt2").unwrap().clone() {
            Value::Function(fv) => fv,
            _ => panic!("expected function"),
        };

        // Create a lazy filter iterator over [1, 2, 3, 4, 5]
        let source = IteratorKind::List {
            items: vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Int(4),
                Value::Int(5),
            ],
            pos: 0,
        };
        let filter_iter = IteratorKind::Filter {
            source: std::sync::Arc::new(std::sync::Mutex::new(source)),
            pred: gt2_fn,
        };
        let iter_val = IteratorValue::new(filter_iter);
        interp.env.define("it", Value::Iterator(iter_val));
        interp.env.define("result", Value::List(vec![]));

        // for item in it { result = result.push(item) }
        let for_expr = node(
            g.next(),
            NodeKind::For {
                pattern: Box::new(node(
                    g.next(),
                    NodeKind::BindPat {
                        name: ident("item"),
                        is_mut: false,
                    },
                )),
                iterable: Box::new(var(&g, "it")),
                body: Box::new(node(
                    g.next(),
                    NodeKind::Assign {
                        op: AssignOp::Assign,
                        target: Box::new(var(&g, "result")),
                        value: Box::new(node(
                            g.next(),
                            NodeKind::MethodCall {
                                receiver: Box::new(var(&g, "result")),
                                method: ident("push"),
                                args: vec![AirArg {
                                    label: None,
                                    value: var(&g, "item"),
                                }],
                                type_args: vec![],
                            },
                        )),
                    },
                )),
            },
        );
        assert_eq!(interp.eval_expr(&for_expr).await, Ok(Value::Void));
        let result = interp.env.get("result").unwrap().clone();
        assert_eq!(
            result,
            Value::List(vec![Value::Int(3), Value::Int(4), Value::Int(5)])
        );
    }
}
