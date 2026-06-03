package main

// ── Bock Optional runtime ──
type __bockOption struct {
	tag string
	v   interface{}
}

func __bockSome(v interface{}) __bockOption { return __bockOption{tag: "Some", v: v} }

var __bockNone = __bockOption{tag: "None"}

// ── Bock numeric payload helpers ──
func __bockAsInt64(v interface{}) int64 {
	switch n := v.(type) {
	case int64:
		return n
	case int:
		return int64(n)
	case int32:
		return int64(n)
	case float64:
		return int64(n)
	default:
		return 0
	}
}

func __bockAsFloat64(v interface{}) float64 {
	switch n := v.(type) {
	case float64:
		return n
	case float32:
		return float64(n)
	case int64:
		return float64(n)
	case int:
		return float64(n)
	default:
		return 0
	}
}
