package main

import "fmt"

type Resource struct {
	Name	string
	Data	[]interface{}[int64]
}

func (r *Resource) describe(self interface{}) string {
	return fmt.Sprintf("Resource(%v, len=%v)", self.Name, self.Data.Len(self.Data))
}

type ParseResult interface {
	isParseResult()
}

type ParseResultParsed struct {
	Field0	string
}

func (ParseResultParsed) isParseResult() {}

type ParseResultFailed struct {
	Field0	string
}

func (ParseResultFailed) isParseResult() {}

func moveBasics() {
	fmt.Println("--- Move Semantics ---")
	a := []interface{}{1, 2, 3}
	b := a
	fmt.Println(fmt.Sprintf("b has %v items", b.Len(b)))
	x := Resource{Name: "config", Data: []interface{}{10, 20}}
	y := x
	z := y
	fmt.Println(fmt.Sprintf("z = %v", z.Describe(z)))
}

func countItems(items []interface{}[int64]) int64 {
	return items.Len(items)
}

func describeResource(r Resource) string {
	return r.Describe(r)
}

func implicitBorrowDemo() {
	fmt.Println("--- Implicit Borrow ---")
	data := []interface{}{10, 20, 30, 40, 50}
	n := countItems(data)
	fmt.Println(fmt.Sprintf("count = %v", n))
	n2 := countItems(data)
	fmt.Println(fmt.Sprintf("count again = %v", n2))
	n3 := data.Len(data)
	fmt.Println(fmt.Sprintf("and once more = %v", n3))
	res := Resource{Name: "db", Data: []interface{}{1, 2, 3}}
	desc := describeResource(res)
	fmt.Println(fmt.Sprintf("desc = %v", desc))
	desc2 := describeResource(res)
	fmt.Println(fmt.Sprintf("desc again = %v", desc2))
}

func appendItem(items []interface{}[int64], value int64) []interface{}[int64] {
	items = (items + []interface{}{value})
	return items
}

func doubleAll(items []interface{}[int64]) []interface{}[int64] {
	items = items.Map(items, func(x interface{}) interface{} { return (x * 2) })
	return items
}

func mutableBorrowDemo() {
	fmt.Println("--- Mutable Borrow ---")
	nums := []interface{}{1, 2, 3}
	result := appendItem(nums, 4)
	fmt.Println(fmt.Sprintf("appended: len=%v", result.Len(result)))
	vals := []interface{}{10, 20, 30}
	doubled := doubleAll(vals)
	fmt.Println(fmt.Sprintf("doubled: len=%v", doubled.Len(doubled)))
}

func buildReport() string {
	title := "Quarterly Report"
	header := fmt.Sprintf("=== %v ===", title)
	section1 := fmt.Sprintf("Section 1 of %v", title)
	section2 := fmt.Sprintf("Section 2 of %v", title)
	footer := fmt.Sprintf("End of %v", title)
	return fmt.Sprintf("%v | %v | %v | %v", header, section1, section2, footer)
}

func buildUiTree() string {
	appName := "Bock App"
	theme := "dark"
	header := fmt.Sprintf("Header: %v (%v)", appName, theme)
	sidebar := fmt.Sprintf("Sidebar: %v nav", appName)
	content := fmt.Sprintf("Content: %v main", appName)
	footer := fmt.Sprintf("Footer: %v v1.0", appName)
	return fmt.Sprintf("%v | %v | %v | %v", header, sidebar, content, footer)
}

func managedDemo() {
	fmt.Println("--- @managed Escape Hatch ---")
	report := buildReport()
	fmt.Println(fmt.Sprintf("report: %v", report))
	ui := buildUiTree()
	fmt.Println(fmt.Sprintf("ui: %v", ui))
}

func validate(input string) Result[string, string] {
	return func() interface{} { if (input == "") { return Err("empty input") } else { return Ok(input) } }()
}

func guardOwnershipDemo() {
	fmt.Println("--- Guard + Ownership ---")
	if !(validate("hello")) {
		/* unsupported */
	}
	fmt.Println(fmt.Sprintf("validated: %v", val))
	if !(validate("world")) {
		/* unsupported */
	}
	fmt.Println(fmt.Sprintf("also validated: %v", val2))
}

func classify(n int64) string {
	label := func() interface{} { switch n { case 0: return "zero" case 1: return "one" default: return /* unsupported */ } return nil }()
	return fmt.Sprintf("classified as: %v", label)
}

func findFirstPositive(items []interface{}[int64]) string {
	result := func() interface{} { switch items { case interface{}: return /* unsupported */ case interface{}: return first } return nil }()
	return fmt.Sprintf("first element: %v", result)
}

func neverDemo() {
	fmt.Println("--- Match with Never ---")
	fmt.Println(classify(0))
	fmt.Println(classify(1))
	fmt.Println(classify(42))
	fmt.Println(findFirstPositive([]interface{}{}))
	fmt.Println(findFirstPositive([]interface{}{7, 8, 9}))
}

func wouldFailExamples() {
	fmt.Println("--- What Would Fail (commented-out examples) ---")
	fmt.Println("(See source comments for ownership error examples)")
}

func main() {
	fmt.Println("=== Ownership Demo ===")
	fmt.Println("")
	moveBasics()
	fmt.Println("")
	implicitBorrowDemo()
	fmt.Println("")
	mutableBorrowDemo()
	fmt.Println("")
	managedDemo()
	fmt.Println("")
	guardOwnershipDemo()
	fmt.Println("")
	neverDemo()
	fmt.Println("")
	wouldFailExamples()
	fmt.Println("")
	fmt.Println("=== Ownership Demo Complete ===")
}
