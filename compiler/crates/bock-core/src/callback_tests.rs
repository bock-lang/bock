//! Integration tests for higher-order builtin callback dispatch.
//!
//! These tests verify that List.map/filter/fold, Optional.map/flat_map,
//! Result.map/map_err, and for..in over lazy iterators all work correctly
//! through the CallbackInvoker trait.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use bock_air::{AIRNode, AirArg, NodeIdGen, NodeKind};
    use bock_ast::{BinOp, Ident, Literal};
    use bock_ast::{FileId, Span};
    use bock_interp::{Interpreter, IteratorKind, IteratorValue, Value};

    // ── AIR node helpers ──────────────────────────────────────────────────

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }

    fn mk_ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: dummy_span(),
        }
    }

    fn mk_node(gen: &NodeIdGen, kind: NodeKind) -> AIRNode {
        AIRNode::new(gen.next(), dummy_span(), kind)
    }

    fn var(gen: &NodeIdGen, name: &str) -> AIRNode {
        mk_node(
            gen,
            NodeKind::Identifier {
                name: mk_ident(name),
            },
        )
    }

    fn int_lit(gen: &NodeIdGen, n: i64) -> AIRNode {
        mk_node(
            gen,
            NodeKind::Literal {
                lit: Literal::Int(n.to_string()),
            },
        )
    }

    fn add(gen: &NodeIdGen, left: AIRNode, right: AIRNode) -> AIRNode {
        mk_node(
            gen,
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(left),
                right: Box::new(right),
            },
        )
    }

    fn mul(gen: &NodeIdGen, left: AIRNode, right: AIRNode) -> AIRNode {
        mk_node(
            gen,
            NodeKind::BinaryOp {
                op: BinOp::Mul,
                left: Box::new(left),
                right: Box::new(right),
            },
        )
    }

    fn gt(gen: &NodeIdGen, left: AIRNode, right: AIRNode) -> AIRNode {
        mk_node(
            gen,
            NodeKind::BinaryOp {
                op: BinOp::Gt,
                left: Box::new(left),
                right: Box::new(right),
            },
        )
    }

    fn method_call(
        gen: &NodeIdGen,
        receiver: AIRNode,
        method: &str,
        args: Vec<AIRNode>,
    ) -> AIRNode {
        mk_node(
            gen,
            NodeKind::MethodCall {
                receiver: Box::new(receiver),
                method: mk_ident(method),
                type_args: vec![],
                args: args
                    .into_iter()
                    .map(|a| AirArg {
                        label: None,
                        value: a,
                    })
                    .collect(),
            },
        )
    }

    fn bind_pat(gen: &NodeIdGen, name: &str) -> AIRNode {
        mk_node(
            gen,
            NodeKind::BindPat {
                name: mk_ident(name),
                is_mut: false,
            },
        )
    }

    fn assign(gen: &NodeIdGen, name: &str, val: AIRNode) -> AIRNode {
        mk_node(
            gen,
            NodeKind::Assign {
                op: bock_ast::AssignOp::Assign,
                target: Box::new(var(gen, name)),
                value: Box::new(val),
            },
        )
    }

    fn list_lit(gen: &NodeIdGen, elems: Vec<AIRNode>) -> AIRNode {
        mk_node(gen, NodeKind::ListLiteral { elems })
    }

    /// Create an interpreter with bock-core builtins registered.
    fn interp() -> Interpreter {
        let mut i = Interpreter::new();
        crate::register_core(&mut i.builtins);
        i
    }

    // ── List higher-order methods ─────────────────────────────────────────

    #[tokio::test]
    async fn list_map_doubles() {
        // [1,2,3].map((x) => x * 2) == [2,4,6]
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "double",
            vec!["x".into()],
            mul(&g, var(&g, "x"), int_lit(&g, 2)),
        );
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );

        let n = method_call(&g, var(&g, "lst"), "map", vec![var(&g, "double")]);
        assert_eq!(
            i.eval_expr(&n).await,
            Ok(Value::List(vec![
                Value::Int(2),
                Value::Int(4),
                Value::Int(6)
            ]))
        );
    }

    #[tokio::test]
    async fn list_filter_gt2() {
        // [1,2,3,4].filter((x) => x > 2) == [3,4]
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "gt2",
            vec!["x".into()],
            gt(&g, var(&g, "x"), int_lit(&g, 2)),
        );
        i.env.define(
            "lst",
            Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Int(4),
            ]),
        );

        let n = method_call(&g, var(&g, "lst"), "filter", vec![var(&g, "gt2")]);
        assert_eq!(
            i.eval_expr(&n).await,
            Ok(Value::List(vec![Value::Int(3), Value::Int(4)]))
        );
    }

    #[tokio::test]
    async fn list_fold_sum() {
        // [1,2,3].fold(0, (acc, x) => acc + x) == 6
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "sum_fn",
            vec!["acc".into(), "x".into()],
            add(&g, var(&g, "acc"), var(&g, "x")),
        );
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );

        let n = method_call(
            &g,
            var(&g, "lst"),
            "fold",
            vec![int_lit(&g, 0), var(&g, "sum_fn")],
        );
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Int(6)));
    }

    #[tokio::test]
    async fn list_reduce_sum() {
        // [1,2,3].reduce((acc, x) => acc + x) == 6
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "sum_fn",
            vec!["acc".into(), "x".into()],
            add(&g, var(&g, "acc"), var(&g, "x")),
        );
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );

        let n = method_call(&g, var(&g, "lst"), "reduce", vec![var(&g, "sum_fn")]);
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Int(6)));
    }

    #[tokio::test]
    async fn list_any_true() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "gt2",
            vec!["x".into()],
            gt(&g, var(&g, "x"), int_lit(&g, 2)),
        );
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );
        let n = method_call(&g, var(&g, "lst"), "any", vec![var(&g, "gt2")]);
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Bool(true)));
    }

    #[tokio::test]
    async fn list_any_false() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "gt5",
            vec!["x".into()],
            gt(&g, var(&g, "x"), int_lit(&g, 5)),
        );
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );
        let n = method_call(&g, var(&g, "lst"), "any", vec![var(&g, "gt5")]);
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Bool(false)));
    }

    #[tokio::test]
    async fn list_all_true() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "gt0",
            vec!["x".into()],
            gt(&g, var(&g, "x"), int_lit(&g, 0)),
        );
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );
        let n = method_call(&g, var(&g, "lst"), "all", vec![var(&g, "gt0")]);
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Bool(true)));
    }

    #[tokio::test]
    async fn list_all_false() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "gt1",
            vec!["x".into()],
            gt(&g, var(&g, "x"), int_lit(&g, 1)),
        );
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );
        let n = method_call(&g, var(&g, "lst"), "all", vec![var(&g, "gt1")]);
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Bool(false)));
    }

    #[tokio::test]
    async fn list_find_some() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "gt1",
            vec!["x".into()],
            gt(&g, var(&g, "x"), int_lit(&g, 1)),
        );
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );
        let n = method_call(&g, var(&g, "lst"), "find", vec![var(&g, "gt1")]);
        assert_eq!(
            i.eval_expr(&n).await,
            Ok(Value::Optional(Some(Box::new(Value::Int(2)))))
        );
    }

    #[tokio::test]
    async fn list_find_none() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "gt10",
            vec!["x".into()],
            gt(&g, var(&g, "x"), int_lit(&g, 10)),
        );
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );
        let n = method_call(&g, var(&g, "lst"), "find", vec![var(&g, "gt10")]);
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Optional(None)));
    }

    #[tokio::test]
    async fn list_for_each_runs() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn("id", vec!["x".into()], var(&g, "x"));
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );
        let n = method_call(&g, var(&g, "lst"), "for_each", vec![var(&g, "id")]);
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Void));
    }

    #[tokio::test]
    async fn list_flat_map_expand() {
        // [1,2,3].flat_map((x) => [x, x * 2]) == [1,2,2,4,3,6]
        let mut i = interp();
        let g = NodeIdGen::new();
        let body = list_lit(
            &g,
            vec![var(&g, "x"), mul(&g, var(&g, "x"), int_lit(&g, 2))],
        );
        i.register_fn("dup", vec!["x".into()], body);
        i.env.define(
            "lst",
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        );

        let n = method_call(&g, var(&g, "lst"), "flat_map", vec![var(&g, "dup")]);
        assert_eq!(
            i.eval_expr(&n).await,
            Ok(Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(2),
                Value::Int(4),
                Value::Int(3),
                Value::Int(6)
            ]))
        );
    }

    // ── Optional higher-order methods ─────────────────────────────────────

    #[tokio::test]
    async fn optional_map_some() {
        // Some(5).map((x) => x + 1) == Some(6)
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "inc",
            vec!["x".into()],
            add(&g, var(&g, "x"), int_lit(&g, 1)),
        );
        i.env
            .define("opt", Value::Optional(Some(Box::new(Value::Int(5)))));

        let n = method_call(&g, var(&g, "opt"), "map", vec![var(&g, "inc")]);
        assert_eq!(
            i.eval_expr(&n).await,
            Ok(Value::Optional(Some(Box::new(Value::Int(6)))))
        );
    }

    #[tokio::test]
    async fn optional_map_none() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "inc",
            vec!["x".into()],
            add(&g, var(&g, "x"), int_lit(&g, 1)),
        );
        i.env.define("opt", Value::Optional(None));

        let n = method_call(&g, var(&g, "opt"), "map", vec![var(&g, "inc")]);
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Optional(None)));
    }

    // ── Result higher-order methods ───────────────────────────────────────

    #[tokio::test]
    async fn result_map_ok() {
        // Ok(5).map((x) => x * 2) == Ok(10)
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "double",
            vec!["x".into()],
            mul(&g, var(&g, "x"), int_lit(&g, 2)),
        );
        i.env
            .define("res", Value::Result(Ok(Box::new(Value::Int(5)))));

        let n = method_call(&g, var(&g, "res"), "map", vec![var(&g, "double")]);
        assert_eq!(
            i.eval_expr(&n).await,
            Ok(Value::Result(Ok(Box::new(Value::Int(10)))))
        );
    }

    #[tokio::test]
    async fn result_map_err_passthrough() {
        // Err("x").map((v) => v * 2) == Err("x")
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "double",
            vec!["x".into()],
            mul(&g, var(&g, "x"), int_lit(&g, 2)),
        );
        i.env.define(
            "res",
            Value::Result(Err(Box::new(Value::String(bock_interp::BockString::new(
                "oops",
            ))))),
        );

        let n = method_call(&g, var(&g, "res"), "map", vec![var(&g, "double")]);
        assert_eq!(
            i.eval_expr(&n).await,
            Ok(Value::Result(Err(Box::new(Value::String(
                bock_interp::BockString::new("oops")
            )))))
        );
    }

    #[tokio::test]
    async fn result_map_err_transforms() {
        // Err(1).map_err((e) => e * 10) == Err(10)
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "mul10",
            vec!["e".into()],
            mul(&g, var(&g, "e"), int_lit(&g, 10)),
        );
        i.env
            .define("res", Value::Result(Err(Box::new(Value::Int(1)))));

        let n = method_call(&g, var(&g, "res"), "map_err", vec![var(&g, "mul10")]);
        assert_eq!(
            i.eval_expr(&n).await,
            Ok(Value::Result(Err(Box::new(Value::Int(10)))))
        );
    }

    #[tokio::test]
    async fn result_map_err_ok_passthrough() {
        // Ok(5).map_err((e) => e) == Ok(5)
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn("id", vec!["e".into()], var(&g, "e"));
        i.env
            .define("res", Value::Result(Ok(Box::new(Value::Int(5)))));

        let n = method_call(&g, var(&g, "res"), "map_err", vec![var(&g, "id")]);
        assert_eq!(
            i.eval_expr(&n).await,
            Ok(Value::Result(Ok(Box::new(Value::Int(5)))))
        );
    }

    // ── Chained operations ────────────────────────────────────────────────

    #[tokio::test]
    async fn filter_then_map_chained() {
        // [1,2,3,4].filter((x) => x > 2).map((x) => x * 10) == [30, 40]
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "gt2",
            vec!["x".into()],
            gt(&g, var(&g, "x"), int_lit(&g, 2)),
        );
        i.register_fn(
            "mul10",
            vec!["x".into()],
            mul(&g, var(&g, "x"), int_lit(&g, 10)),
        );
        i.env.define(
            "lst",
            Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Int(4),
            ]),
        );

        let chained = method_call(
            &g,
            method_call(&g, var(&g, "lst"), "filter", vec![var(&g, "gt2")]),
            "map",
            vec![var(&g, "mul10")],
        );
        assert_eq!(
            i.eval_expr(&chained).await,
            Ok(Value::List(vec![Value::Int(30), Value::Int(40)]))
        );
    }

    // ── Map higher-order methods ───────────────────────────────────────────

    #[tokio::test]
    async fn map_map_values_doubles() {
        // {1: 10, 2: 20}.map_values((v) => v * 2) == {1: 20, 2: 40}
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "double",
            vec!["x".into()],
            mul(&g, var(&g, "x"), int_lit(&g, 2)),
        );
        let mut m = std::collections::BTreeMap::new();
        m.insert(Value::Int(1), Value::Int(10));
        m.insert(Value::Int(2), Value::Int(20));
        i.env.define("m", Value::Map(m));

        let n = method_call(&g, var(&g, "m"), "map_values", vec![var(&g, "double")]);
        let result = i.eval_expr(&n).await.unwrap();
        let mut expected = std::collections::BTreeMap::new();
        expected.insert(Value::Int(1), Value::Int(20));
        expected.insert(Value::Int(2), Value::Int(40));
        assert_eq!(result, Value::Map(expected));
    }

    #[tokio::test]
    async fn map_filter_by_value() {
        // {1: 10, 2: 5, 3: 20}.filter((k, v) => v > 8) == {1: 10, 3: 20}
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "val_gt8",
            vec!["k".into(), "v".into()],
            gt(&g, var(&g, "v"), int_lit(&g, 8)),
        );
        let mut m = std::collections::BTreeMap::new();
        m.insert(Value::Int(1), Value::Int(10));
        m.insert(Value::Int(2), Value::Int(5));
        m.insert(Value::Int(3), Value::Int(20));
        i.env.define("m", Value::Map(m));

        let n = method_call(&g, var(&g, "m"), "filter", vec![var(&g, "val_gt8")]);
        let result = i.eval_expr(&n).await.unwrap();
        let mut expected = std::collections::BTreeMap::new();
        expected.insert(Value::Int(1), Value::Int(10));
        expected.insert(Value::Int(3), Value::Int(20));
        assert_eq!(result, Value::Map(expected));
    }

    #[tokio::test]
    async fn map_for_each_runs() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn("noop", vec!["k".into(), "v".into()], var(&g, "v"));
        let mut m = std::collections::BTreeMap::new();
        m.insert(Value::Int(1), Value::Int(10));
        i.env.define("m", Value::Map(m));

        let n = method_call(&g, var(&g, "m"), "for_each", vec![var(&g, "noop")]);
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Void));
    }

    // ── Set higher-order methods ────────────────────────────────────────────

    #[tokio::test]
    async fn set_filter_gt2() {
        // {1, 2, 3, 4}.filter((x) => x > 2) == {3, 4}
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "gt2",
            vec!["x".into()],
            gt(&g, var(&g, "x"), int_lit(&g, 2)),
        );
        let s: std::collections::BTreeSet<Value> =
            [1, 2, 3, 4].iter().map(|&v| Value::Int(v)).collect();
        i.env.define("s", Value::Set(s));

        let n = method_call(&g, var(&g, "s"), "filter", vec![var(&g, "gt2")]);
        let result = i.eval_expr(&n).await.unwrap();
        let expected: std::collections::BTreeSet<Value> =
            [3, 4].iter().map(|&v| Value::Int(v)).collect();
        assert_eq!(result, Value::Set(expected));
    }

    #[tokio::test]
    async fn set_for_each_runs() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn("id", vec!["x".into()], var(&g, "x"));
        let s: std::collections::BTreeSet<Value> =
            [1, 2, 3].iter().map(|&v| Value::Int(v)).collect();
        i.env.define("s", Value::Set(s));

        let n = method_call(&g, var(&g, "s"), "for_each", vec![var(&g, "id")]);
        assert_eq!(i.eval_expr(&n).await, Ok(Value::Void));
    }

    #[tokio::test]
    async fn set_map_doubles() {
        // {1, 2, 3}.map((x) => x * 2) == {2, 4, 6}
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "double",
            vec!["x".into()],
            mul(&g, var(&g, "x"), int_lit(&g, 2)),
        );
        let s: std::collections::BTreeSet<Value> =
            [1, 2, 3].iter().map(|&v| Value::Int(v)).collect();
        i.env.define("s", Value::Set(s));

        let n = method_call(&g, var(&g, "s"), "map", vec![var(&g, "double")]);
        let result = i.eval_expr(&n).await.unwrap();
        let expected: std::collections::BTreeSet<Value> =
            [2, 4, 6].iter().map(|&v| Value::Int(v)).collect();
        assert_eq!(result, Value::Set(expected));
    }

    // ── for..in over lazy iterators ───────────────────────────────────────

    #[tokio::test]
    async fn for_in_over_eager_map_result() {
        // for item in [1,2,3].map(double) { sum += item }
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "double",
            vec!["x".into()],
            mul(&g, var(&g, "x"), int_lit(&g, 2)),
        );
        i.env.define("sum", Value::Int(0));

        let iterable = method_call(
            &g,
            list_lit(&g, vec![int_lit(&g, 1), int_lit(&g, 2), int_lit(&g, 3)]),
            "map",
            vec![var(&g, "double")],
        );
        let for_node = mk_node(
            &g,
            NodeKind::For {
                pattern: Box::new(bind_pat(&g, "item")),
                iterable: Box::new(iterable),
                body: Box::new(assign(&g, "sum", add(&g, var(&g, "sum"), var(&g, "item")))),
            },
        );

        assert_eq!(i.eval_expr(&for_node).await, Ok(Value::Void));
        assert_eq!(i.env.get("sum"), Some(&Value::Int(12)));
    }

    #[tokio::test]
    async fn for_in_over_lazy_map_iterator() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "double",
            vec!["x".into()],
            mul(&g, var(&g, "x"), int_lit(&g, 2)),
        );

        let fn_val = match i.env.get("double").unwrap().clone() {
            Value::Function(fv) => fv,
            _ => panic!("expected function"),
        };

        let source = IteratorKind::List {
            items: vec![Value::Int(1), Value::Int(2), Value::Int(3)],
            pos: 0,
        };
        let map_iter = IteratorKind::Map {
            source: Arc::new(Mutex::new(source)),
            func: fn_val,
        };
        i.env
            .define("it", Value::Iterator(IteratorValue::new(map_iter)));
        i.env.define("sum", Value::Int(0));

        let for_node = mk_node(
            &g,
            NodeKind::For {
                pattern: Box::new(bind_pat(&g, "item")),
                iterable: Box::new(var(&g, "it")),
                body: Box::new(assign(&g, "sum", add(&g, var(&g, "sum"), var(&g, "item")))),
            },
        );

        assert_eq!(i.eval_expr(&for_node).await, Ok(Value::Void));
        assert_eq!(i.env.get("sum"), Some(&Value::Int(12)));
    }

    #[tokio::test]
    async fn for_in_over_lazy_filter_iterator() {
        let mut i = interp();
        let g = NodeIdGen::new();
        i.register_fn(
            "gt2",
            vec!["x".into()],
            gt(&g, var(&g, "x"), int_lit(&g, 2)),
        );

        let fn_val = match i.env.get("gt2").unwrap().clone() {
            Value::Function(fv) => fv,
            _ => panic!("expected function"),
        };

        let source = IteratorKind::List {
            items: vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)],
            pos: 0,
        };
        let filter_iter = IteratorKind::Filter {
            source: Arc::new(Mutex::new(source)),
            pred: fn_val,
        };
        i.env
            .define("it", Value::Iterator(IteratorValue::new(filter_iter)));
        i.env.define("sum", Value::Int(0));

        let for_node = mk_node(
            &g,
            NodeKind::For {
                pattern: Box::new(bind_pat(&g, "item")),
                iterable: Box::new(var(&g, "it")),
                body: Box::new(assign(&g, "sum", add(&g, var(&g, "sum"), var(&g, "item")))),
            },
        );

        assert_eq!(i.eval_expr(&for_node).await, Ok(Value::Void));
        // 3 + 4 = 7
        assert_eq!(i.env.get("sum"), Some(&Value::Int(7)));
    }
}
