package main

import "fmt"

func primitives() {
	var i int64 = 42
	var f float64 = 3.14
	var b bool = true
	var s string = "hello"
	var c Char = 'A'
	sum := (i + 8)
	product := (f * 2.0)
	power := (2 /* pow */ 10)
	check := ((i > 0) && b)
	either := ((i == 42) || (f < 1.0))
	fmt.Println(fmt.Sprintf("int=%v float=%v bool=%v char=%v", i, f, b, c))
	fmt.Println(fmt.Sprintf("sum=%v product=%v power=%v", sum, product, power))
}

func Identity[T any](x T) T {
	return x
}

func FirstOf[A any, B any](a A, b B) A {
	return a
}

type Pair[A any, B any] struct {
	First	A
	Second	B
}

func (p *Pair) Swap(self interface{}) Pair[B, A] {
	return Pair{First: self.Second, Second: self.First}
}

type Box[T any] struct {
	Value	T
}

func (b *Box) Map(self interface{}, f func(T) U) Box[U] {
	return Box{Value: f(self.Value)}
}

func MaxOf[T Comparable](a T, b T) T {
	return func() interface{} { if (a > b) { return a } else { return b } }()
}

type Describable interface {
	Describe(interface{}) string
}

type Color struct {
	R	int64
	G	int64
	B	int64
}

func (c Color) Describe(self interface{}) string {
	return fmt.Sprintf("rgb(%v, %v, %v)", self.R, self.G, self.B)
}

func Apply(f func(int64) int64, x int64) int64 {
	return f(x)
}

func ApplyTwice(f func(int64) int64, x int64) int64 {
	return f(f(x))
}

func ComposeInt(f func(int64) int64, g func(int64) int64) func(int64) int64 {
	return func(x interface{}) interface{} { return f(g(x)) }
}

type UserId = interface{}

type Predicate = interface{}

type StringPair = interface{}

func FindUser(id UserId) Optional[UserId] {
	return func() interface{} { if (id == "") { return None } else { return Some(id) } }()
}

func CountMatching(items []interface{}[int64], pred Predicate) int64 {
	return items.Filter(items, pred).Len(items.Filter(items, pred))
}

func DescribeOptional(opt Optional[int64]) string {
	return func() interface{} { switch opt { case Some: return fmt.Sprintf("positive: %v", n) case Some: return fmt.Sprintf("non-positive: %v", n) case None: return "absent" } return nil }()
}

func SafeDivide(a float64, b float64) Result[float64, string] {
	return func() interface{} { if (b == 0.0) { return Err("division by zero") } else { return Ok((a / b)) } }()
}

func ChainedDivide(x float64) Result[float64, string] {
	half := SafeDivide(x, 2.0)
	quarter := SafeDivide(half, 2.0)
	return Ok(quarter)
}

func Stats(items []interface{}[int64]) struct{ Field0 int64; Field1 int64 } {
	count := items.Len(items)
	total := items.Len(items)
	return [...]interface{}{count, total}
}

func CollectionsDemo() {
	list := []interface{}{10, 20, 30, 40, 50}
	map := map[interface{}]interface{}{"name": "Bock", "version": "0.1"}
	set := map[interface{}]struct{}{"alpha": {}, "beta": {}, "gamma": {}}
	n := list.Len(list)
	keys := map.Keys(map)
	has := set.Len(set)
	fmt.Println(fmt.Sprintf("list len=%v map keys=%v set size=%v", n, keys, has))
	extended := (list + []interface{}{60, 70})
	fmt.Println(fmt.Sprintf("extended len=%v", extended.Len(extended)))
}

func ChainDemo() {
	numbers := []interface{}{1, 2, 3, 4, 5, 6, 7, 8, 9, 10}
	result := numbers.Filter(numbers, func(n interface{}) interface{} { return ((n % 2) == 0) }).Map(numbers.Filter(numbers, func(n interface{}) interface{} { return ((n % 2) == 0) }), func(n interface{}) interface{} { return (n * 2) })
	fmt.Println(fmt.Sprintf("chained result len=%v", result.Len(result)))
}

func Double(x int64) int64 {
	return (x * 2)
}

func Increment(x int64) int64 {
	return (x + 1)
}

func PipeDemo() {
	piped := Double(5)
	transform := func(composeX interface{}) interface{} { return Increment(Double(composeX)) }
	fmt.Println(fmt.Sprintf("piped=%v", piped))
}

func main() {
	fmt.Println("=== Type Zoo ===")
	primitives()
	n := Identity(42)
	s := Identity("hello")
	f := FirstOf(1, "two")
	fmt.Println(fmt.Sprintf("identity(42)=%v identity(hello)=%v first_of=%v", n, s, f))
	pair := Pair{First: 1, Second: "one"}
	fmt.Println(fmt.Sprintf("pair: %v, %v", pair.First, pair.Second))
	swapped := pair.Swap(pair)
	fmt.Println(fmt.Sprintf("swapped: %v, %v", swapped.First, swapped.Second))
	bigger := MaxOf(10, 20)
	fmt.Println(fmt.Sprintf("max_of(10,20)=%v", bigger))
	color := Color{R: 255, G: 128, B: 0}
	fmt.Println(fmt.Sprintf("color: %v", color.Describe(color)))
	doubled := Apply(func(x interface{}) interface{} { return (x * 2) }, 21)
	quad := ApplyTwice(func(x interface{}) interface{} { return (x * 2) }, 3)
	fmt.Println(fmt.Sprintf("apply doubled=%v apply_twice=%v", doubled, quad))
	user := FindUser("alice")
	evens := CountMatching([]interface{}{1, 2, 3, 4, 5}, func(x interface{}) interface{} { return ((x % 2) == 0) })
	fmt.Println(fmt.Sprintf("find_user=some evens=%v", evens))
	fmt.Println(DescribeOptional(Some(42)))
	fmt.Println(DescribeOptional(None))
	divResult := ChainedDivide(100.0)
	switch __v := divResult; __v.(type) {
		case Ok:
			fmt.Println(fmt.Sprintf("chained_divide(100)=%v", v))
			case Err:
				fmt.Println(fmt.Sprintf("error: %v", e))
			}
			count := Stats([]interface{}{1, 2, 3})
			fmt.Println(fmt.Sprintf("stats: count=%v total=%v", count, total))
			CollectionsDemo()
			ChainDemo()
			PipeDemo()
		}
